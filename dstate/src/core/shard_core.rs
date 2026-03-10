use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use crate::core::view_map::ViewMap;
use crate::traits::clock::Clock;
use crate::types::envelope::{StateObject, StateViewObject};
use crate::types::node::{NodeId, Generation};

/// Parameters for an inbound snapshot from a peer.
#[allow(dead_code)]
pub(crate) struct InboundSnapshot<V> {
    pub source: NodeId,
    pub generation: Generation,
    pub wire_version: u32,
    pub view: V,
    pub created_time: i64,
    pub modified_time: i64,
}

#[allow(dead_code)] // Used by tests and future actor shell
/// Outcome of a successful simple mutation (State == View).
#[derive(Debug)]
pub(crate) struct MutationOutcome<V: Clone> {
    pub generation: Generation,
    pub view: V,
}

#[allow(dead_code)] // Used by tests and future actor shell
/// Outcome of a successful delta-aware mutation.
#[derive(Debug)]
pub(crate) struct DeltaMutationOutcome<V: Clone, VD: Clone> {
    pub generation: Generation,
    pub view: V,
    pub view_delta: VD,
}

#[allow(dead_code)] // Used by tests and future actor shell
/// Result of attempting to accept an inbound snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcceptResult {
    /// The snapshot was accepted and the view map was updated.
    Accepted,
    /// The snapshot was discarded because it is stale or duplicate.
    Discarded,
}

#[allow(dead_code)] // Used by tests and future actor shell
/// Result of attempting to accept an inbound delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeltaAcceptResult {
    /// The delta was applied to the peer's view.
    Applied,
    /// The delta was discarded (stale or duplicate).
    Discarded,
    /// There is a gap — the delta's generation doesn't follow the local view.
    /// Caller should request a full snapshot.
    GapDetected { local: Generation, incoming: Generation },
    /// The peer has no entry in the view map. Caller should request snapshot.
    UnknownPeer,
}

/// Pure state machine managing one distributed state shard on a single node.
///
/// `ShardCore` holds the owned `StateObject<S>` and the cluster-wide
/// view map (a per-node `ArcSwap` structure). It implements all ordering,
/// mutation, and view-map logic without depending on any actor framework.
///
/// # View Map Concurrency
///
/// The view map uses a two-level `ArcSwap` design:
/// - Outer: `ArcSwap<HashMap<NodeId, Arc<ArcSwap<StateViewObject<V>>>>>`
///   — swapped only on node join/leave (rare).
/// - Inner (per-node): `ArcSwap<StateViewObject<V>>` — swapped on every
///   snapshot/delta update (frequent), O(1) with no map cloning.
///
/// Read path is fully lock-free (two atomic pointer loads).
///
/// - For simple states (`DistributedState`): `S == V`.
/// - For delta states (`DeltaDistributedState`): `V = D::View`.
#[allow(dead_code)] // Used by tests and future actor shell
pub(crate) struct ShardCore<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    node_id: NodeId,
    state: StateObject<S>,
    views: ViewMap<V>,
    clock: Arc<dyn Clock>,
}

impl<S, V> ShardCore<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    /// Create a new `ShardCore`.
    ///
    /// The `initial_view` is the projection of the initial state for the
    /// local node's entry in the view map. For simple states, this is
    /// `state.value.clone()`; for delta states, use `project_view`.
    pub fn new(
        node_id: NodeId,
        state: StateObject<S>,
        initial_view: V,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let now = clock.now();
        let initial_vo = StateViewObject {
            generation: state.generation,
            wire_version: state.storage_version,
            value: initial_view,
            created_time: state.created_time,
            modified_time: state.modified_time,
            synced_at: now,
            pending_remote_generation: None,
            source_node: node_id,
        };
        let views = ViewMap::new(node_id, initial_vo);

        Self {
            node_id,
            state,
            views,
            clock,
        }
    }

    // ── Accessors ───────────────────────────────────────────────

    /// The local node's ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Read-only access to the owned state.
    pub fn state(&self) -> &StateObject<S> {
        &self.state
    }

    /// Mutable access to the owned state (for persistence restore).
    pub fn state_mut(&mut self) -> &mut StateObject<S> {
        &mut self.state
    }

    /// Read a single node's view. Lock-free O(1).
    pub fn get_view(&self, node_id: &NodeId) -> Option<Arc<StateViewObject<V>>> {
        self.views.get(node_id)
    }

    /// Collect a full snapshot of all views. Lock-free but O(n).
    pub fn snapshot(&self) -> HashMap<NodeId, StateViewObject<V>> {
        self.views.snapshot()
    }

    /// Number of nodes in the view map.
    pub fn view_count(&self) -> usize {
        self.views.len()
    }

    // ── Ordering ────────────────────────────────────────────────

    /// Determine whether an inbound update should be accepted.
    ///
    /// Uses `Generation` comparison (lexicographic `(incarnation, age)`):
    /// accept iff `incoming > local`.
    pub fn should_accept(incoming: Generation, local: Generation) -> bool {
        incoming > local
    }

    // ── Mutation (simple: State == View) ────────────────────────

    /// Apply a mutation closure to the owned state.
    ///
    /// The `project_fn` converts the updated state into a view for the
    /// view map entry. For simple states, pass `|s| s.clone()`.
    ///
    /// Returns `MutationOutcome` on success.
    pub fn apply_mutation<F, P>(
        &mut self,
        mutate_fn: F,
        project_fn: P,
    ) -> MutationOutcome<V>
    where
        F: FnOnce(&mut S),
        P: FnOnce(&S) -> V,
    {
        mutate_fn(&mut self.state.value);

        self.state.generation.age += 1;
        self.state.modified_time = self.clock.unix_ms();

        let view = project_fn(&self.state.value);
        self.update_own_view(view.clone());

        MutationOutcome {
            generation: self.state.generation,
            view,
        }
    }

    // ── Mutation (delta-aware) ──────────────────────────────────

    /// Apply a delta-aware mutation to the owned state.
    ///
    /// - `mutate_fn` applies the mutation and returns a `StateDeltaChange`.
    /// - `project_view_fn` projects the full state into the public view.
    /// - `project_delta_fn` projects the state-level change into a view-level delta.
    ///
    /// Returns `DeltaMutationOutcome` on success.
    pub fn apply_mutation_with_delta<F, PV, PD, DC, VD>(
        &mut self,
        mutate_fn: F,
        project_view_fn: PV,
        project_delta_fn: PD,
    ) -> DeltaMutationOutcome<V, VD>
    where
        F: FnOnce(&mut S) -> DC,
        PV: FnOnce(&S) -> V,
        PD: FnOnce(&DC) -> VD,
        VD: Clone + Debug,
    {
        let delta_change = mutate_fn(&mut self.state.value);

        self.state.generation.age += 1;
        self.state.modified_time = self.clock.unix_ms();

        let view = project_view_fn(&self.state.value);
        let view_delta = project_delta_fn(&delta_change);
        self.update_own_view(view.clone());

        DeltaMutationOutcome {
            generation: self.state.generation,
            view,
            view_delta,
        }
    }

    // ── Inbound snapshot ────────────────────────────────────────

    /// Accept or discard an inbound full snapshot from a peer.
    ///
    /// Uses `should_accept` to compare against the peer's existing entry
    /// (if any). A peer with no existing entry is always accepted.
    pub fn accept_inbound_snapshot(&self, snap: InboundSnapshot<V>) -> AcceptResult {
        let now = self.clock.now();
        let source = snap.source;

        let accepted = self.views.update_if(&source, |existing| {
            let accept = match existing {
                Some(e) => Self::should_accept(snap.generation, e.generation),
                None => true,
            };
            if !accept {
                return None;
            }
            Some(StateViewObject {
                generation: snap.generation,
                wire_version: snap.wire_version,
                value: snap.view,
                created_time: snap.created_time,
                modified_time: snap.modified_time,
                synced_at: now,
                pending_remote_generation: None,
                source_node: source,
            })
        });

        if accepted {
            AcceptResult::Accepted
        } else {
            AcceptResult::Discarded
        }
    }

    // ── Inbound delta ───────────────────────────────────────────

    /// Accept or discard an inbound delta update from a peer.
    ///
    /// The `generation` is the generation *after* applying the delta
    /// (i.e., the delta advances the view from `generation.age - 1` to
    /// `generation.age`).
    ///
    /// - If the peer has no entry, returns `UnknownPeer`.
    /// - If the generation doesn't follow the peer's current view, returns `GapDetected`.
    /// - If stale (older incarnation or age), returns `Discarded`.
    /// - Otherwise, applies `apply_fn` and updates the view map.
    pub fn accept_inbound_delta<AF>(
        &self,
        source: NodeId,
        generation: Generation,
        wire_version: u32,
        apply_fn: AF,
    ) -> DeltaAcceptResult
    where
        AF: FnOnce(&V) -> V,
    {
        let existing = match self.views.get(&source) {
            Some(e) => e,
            None => return DeltaAcceptResult::UnknownPeer,
        };

        // Stale incarnation — discard.
        if generation.incarnation < existing.generation.incarnation {
            return DeltaAcceptResult::Discarded;
        }

        // New incarnation — gap (need full snapshot).
        if generation.incarnation > existing.generation.incarnation {
            return DeltaAcceptResult::GapDetected {
                local: existing.generation,
                incoming: generation,
            };
        }

        // Same incarnation: delta must advance age by exactly 1.
        if generation.age != existing.generation.age + 1 {
            if generation.age <= existing.generation.age {
                return DeltaAcceptResult::Discarded;
            }
            return DeltaAcceptResult::GapDetected {
                local: existing.generation,
                incoming: generation,
            };
        }

        let new_view = apply_fn(&existing.value);
        let now = self.clock.now();

        self.views.update(
            &source,
            StateViewObject {
                generation,
                wire_version,
                value: new_view,
                created_time: existing.created_time,
                modified_time: self.clock.unix_ms(),
                synced_at: now,
                pending_remote_generation: None,
                source_node: source,
            },
        );

        DeltaAcceptResult::Applied
    }

    // ── Staleness ───────────────────────────────────────────────

    /// Return peers whose views are stale (not synced within `max_staleness`
    /// or have a `pending_remote_generation`).
    pub fn stale_peers(&self, max_staleness: Duration) -> Vec<NodeId> {
        self.views.stale_peers(self.node_id, max_staleness, self.clock.as_ref())
    }

    /// Mark a peer's view as stale based on a change-feed notification.
    ///
    /// Sets `pending_remote_generation` if the incoming version is newer
    /// than the current entry.
    pub fn mark_stale(&self, source: NodeId, incarnation: u64, age: u64) {
        let incoming = Generation::new(incarnation, age);
        self.views.update_if(&source, |existing| {
            let existing = existing?;

            if incoming <= existing.generation {
                return None;
            }

            let mut updated = existing.clone();
            updated.pending_remote_generation = Some(incoming);
            Some(updated)
        });
    }

    // ── Cluster events ──────────────────────────────────────────

    /// Handle a node joining the cluster.
    ///
    /// Adds an empty placeholder entry for the new node if it doesn't
    /// already exist. The `default_view` is the initial view value.
    pub fn on_node_joined(&self, node_id: NodeId, default_view: V) {
        if node_id == self.node_id {
            return; // don't re-add ourselves
        }

        let now = self.clock.now();
        self.views.insert_node(
            node_id,
            StateViewObject {
                generation: Generation::zero(),
                wire_version: 0,
                value: default_view,
                created_time: 0,
                modified_time: 0,
                synced_at: now,
                pending_remote_generation: None,
                source_node: node_id,
            },
        );
    }

    /// Handle a node leaving the cluster.
    ///
    /// Removes the departed node's entry from the view map.
    pub fn on_node_left(&self, node_id: NodeId) {
        if node_id == self.node_id {
            return; // don't remove ourselves
        }
        self.views.remove_node(&node_id);
    }

    // ── Helpers ─────────────────────────────────────────────────

    /// Update the local node's own entry in the view map. O(1) — just
    /// an atomic swap on the per-node ArcSwap, no map cloning.
    fn update_own_view(&self, view: V) {
        let now = self.clock.now();
        self.views.update(
            &self.node_id,
            StateViewObject {
                generation: self.state.generation,
                wire_version: self.state.storage_version,
                value: view,
                created_time: self.state.created_time,
                modified_time: self.state.modified_time,
                synced_at: now,
                pending_remote_generation: None,
                source_node: self.node_id,
            },
        );
    }
}

#[allow(dead_code)] // Used by tests and future actor shell
/// Helper to create a fresh `StateObject` for a state with a given
/// incarnation. Used at node startup.
pub(crate) fn new_state_object<S>(
    value: S,
    incarnation: u64,
    clock: &dyn Clock,
) -> StateObject<S> {
    let now = clock.unix_ms();
    StateObject {
        generation: Generation::new(incarnation, 0),
        storage_version: 0,
        value,
        created_time: now,
        modified_time: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_clock::TestClock;
    use crate::test_support::test_state::{
        TestDeltaChange, TestDeltaState, TestDeltaStateInner, TestDeltaView,
        TestDeltaViewDelta, TestState,
    };
    use crate::traits::state::DeltaDistributedState;
    use crate::types::node::Generation;
    use std::time::Duration;

    fn test_clock() -> Arc<dyn Clock> {
        Arc::new(TestClock::with_base_unix_ms(1_000_000))
    }

    fn make_state_object(counter: u64, label: &str, clock: &dyn Clock) -> StateObject<TestState> {
        StateObject {
            generation: Generation::new(1, 0),
            storage_version: 1,
            value: TestState {
                counter,
                label: label.to_string(),
            },
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        }
    }

    fn make_shard(clock: Arc<dyn Clock>) -> ShardCore<TestState, TestState> {
        let state = make_state_object(0, "init", clock.as_ref());
        let view = state.value.clone();
        ShardCore::new(NodeId(1), state, view, clock)
    }

    fn make_delta_shard(
        clock: Arc<dyn Clock>,
    ) -> ShardCore<TestDeltaStateInner, TestDeltaView> {
        let state = StateObject {
            generation: Generation::new(1, 0),
            storage_version: 1,
            value: TestDeltaStateInner::default(),
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        };
        let view = TestDeltaState::project_view(&state.value);
        ShardCore::new(NodeId(1), state, view, clock)
    }

    // ── TEST-05: should_accept ordering ─────────────────────────

    #[test]
    fn test_05_should_accept_newer_incarnation() {
        assert!(ShardCore::<TestState, TestState>::should_accept(
            Generation::new(2, 0),
            Generation::new(1, 100),
        ));
    }

    #[test]
    fn test_05_should_accept_same_inc_newer_age() {
        assert!(ShardCore::<TestState, TestState>::should_accept(
            Generation::new(1, 5),
            Generation::new(1, 4),
        ));
    }

    #[test]
    fn test_05_should_reject_same_inc_same_age() {
        assert!(!ShardCore::<TestState, TestState>::should_accept(
            Generation::new(1, 5),
            Generation::new(1, 5),
        ));
    }

    #[test]
    fn test_05_should_reject_same_inc_older_age() {
        assert!(!ShardCore::<TestState, TestState>::should_accept(
            Generation::new(1, 3),
            Generation::new(1, 5),
        ));
    }

    #[test]
    fn test_05_should_reject_older_incarnation() {
        assert!(!ShardCore::<TestState, TestState>::should_accept(
            Generation::new(1, 100),
            Generation::new(2, 0),
        ));
    }

    // ── TEST-06: stale_peers ────────────────────────────────────

    #[test]
    fn test_06_stale_peers_by_time() {
        let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
        let shard = make_shard(clock.clone());

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        // No stale peers immediately.
        assert!(shard.stale_peers(Duration::from_secs(10)).is_empty());

        // Advance clock past staleness threshold.
        clock.advance(Duration::from_secs(15));
        let stale = shard.stale_peers(Duration::from_secs(10));
        assert_eq!(stale, vec![NodeId(2)]);
    }

    #[test]
    fn test_06_stale_peers_by_pending_age() {
        let clock = test_clock();
        let shard = make_shard(clock.clone());

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        shard.mark_stale(NodeId(2), 1, 5);

        let stale = shard.stale_peers(Duration::from_secs(3600));
        assert_eq!(stale, vec![NodeId(2)]);
    }

    #[test]
    fn test_06_own_node_never_stale() {
        let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
        let shard = make_shard(clock.clone());
        clock.advance(Duration::from_secs(100));
        assert!(shard.stale_peers(Duration::from_secs(1)).is_empty());
    }

    // ── TEST-07: apply_mutation advances age ────────────────────

    #[test]
    fn test_07_mutation_advances_age() {
        let clock = test_clock();
        let mut shard = make_shard(clock);
        assert_eq!(shard.state().age(), 0);

        let outcome = shard.apply_mutation(
            |s| s.counter += 1,
            |s| s.clone(),
        );

        assert_eq!(outcome.generation.age, 1);
        assert_eq!(shard.state().age(), 1);
        assert_eq!(shard.state().value.counter, 1);
    }

    // ── PVM-01: Own node entry exists after construction ────────

    #[test]
    fn pvm_01_own_entry_after_construction() {
        let clock = test_clock();
        let shard = make_shard(clock);
        let snap = shard.snapshot();
        assert!(snap.contains_key(&NodeId(1)));
        assert_eq!(snap.len(), 1);
    }

    // ── PVM-02: Own entry derives from local shard ──────────────

    #[test]
    fn pvm_02_own_entry_matches_state() {
        let clock = test_clock();
        let shard = make_shard(clock);
        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(1)).unwrap();
        assert_eq!(entry.generation.age, 0);
        assert_eq!(entry.generation.incarnation, 1);
        assert_eq!(entry.value.counter, 0);
        assert_eq!(entry.value.label, "init");
    }

    // ── PVM-03: Map is empty of peers on single-node cluster ────

    #[test]
    fn pvm_03_no_peers_on_single_node() {
        let clock = test_clock();
        let shard = make_shard(clock);
        let snap = shard.snapshot();
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key(&NodeId(1)));
    }

    // ── SHARD-01: Mutation increments age ───────────────────────

    #[test]
    fn shard_01_mutation_increments_age() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        for i in 1..=5 {
            shard.apply_mutation(|s| s.counter = i, |s| s.clone());
        }

        assert_eq!(shard.state().age(), 5);
        assert_eq!(shard.state().value.counter, 5);
    }

    // ── SHARD-02: Mutation updates modified_time ────────────────

    #[test]
    fn shard_02_mutation_updates_modified_time() {
        let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
        let mut shard = make_shard(clock.clone());
        let t0 = shard.state().modified_time;

        clock.advance(Duration::from_secs(5));
        shard.apply_mutation(|s| s.counter += 1, |s| s.clone());

        assert!(shard.state().modified_time > t0);
    }

    // ── SHARD-03: Mutation produces view for sync ───────────────

    #[test]
    fn shard_03_mutation_produces_outbound_view() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        let outcome = shard.apply_mutation(
            |s| { s.counter = 42; s.label = "updated".into(); },
            |s| s.clone(),
        );

        assert_eq!(outcome.view.counter, 42);
        assert_eq!(outcome.view.label, "updated");
        assert_eq!(outcome.generation.incarnation, 1);
    }

    // ── SHARD-04: Sequential mutations produce sequential ages ──

    #[test]
    fn shard_04_sequential_mutations_sequential_ages() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        let mut ages = Vec::new();
        for i in 0..100 {
            let outcome = shard.apply_mutation(|s| s.counter = i, |s| s.clone());
            ages.push(outcome.generation.age);
        }

        let expected: Vec<u64> = (1..=100).collect();
        assert_eq!(ages, expected);
    }

    // ── SHARD-05: on_node_joined adds entry ─────────────────────

    #[test]
    fn shard_05_node_joined_adds_entry() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        let snap = shard.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.contains_key(&NodeId(2)));
        assert_eq!(snap[&NodeId(2)].generation.age, 0);
    }

    // ── SHARD-06: on_node_left removes entry ────────────────────

    #[test]
    fn shard_06_node_left_removes_entry() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);
        assert_eq!(shard.snapshot().len(), 2);

        shard.on_node_left(NodeId(2));
        assert_eq!(shard.snapshot().len(), 1);
        assert!(!shard.snapshot().contains_key(&NodeId(2)));
    }

    // ── SHARD-07: MarkStale sets pending_remote_age ─────────────

    #[test]
    fn shard_07_mark_stale_sets_pending() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        shard.mark_stale(NodeId(2), 1, 10);

        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(2)).unwrap();
        assert_eq!(entry.pending_remote_generation, Some(Generation::new(1, 10)));
    }

    // ── SHARD-08: MarkStale with stale incarnation is no-op ─────

    #[test]
    fn shard_08_mark_stale_old_incarnation_noop() {
        let clock = test_clock();
        let shard = make_shard(clock);

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(5, 10),
            wire_version: 1,
            view: TestState { counter: 10, label: "peer".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        shard.mark_stale(NodeId(2), 3, 20);

        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(2)).unwrap();
        assert_eq!(entry.pending_remote_generation, None);
    }

    // ── SHARD-10: snapshot returns full view map ────────────────

    #[test]
    fn shard_10_snapshot_returns_view_map() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view.clone());
        shard.on_node_joined(NodeId(3), default_view);

        let snap = shard.snapshot();
        assert_eq!(snap.len(), 3);
        assert!(snap.contains_key(&NodeId(1)));
        assert!(snap.contains_key(&NodeId(2)));
        assert!(snap.contains_key(&NodeId(3)));
    }

    // ── MUT-01: Mutation closure receives mutable state ─────────

    #[test]
    fn mut_01_closure_receives_mutable_state() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        shard.apply_mutation(
            |s| {
                s.counter = 99;
                s.label = "mutated".to_string();
            },
            |s| s.clone(),
        );

        assert_eq!(shard.state().value.counter, 99);
        assert_eq!(shard.state().value.label, "mutated");
    }

    // ── MUT-02: Mutation updates local view map ─────────────────

    #[test]
    fn mut_02_mutation_updates_view_map() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        shard.apply_mutation(|s| s.counter = 42, |s| s.clone());

        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(1)).unwrap();
        assert_eq!(entry.value.counter, 42);
        assert_eq!(entry.generation.age, 1);
    }

    // ── MUT-03: Mutation failure doesn't advance age (panic safety)

    #[test]
    fn mut_03_panic_safety_via_catch_unwind() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            shard.apply_mutation(
                |_s| panic!("intentional test panic"),
                |s| s.clone(),
            );
        }));

        assert!(result.is_err());
    }

    // ── MUT-04: Simple mutation produces view (for outbound snapshot)

    #[test]
    fn mut_04_mutation_broadcasts_snapshot_data() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        let outcome = shard.apply_mutation(
            |s| s.counter = 77,
            |s| s.clone(),
        );

        assert_eq!(outcome.view.counter, 77);
        assert_eq!(outcome.generation.age, 1);
        assert_eq!(outcome.generation.incarnation, 1);
    }

    // ── MUT-05: Receiver replaces state on inbound snapshot ─────

    #[test]
    fn mut_05_inbound_snapshot_replaces_view() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        let result = shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestState { counter: 50, label: "from-peer".into() },
            created_time: 1_000_000,
            modified_time: 1_005_000,
        });

        assert_eq!(result, AcceptResult::Accepted);
        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(2)).unwrap();
        assert_eq!(entry.value.counter, 50);
        assert_eq!(entry.generation.age, 5);
    }

    // ── MUT-06: Receiver discards stale inbound snapshot ────────

    #[test]
    fn mut_06_stale_snapshot_discarded() {
        let clock = test_clock();
        let shard = make_shard(clock);

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 10),
            wire_version: 1,
            view: TestState { counter: 10, label: "new".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        let result = shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestState { counter: 5, label: "old".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        assert_eq!(result, AcceptResult::Discarded);
        let snap = shard.snapshot();
        assert_eq!(snap[&NodeId(2)].value.counter, 10);
    }

    // ── DMUT-01: mutate_with_delta captures StateDeltaChange ────

    #[test]
    fn dmut_01_delta_mutation_captures_change() {
        let clock = test_clock();
        let mut shard = make_delta_shard(clock);

        let outcome = shard.apply_mutation_with_delta(
            |s| {
                s.counter += 5;
                s.label = "delta".to_string();
                TestDeltaChange {
                    counter_delta: 5,
                    new_label: Some("delta".to_string()),
                    accumulator_delta: 1.0,
                }
            },
            TestDeltaState::project_view,
            TestDeltaState::project_delta,
        );

        assert_eq!(outcome.generation.age, 1);
        assert_eq!(outcome.view.counter, 5);
        assert_eq!(outcome.view.label, "delta");
        assert_eq!(outcome.view_delta.counter_delta, 5);
        assert_eq!(outcome.view_delta.new_label, Some("delta".to_string()));
    }

    // ── DMUT-02: project_delta is called ────────────────────────

    #[test]
    fn dmut_02_project_delta_called() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let clock = test_clock();
        let mut shard = make_delta_shard(clock);
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        shard.apply_mutation_with_delta(
            |s| {
                s.counter += 1;
                TestDeltaChange {
                    counter_delta: 1,
                    new_label: None,
                    accumulator_delta: 0.0,
                }
            },
            TestDeltaState::project_view,
            move |change| {
                called_clone.store(true, Ordering::SeqCst);
                TestDeltaState::project_delta(change)
            },
        );

        assert!(called.load(Ordering::SeqCst));
    }

    // ── DMUT-03: Delta mutation updates view map via project_view

    #[test]
    fn dmut_03_delta_mutation_updates_view_map() {
        let clock = test_clock();
        let mut shard = make_delta_shard(clock);

        shard.apply_mutation_with_delta(
            |s| {
                s.counter = 42;
                s.label = "projected".to_string();
                s.internal_accumulator = 99.9;
                TestDeltaChange {
                    counter_delta: 42,
                    new_label: Some("projected".to_string()),
                    accumulator_delta: 99.9,
                }
            },
            TestDeltaState::project_view,
            TestDeltaState::project_delta,
        );

        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(1)).unwrap();
        assert_eq!(entry.value.counter, 42);
        assert_eq!(entry.value.label, "projected");
    }

    // ── DMUT-04: project_view called once (for the new view) ────

    #[test]
    fn dmut_04_project_view_called_once() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let clock = test_clock();
        let mut shard = make_delta_shard(clock);
        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = call_count.clone();

        shard.apply_mutation_with_delta(
            |s| {
                s.counter += 1;
                TestDeltaChange {
                    counter_delta: 1,
                    new_label: None,
                    accumulator_delta: 0.0,
                }
            },
            move |state| {
                count_clone.fetch_add(1, Ordering::SeqCst);
                TestDeltaState::project_view(state)
            },
            TestDeltaState::project_delta,
        );

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    // ── DMUT-05: delta mutation produces view_delta for sync ────

    #[test]
    fn dmut_05_delta_mutation_produces_view_delta() {
        let clock = test_clock();
        let mut shard = make_delta_shard(clock);

        let outcome = shard.apply_mutation_with_delta(
            |s| {
                s.counter += 10;
                TestDeltaChange {
                    counter_delta: 10,
                    new_label: None,
                    accumulator_delta: 5.0,
                }
            },
            TestDeltaState::project_view,
            TestDeltaState::project_delta,
        );

        assert_eq!(outcome.view_delta.counter_delta, 10);
        assert_eq!(outcome.view_delta.new_label, None);
    }

    // ── DMUT-06: Simple mutate on delta shard passes no delta ───

    #[test]
    fn dmut_06_simple_mutate_on_delta_shard() {
        let clock = test_clock();
        let mut shard = make_delta_shard(clock);

        let outcome = shard.apply_mutation(
            |s| s.counter = 7,
            TestDeltaState::project_view,
        );

        assert_eq!(outcome.generation.age, 1);
        assert_eq!(outcome.view.counter, 7);
    }

    // ── Additional: inbound delta tests ─────────────────────────

    #[test]
    fn inbound_delta_applied_correctly() {
        let clock = test_clock();
        let shard: ShardCore<TestDeltaStateInner, TestDeltaView> = make_delta_shard(clock);

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestDeltaView { counter: 10, label: "peer".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        // Delta advances from age 5 to age 6: generation = (1, 6).
        let result = shard.accept_inbound_delta(
            NodeId(2), Generation::new(1, 6), 1,
            |view| TestDeltaState::apply_delta(
                view,
                &TestDeltaViewDelta { counter_delta: 3, new_label: None },
            ),
        );

        assert_eq!(result, DeltaAcceptResult::Applied);
        let snap = shard.snapshot();
        assert_eq!(snap[&NodeId(2)].value.counter, 13);
        assert_eq!(snap[&NodeId(2)].generation.age, 6);
    }

    #[test]
    fn inbound_delta_gap_detected() {
        let clock = test_clock();
        let shard: ShardCore<TestDeltaStateInner, TestDeltaView> = make_delta_shard(clock);

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestDeltaView { counter: 10, label: "peer".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        // Peer is at age 5 but delta claims generation (1, 8) — gap (skipped ages).
        let result = shard.accept_inbound_delta(
            NodeId(2), Generation::new(1, 8), 1,
            |_| unreachable!(),
        );

        assert_eq!(result, DeltaAcceptResult::GapDetected {
            local: Generation::new(1, 5),
            incoming: Generation::new(1, 8),
        });
    }

    #[test]
    fn inbound_delta_stale_discarded() {
        let clock = test_clock();
        let shard: ShardCore<TestDeltaStateInner, TestDeltaView> = make_delta_shard(clock);

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(2, 10),
            wire_version: 1,
            view: TestDeltaView { counter: 10, label: "peer".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        // Delta from old incarnation 1 but peer is at incarnation 2.
        let result = shard.accept_inbound_delta(
            NodeId(2), Generation::new(1, 10), 1,
            |_| unreachable!(),
        );

        assert_eq!(result, DeltaAcceptResult::Discarded);
    }

    #[test]
    fn inbound_delta_unknown_peer() {
        let clock = test_clock();
        let shard: ShardCore<TestDeltaStateInner, TestDeltaView> = make_delta_shard(clock);

        let result = shard.accept_inbound_delta(
            NodeId(99), Generation::new(1, 1), 1,
            |_| unreachable!(),
        );

        assert_eq!(result, DeltaAcceptResult::UnknownPeer);
    }

    // ── Additional: mark_stale with new incarnation ─────────────

    #[test]
    fn mark_stale_new_incarnation_sets_both() {
        let clock = test_clock();
        let shard = make_shard(clock);

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestState { counter: 5, label: "peer".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        shard.mark_stale(NodeId(2), 2, 3);

        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(2)).unwrap();
        assert_eq!(entry.pending_remote_generation, Some(Generation::new(2, 3)));
    }

    // ── Additional: on_node_joined idempotent ───────────────────

    #[test]
    fn node_joined_idempotent() {
        let clock = test_clock();
        let shard = make_shard(clock);
        let default_view = TestState { counter: 0, label: String::new() };

        shard.on_node_joined(NodeId(2), default_view.clone());
        shard.on_node_joined(NodeId(2), default_view);

        assert_eq!(shard.snapshot().len(), 2);
    }

    // ── Additional: on_node_left for unknown node is noop ───────

    #[test]
    fn node_left_unknown_noop() {
        let clock = test_clock();
        let shard = make_shard(clock);
        shard.on_node_left(NodeId(99));
        assert_eq!(shard.snapshot().len(), 1);
    }

    // ── Additional: inbound snapshot for new peer without join ──

    #[test]
    fn inbound_snapshot_creates_entry_for_unknown_peer() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let result = shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(5),
            generation: Generation::new(1, 1),
            wire_version: 1,
            view: TestState { counter: 1, label: "new".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        assert_eq!(result, AcceptResult::Accepted);
        assert_eq!(shard.snapshot().len(), 2);
        assert_eq!(shard.snapshot()[&NodeId(5)].value.counter, 1);
    }

    // ── Additional: accept snapshot clears pending_remote ───────

    #[test]
    fn accept_snapshot_clears_pending_remote() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);
        shard.mark_stale(NodeId(2), 1, 10);

        assert!(shard.snapshot()[&NodeId(2)].pending_remote_generation.is_some());

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 10),
            wire_version: 1,
            view: TestState { counter: 10, label: "fresh".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        let snap = shard.snapshot();
        assert_eq!(snap[&NodeId(2)].pending_remote_generation, None);
    }

    // ── Additional: new_state_object helper ─────────────────────

    #[test]
    fn test_new_state_object() {
        let clock = TestClock::with_base_unix_ms(5_000_000);
        let state = new_state_object(
            TestState { counter: 0, label: "fresh".into() },
            42,
            &clock,
        );

        assert_eq!(state.age(), 0);
        assert_eq!(state.incarnation(), 42);
        assert_eq!(state.created_time, 5_000_000);
        assert_eq!(state.modified_time, 5_000_000);
        assert_eq!(state.value.counter, 0);
    }
}
