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
/// A mutation that has been applied to a clone of the state but not yet
/// committed. The caller should persist `state()` and then call
/// `ShardCore::commit_mutation` on success.
pub(crate) struct PreparedMutation<S: Clone, V: Clone> {
    pub(crate) new_state: StateObject<S>,
    pub(crate) view: V,
}

#[allow(dead_code)]
impl<S: Clone, V: Clone> PreparedMutation<S, V> {
    /// The state to persist before committing.
    pub fn state(&self) -> &StateObject<S> {
        &self.new_state
    }
}

#[allow(dead_code)] // Used by tests and future actor shell
/// A delta mutation that has been applied to a clone but not yet committed.
pub(crate) struct PreparedDeltaMutation<S: Clone, V: Clone, VD: Clone> {
    pub(crate) new_state: StateObject<S>,
    pub(crate) view: V,
    pub(crate) view_delta: VD,
}

#[allow(dead_code)]
impl<S: Clone, V: Clone, VD: Clone> PreparedDeltaMutation<S, V, VD> {
    /// The state to persist before committing.
    pub fn state(&self) -> &StateObject<S> {
        &self.new_state
    }
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

/// Result of a freshness-checked query against the view map.
///
/// # Threading model
///
/// This enum is returned by [`ShardCore::query_local`]. It is intended for
/// single-actor use — the actor shell inspects the result and either returns
/// the projection directly (fast path) or initiates pulls for stale peers
/// before re-querying (slow path).
#[derive(Debug)]
pub(crate) enum QueryResult<R> {
    /// All views are fresh. The projection result is ready.
    Fresh(R),
    /// Some peers have stale views. The projection ran on the current
    /// (potentially stale) data, but the caller should pull fresh data
    /// from the listed peers and re-query if freshness is required.
    StalePeersDetected {
        stale_peers: Vec<NodeId>,
        /// The projection result computed on the current (stale) snapshot.
        /// The actor shell may discard this and re-project after pulling.
        result: R,
    },
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

    // ── Two-phase mutation (persist-before-publish) ────────────

    /// Phase 1: Prepare a mutation without modifying state or view map.
    ///
    /// Clones the state, applies the mutation to the clone, and computes
    /// the new view. The caller should persist `prepared.state()` and then
    /// call [`commit_mutation`] on success, or simply drop the prepared
    /// value on failure (state remains unchanged).
    pub fn prepare_mutation<F, P>(
        &self,
        mutate_fn: F,
        project_fn: P,
    ) -> PreparedMutation<S, V>
    where
        F: FnOnce(&mut S),
        P: FnOnce(&S) -> V,
    {
        let mut new_state = self.state.clone();
        mutate_fn(&mut new_state.value);
        new_state.generation.age += 1;
        new_state.modified_time = self.clock.unix_ms();

        let view = project_fn(&new_state.value);

        PreparedMutation { new_state, view }
    }

    /// Phase 2: Commit a previously prepared mutation.
    ///
    /// Replaces the owned state and updates the local view map entry.
    /// Only call after persistence succeeds.
    pub fn commit_mutation(
        &mut self,
        prepared: PreparedMutation<S, V>,
    ) -> MutationOutcome<V> {
        self.state = prepared.new_state;
        self.update_own_view(prepared.view.clone());

        MutationOutcome {
            generation: self.state.generation,
            view: prepared.view,
        }
    }

    /// Phase 1 (delta-aware): Prepare a delta mutation without modifying
    /// state or view map.
    pub fn prepare_mutation_with_delta<F, PV, PD, DC, VD>(
        &self,
        mutate_fn: F,
        project_view_fn: PV,
        project_delta_fn: PD,
    ) -> PreparedDeltaMutation<S, V, VD>
    where
        F: FnOnce(&mut S) -> DC,
        PV: FnOnce(&S) -> V,
        PD: FnOnce(&DC) -> VD,
        VD: Clone + Debug,
    {
        let mut new_state = self.state.clone();
        let delta_change = mutate_fn(&mut new_state.value);
        new_state.generation.age += 1;
        new_state.modified_time = self.clock.unix_ms();

        let view = project_view_fn(&new_state.value);
        let view_delta = project_delta_fn(&delta_change);

        PreparedDeltaMutation {
            new_state,
            view,
            view_delta,
        }
    }

    /// Phase 2 (delta-aware): Commit a previously prepared delta mutation.
    pub fn commit_delta_mutation<VD: Clone + Debug>(
        &mut self,
        prepared: PreparedDeltaMutation<S, V, VD>,
    ) -> DeltaMutationOutcome<V, VD> {
        self.state = prepared.new_state;
        self.update_own_view(prepared.view.clone());

        DeltaMutationOutcome {
            generation: self.state.generation,
            view: prepared.view,
            view_delta: prepared.view_delta,
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
            // Only clear pending_remote_generation if we've caught up.
            let pending = existing
                .and_then(|e| e.pending_remote_generation)
                .filter(|p| snap.generation < *p);
            Some(StateViewObject {
                generation: snap.generation,
                wire_version: snap.wire_version,
                value: snap.view,
                created_time: snap.created_time,
                modified_time: snap.modified_time,
                synced_at: now,
                pending_remote_generation: pending,
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

        // Only clear pending_remote_generation if we've caught up.
        let pending = existing
            .pending_remote_generation
            .filter(|p| generation < *p);

        self.views.update(
            &source,
            StateViewObject {
                generation,
                wire_version,
                value: new_view,
                created_time: existing.created_time,
                modified_time: self.clock.unix_ms(),
                synced_at: now,
                pending_remote_generation: pending,
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
    /// Adds a placeholder entry for the new node if it doesn't already exist.
    /// The placeholder uses `Generation::zero()` (incarnation=0, age=0) which
    /// is always less than any real generation (real incarnations use
    /// `current_unix_time_ms()`). The entry is marked stale via
    /// `pending_remote_generation` so the query path knows to pull.
    pub fn on_node_joined(&self, node_id: NodeId, default_view: V) {
        if node_id == self.node_id {
            return; // don't re-add ourselves
        }

        let now = self.clock.now();
        // Mark as stale so query path triggers a pull for the real state.
        let pending = Some(Generation::new(1, 0));
        self.views.insert_node(
            node_id,
            StateViewObject {
                generation: Generation::zero(),
                wire_version: 0,
                value: default_view,
                created_time: 0,
                modified_time: 0,
                synced_at: now,
                pending_remote_generation: pending,
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

    /// Query the view map with a freshness requirement.
    ///
    /// Takes a snapshot of the view map and checks every peer's view for
    /// staleness (by elapsed time since `synced_at` or by
    /// `pending_remote_generation`). The projection always runs against
    /// the captured snapshot regardless of staleness — this lets the actor
    /// shell use the result immediately or discard it after pulling.
    ///
    /// Returns [`QueryResult::Fresh`] if all peers are within
    /// `max_staleness`, or [`QueryResult::StalePeersDetected`] with the
    /// stale peer list and the (potentially stale) projection result.
    ///
    /// The local node's own entry is never considered stale.
    pub fn query_local<R, F>(&self, max_staleness: Duration, project: F) -> QueryResult<R>
    where
        F: FnOnce(&HashMap<NodeId, StateViewObject<V>>) -> R,
    {
        let snapshot = self.views.snapshot();
        let now = self.clock.now();

        // Check staleness against the same snapshot used for projection
        // to avoid TOCTOU races between snapshot() and stale_peers().
        let stale: Vec<NodeId> = snapshot
            .iter()
            .filter(|(id, _)| **id != self.node_id)
            .filter(|(_, view)| {
                view.pending_remote_generation.is_some()
                    || now.duration_since(view.synced_at) > max_staleness
            })
            .map(|(id, _)| *id)
            .collect();

        let result = project(&snapshot);
        if stale.is_empty() {
            QueryResult::Fresh(result)
        } else {
            QueryResult::StalePeersDetected {
                stale_peers: stale,
                result,
            }
        }
    }

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

        // Peer is stale immediately after join (placeholder has pending_remote_generation).
        assert_eq!(shard.stale_peers(Duration::from_secs(10)), vec![NodeId(2)]);

        // Accept a snapshot to clear the stale marker.
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 1),
            wire_version: 1,
            view: TestState { counter: 1, label: "synced".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        // Now not stale (just synced).
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

    // ── Partial catch-up preserves pending_remote_generation ────

    #[test]
    fn accept_snapshot_partial_catchup_preserves_pending() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);
        // Change feed says peer is at generation (1, 10).
        shard.mark_stale(NodeId(2), 1, 10);

        // Accept a delayed snapshot at generation (1, 6) — still behind.
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 6),
            wire_version: 1,
            view: TestState { counter: 6, label: "partial".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        let snap = shard.snapshot();
        // pending_remote_generation should be preserved — we're not caught up yet.
        assert_eq!(snap[&NodeId(2)].pending_remote_generation, Some(Generation::new(1, 10)));
        assert_eq!(snap[&NodeId(2)].value.counter, 6);
    }

    // ── Delta partial catch-up preserves pending_remote_generation ──

    #[test]
    fn accept_delta_partial_catchup_preserves_pending() {
        let clock = test_clock();
        let shard = make_delta_shard(clock.clone());

        let default_view = TestDeltaView { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        // Accept snapshot to establish baseline at (1, 5).
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestDeltaView { counter: 5, label: "base".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        // Change feed says peer is at (1, 10).
        shard.mark_stale(NodeId(2), 1, 10);

        // Apply delta advancing to (1, 6) — still behind (1, 10).
        let result = shard.accept_inbound_delta(
            NodeId(2), Generation::new(1, 6), 1,
            |v| TestDeltaView { counter: v.counter + 1, label: v.label.clone() },
        );
        assert_eq!(result, DeltaAcceptResult::Applied);

        let snap = shard.snapshot();
        assert_eq!(snap[&NodeId(2)].pending_remote_generation, Some(Generation::new(1, 10)));
    }

    // ── on_node_joined marks placeholder as stale ──────────────

    #[test]
    fn node_joined_placeholder_is_stale() {
        let clock = test_clock();
        let shard = make_shard(clock);

        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(2)).unwrap();
        // Placeholder should have pending_remote_generation set (marked stale).
        assert!(entry.pending_remote_generation.is_some());
        assert_eq!(entry.generation, Generation::zero());
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

    // ── WIRE-05: Full snapshot updates view map ─────────────────

    #[test]
    fn wire_full_snapshot_updates_view_map() {
        use crate::traits::state::DistributedState;

        let clock = test_clock();
        let shard = make_shard(clock);

        // Simulate a peer joining.
        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        // Build a SyncMessage::FullSnapshot as if received over the wire,
        // then extract its fields into an InboundSnapshot.
        let source_node = NodeId(2);
        let gen = Generation::new(1, 7);
        let view = TestState { counter: 42, label: "wire-snap".into() };
        let wire_version = TestState::WIRE_VERSION;

        let result = shard.accept_inbound_snapshot(InboundSnapshot {
            source: source_node,
            generation: gen,
            wire_version,
            view: view.clone(),
            created_time: 1_000_000,
            modified_time: 1_007_000,
        });

        assert_eq!(result, AcceptResult::Accepted);
        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(2)).unwrap();
        assert_eq!(entry.generation, gen);
        assert_eq!(entry.value.counter, 42);
        assert_eq!(entry.value.label, "wire-snap");
    }

    // ── WIRE-06: Delta update advances peer view ────────────────

    #[test]
    fn wire_delta_advances_peer_view() {
        let clock = test_clock();
        let shard: ShardCore<TestDeltaStateInner, TestDeltaView> = make_delta_shard(clock);

        // Join peer and accept baseline snapshot at (1, 5).
        let default_view = TestDeltaView { counter: 0, label: String::new() };
        shard.on_node_joined(NodeId(2), default_view);

        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestDeltaView { counter: 100, label: "baseline".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        // Simulate receiving a DeltaUpdate: generation advances to (1, 6).
        let result = shard.accept_inbound_delta(
            NodeId(2),
            Generation::new(1, 6),
            1,
            |v| TestDeltaView { counter: v.counter + 5, label: v.label.clone() },
        );

        assert_eq!(result, DeltaAcceptResult::Applied);
        let snap = shard.snapshot();
        assert_eq!(snap[&NodeId(2)].generation, Generation::new(1, 6));
        assert_eq!(snap[&NodeId(2)].value.counter, 105);
    }

    // ── WIRE-07: Change feed marks peer stale ───────────────────

    #[test]
    fn wire_change_feed_marks_peer_stale() {
        let clock = test_clock();
        let shard = make_shard(clock);

        // Accept a snapshot from peer at (1, 5).
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestState { counter: 5, label: "peer".into() },
            created_time: 1_000_000,
            modified_time: 1_000_000,
        });

        // Simulate a BatchedChangeFeed with a ChangeNotification at (1, 10).
        // The actor shell would call mark_stale on the shard.
        shard.mark_stale(NodeId(2), 1, 10);

        let snap = shard.snapshot();
        let entry = snap.get(&NodeId(2)).unwrap();
        assert_eq!(entry.pending_remote_generation, Some(Generation::new(1, 10)));
        // The view value should still be the old snapshot (not yet updated).
        assert_eq!(entry.value.counter, 5);
    }

    // ── WIRE-08: Request snapshot response ──────────────────────

    #[test]
    fn wire_request_snapshot_response() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        // Mutate a few times to build up state.
        shard.apply_mutation(|s| { s.counter = 10; s.label = "first".into(); }, |s| s.clone());
        shard.apply_mutation(|s| { s.counter = 20; s.label = "second".into(); }, |s| s.clone());
        shard.apply_mutation(|s| { s.counter = 30; s.label = "third".into(); }, |s| s.clone());

        // Verify that state() and snapshot() provide the data a peer would
        // need when it sends RequestSnapshot. The actual wire sending is the
        // actor shell's responsibility — here we just verify data accessibility.
        let state = shard.state();
        assert_eq!(state.value.counter, 30);
        assert_eq!(state.value.label, "third");
        assert_eq!(state.generation.age, 3);
        assert_eq!(state.generation.incarnation, 1);

        let snap = shard.snapshot();
        assert_eq!(snap[&NodeId(1)].value.counter, 30);
        assert_eq!(snap[&NodeId(1)].generation.age, 3);
    }

    // ── WIRE-09: Unknown peer snapshot accepted ─────────────────

    #[test]
    fn wire_unknown_peer_snapshot_accepted() {
        let clock = test_clock();
        let shard = make_shard(clock);

        // No prior NodeJoined for NodeId(99) — accept_inbound_snapshot
        // should handle this via update_if which upserts.
        let result = shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(99),
            generation: Generation::new(3, 15),
            wire_version: 1,
            view: TestState { counter: 999, label: "unknown-peer".into() },
            created_time: 2_000_000,
            modified_time: 2_015_000,
        });

        assert_eq!(result, AcceptResult::Accepted);
        let snap = shard.snapshot();
        // Should have created a new entry (self + unknown peer).
        assert_eq!(snap.len(), 2);
        let entry = snap.get(&NodeId(99)).unwrap();
        assert_eq!(entry.value.counter, 999);
        assert_eq!(entry.value.label, "unknown-peer");
        assert_eq!(entry.generation, Generation::new(3, 15));
    }

    // ══════════════════════════════════════════════════════════════
    // PR 4 — Query Logic Tests
    // ══════════════════════════════════════════════════════════════

    /// Helper: add a peer with a fresh snapshot at a given generation.
    fn add_fresh_peer(shard: &ShardCore<TestState, TestState>, node_id: NodeId, counter: u64, clock: &dyn Clock) {
        let default_view = TestState { counter: 0, label: String::new() };
        shard.on_node_joined(node_id, default_view);
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: node_id,
            generation: Generation::new(1, 1),
            wire_version: 1,
            view: TestState { counter, label: format!("peer-{}", node_id.0) },
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        });
    }

    // ── QUERY-01: Projection callback receives full view map ────

    #[test]
    fn query_01_projection_receives_full_view_map() {
        let clock = test_clock();
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());
        add_fresh_peer(&shard, NodeId(3), 20, clock.as_ref());
        add_fresh_peer(&shard, NodeId(4), 30, clock.as_ref());

        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap.len()
        });

        match result {
            QueryResult::Fresh(count) => assert_eq!(count, 4), // self + 3 peers
            QueryResult::StalePeersDetected { .. } => panic!("expected fresh"),
        }
    }

    // ── QUERY-02: Stale entries detected by time ────────────────

    #[test]
    fn query_02_stale_entries_detected_by_time() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());

        // Advance time beyond max_staleness.
        tc.advance(Duration::from_secs(10));

        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap.len()
        });

        match result {
            QueryResult::StalePeersDetected { stale_peers, result } => {
                assert_eq!(stale_peers, vec![NodeId(2)]);
                assert_eq!(result, 2);
            }
            QueryResult::Fresh(_) => panic!("expected stale detection"),
        }
    }

    // ── QUERY-03: Fresh entries not re-fetched ──────────────────

    #[test]
    fn query_03_fresh_entries_not_flagged() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());
        add_fresh_peer(&shard, NodeId(3), 20, clock.as_ref());

        // Advance time but stay within max_staleness.
        tc.advance(Duration::from_secs(2));

        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap.len()
        });

        match result {
            QueryResult::Fresh(count) => assert_eq!(count, 3),
            QueryResult::StalePeersDetected { .. } => panic!("expected fresh"),
        }
    }

    // ── QUERY-04: Callback result is returned to caller ─────────

    #[test]
    fn query_04_callback_result_returned() {
        let clock = test_clock();
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 42, clock.as_ref());

        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap.values()
                .map(|v| v.value.counter)
                .sum::<u64>()
        });

        match result {
            QueryResult::Fresh(sum) => assert_eq!(sum, 42), // self=0 + peer=42
            QueryResult::StalePeersDetected { .. } => panic!("expected fresh"),
        }
    }

    // ── QUERY-05: Stale peer produces StalePeersDetected ────────

    #[test]
    fn query_05_unreachable_peer_returns_stale() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());
        add_fresh_peer(&shard, NodeId(3), 20, clock.as_ref());

        // Only peer 2 goes stale.
        tc.advance(Duration::from_secs(6));
        // Re-sync peer 3 so it's fresh.
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(3),
            generation: Generation::new(1, 2),
            wire_version: 1,
            view: TestState { counter: 25, label: "refreshed".into() },
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        });

        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap[&NodeId(3)].value.counter
        });

        match result {
            QueryResult::StalePeersDetected { stale_peers, result } => {
                assert_eq!(stale_peers, vec![NodeId(2)]);
                assert_eq!(result, 25);
            }
            QueryResult::Fresh(_) => panic!("expected stale detection for peer 2"),
        }
    }

    // ── QUERY-06: pending_remote_generation triggers stale detection

    #[test]
    fn query_06_pending_remote_generation_triggers_stale() {
        let clock = test_clock();
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());

        // Simulate change feed notification: peer 2 is now at generation (1, 5).
        shard.mark_stale(NodeId(2), 1, 5);

        let result = shard.query_local(Duration::from_secs(60), |snap| {
            snap[&NodeId(2)].value.counter
        });

        match result {
            QueryResult::StalePeersDetected { stale_peers, result } => {
                assert_eq!(stale_peers, vec![NodeId(2)]);
                assert_eq!(result, 10); // old value still visible
            }
            QueryResult::Fresh(_) => panic!("expected stale detection via pending_remote_generation"),
        }
    }

    // ── QUERY-07: Multiple stale peers detected concurrently ────

    #[test]
    fn query_07_multiple_stale_peers() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());
        add_fresh_peer(&shard, NodeId(3), 20, clock.as_ref());
        add_fresh_peer(&shard, NodeId(4), 30, clock.as_ref());

        // All peers go stale.
        tc.advance(Duration::from_secs(10));

        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap.len()
        });

        match result {
            QueryResult::StalePeersDetected { mut stale_peers, result } => {
                stale_peers.sort();
                assert_eq!(stale_peers, vec![NodeId(2), NodeId(3), NodeId(4)]);
                assert_eq!(result, 4);
            }
            QueryResult::Fresh(_) => panic!("expected 3 stale peers"),
        }
    }

    // ── QUERY-08: snapshot() returns stale data without pulling ──

    #[test]
    fn query_08_snapshot_returns_stale_data_without_freshness() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 42, clock.as_ref());

        // Make it very stale.
        tc.advance(Duration::from_secs(3600));

        // snapshot() does not check freshness — returns whatever is there.
        let snap = shard.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[&NodeId(2)].value.counter, 42);
    }

    // ── QUERY: local node is never considered stale ─────────────

    #[test]
    fn query_local_node_never_stale() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        // Advance time so even the local node's synced_at is old.
        tc.advance(Duration::from_secs(100));

        // With no peers, query should always be fresh (local node excluded).
        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap.len()
        });

        match result {
            QueryResult::Fresh(count) => assert_eq!(count, 1),
            QueryResult::StalePeersDetected { .. } => panic!("local node should not be stale"),
        }
    }

    // ══════════════════════════════════════════════════════════════
    // PR 4 — Dynamic Sync Urgency Integration Tests
    // ══════════════════════════════════════════════════════════════

    // These tests verify the integration between SyncLogic.resolve_action()
    // and ShardCore mutation outcomes. The actual dispatch (push/timer/suppress)
    // is actor-shell behavior, but we test the decision logic here.

    use crate::core::sync_logic::{SyncAction, SyncLogic};
    use crate::types::config::SyncStrategy;
    use crate::traits::state::SyncUrgency;

    // ── SYNC-14: Immediate bypasses timer ───────────────────────

    #[test]
    fn sync_14_immediate_bypasses_timer() {
        let logic = SyncLogic::new("counters".into(), SyncStrategy::active_push(), true);
        let action = logic.resolve_action(SyncUrgency::Immediate);
        assert_eq!(action, SyncAction::BroadcastDelta);
    }

    // ── SYNC-15: Delayed overrides interval ─────────────────────

    #[test]
    fn sync_15_delayed_overrides_interval() {
        let logic = SyncLogic::new("counters".into(), SyncStrategy::active_push(), true);
        let delay = Duration::from_secs(1);
        let action = logic.resolve_action(SyncUrgency::Delayed(delay));
        assert_eq!(action, SyncAction::ScheduleDelayed(Duration::from_secs(1)));
    }

    // ── SYNC-16: Suppress skips broadcast ───────────────────────

    #[test]
    fn sync_16_suppress_skips_broadcast() {
        let logic = SyncLogic::new("counters".into(), SyncStrategy::active_push(), true);
        let action = logic.resolve_action(SyncUrgency::Suppress);
        assert_eq!(action, SyncAction::Suppress);
    }

    // ── SYNC-17: Suppressed deltas accumulate ───────────────────

    #[test]
    fn sync_17_suppressed_deltas_accumulate() {
        use crate::core::delta_accumulator::DeltaAccumulator;

        let logic = SyncLogic::new("counters".into(), SyncStrategy::active_push(), true);
        let mut acc = DeltaAccumulator::new();

        // 3 suppressed mutations → accumulate.
        for i in 0..3 {
            let action = logic.resolve_action(SyncUrgency::Suppress);
            assert_eq!(action, SyncAction::Suppress);
            acc.accumulate(format!("delta-{i}"));
        }

        // Non-suppressed mutation → flush accumulated + broadcast current.
        let action = logic.resolve_action(SyncUrgency::Immediate);
        assert_eq!(action, SyncAction::BroadcastDelta);

        let flushed = acc.flush().unwrap();
        assert_eq!(flushed.len(), 3);
        assert_eq!(flushed[0], "delta-0");
        assert_eq!(flushed[2], "delta-2");
        // After flush, buffer is empty.
        assert!(acc.is_empty());
    }

    // ── SYNC-18: Default uses configured strategy ───────────────

    #[test]
    fn sync_18_default_uses_configured_strategy() {
        // ActivePush → BroadcastDelta for Default urgency.
        let push_logic = SyncLogic::new("counters".into(), SyncStrategy::active_push(), true);
        assert_eq!(
            push_logic.resolve_action(SyncUrgency::Default),
            SyncAction::BroadcastDelta,
        );

        // ActiveFeedLazyPull → NotifyChangeFeed for Default urgency.
        let feed_logic = SyncLogic::new("sessions".into(), SyncStrategy::feed_lazy_pull(), true);
        assert_eq!(
            feed_logic.resolve_action(SyncUrgency::Default),
            SyncAction::NotifyChangeFeed,
        );
    }

    // ══════════════════════════════════════════════════════════════
    // PR 4 — Error Handling Tests
    // ══════════════════════════════════════════════════════════════

    // ── ERR-05: Stale-state retry succeeds on second attempt ────

    #[test]
    fn err_05_stale_retry_succeeds_after_refresh() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());

        // First query: peer is stale.
        tc.advance(Duration::from_secs(10));

        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap[&NodeId(2)].value.counter
        });
        assert!(matches!(result, QueryResult::StalePeersDetected { .. }));

        // Simulate the actor shell pulling fresh data.
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestState { counter: 99, label: "refreshed".into() },
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        });

        // Second query: all fresh now.
        let result = shard.query_local(Duration::from_secs(5), |snap| {
            snap[&NodeId(2)].value.counter
        });
        match result {
            QueryResult::Fresh(counter) => assert_eq!(counter, 99),
            QueryResult::StalePeersDetected { .. } => panic!("expected fresh after refresh"),
        }
    }

    // ── ERR-06: Backoff between retries ─────────────────────────
    // At the pure-logic level, we verify that stale_peers() consistently
    // returns the same stale peers until they are refreshed, supporting
    // a retry loop with backoff at the actor shell level.

    #[test]
    fn err_06_stale_peers_consistent_for_retry() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_shard(clock.clone());

        add_fresh_peer(&shard, NodeId(2), 10, clock.as_ref());
        tc.advance(Duration::from_secs(10));

        // Multiple calls consistently report the same stale peer.
        let stale1 = shard.stale_peers(Duration::from_secs(5));
        let stale2 = shard.stale_peers(Duration::from_secs(5));
        assert_eq!(stale1, stale2);
        assert_eq!(stale1, vec![NodeId(2)]);

        // After refresh, peer is no longer stale.
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(1, 5),
            wire_version: 1,
            view: TestState { counter: 99, label: "refreshed".into() },
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        });

        let stale3 = shard.stale_peers(Duration::from_secs(5));
        assert!(stale3.is_empty());
    }

    // ═══════════════════════════════════════════════════════════
    // Persist-before-publish (two-phase mutation) tests
    // ═══════════════════════════════════════════════════════════

    // ── PERSIST-01: save() called after each mutation ────────────

    #[test]
    fn persist_01_prepare_returns_state_to_persist() {
        let clock = test_clock();
        let shard = make_shard(clock.clone());

        // Three prepare calls — each returns the next state to persist
        let p1 = shard.prepare_mutation(|s| s.counter = 1, |s| s.clone());
        assert_eq!(p1.state().generation.age, 1);
        assert_eq!(p1.state().value.counter, 1);

        // Without committing p1, shard is unchanged — but we can still
        // prepare again (would produce same age since shard hasn't changed)
        let p1b = shard.prepare_mutation(|s| s.counter = 10, |s| s.clone());
        assert_eq!(p1b.state().generation.age, 1);
        assert_eq!(p1b.state().value.counter, 10);
    }

    // ── PERSIST-03: State survives actor restart (simulation) ────

    #[test]
    fn persist_03_state_survives_restart() {
        let clock = test_clock();
        let mut shard = make_shard(clock.clone());

        // Mutate to age 5
        for i in 1..=5 {
            let prepared = shard.prepare_mutation(
                move |s| s.counter = i,
                |s| s.clone(),
            );
            // Simulate successful persist
            shard.commit_mutation(prepared);
        }
        let saved = shard.state().clone();
        assert_eq!(saved.generation.age, 5);
        assert_eq!(saved.value.counter, 5);

        // Simulate restart — create new shard from saved state
        let shard2 = ShardCore::new(
            NodeId(1),
            saved.clone(),
            saved.value.clone(),
            clock,
        );
        assert_eq!(shard2.state().generation.age, 5);
        assert_eq!(shard2.state().value.counter, 5);
    }

    // ── PERSIST-04: No persistence when disabled ─────────────────

    #[test]
    fn persist_04_no_persistence_direct_commit() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        // When persistence is disabled, the actor shell calls
        // prepare + commit without persisting in between.
        let prepared = shard.prepare_mutation(|s| s.counter = 42, |s| s.clone());
        let outcome = shard.commit_mutation(prepared);

        assert_eq!(outcome.generation.age, 1);
        assert_eq!(shard.state().value.counter, 42);
    }

    // ── PERSIST-05 / ERR-03: Save failure returns no outcome ─────

    #[test]
    fn persist_05_err_03_save_failure_no_commit() {
        let clock = test_clock();
        let shard = make_shard(clock);

        // Prepare a mutation
        let _prepared = shard.prepare_mutation(|s| s.counter = 99, |s| s.clone());

        // Simulate save failure — just drop the prepared value.
        // The MutationError::PersistenceFailed is constructed by the actor
        // shell. Here we verify that dropping the prepared value leaves
        // shard state unchanged.
        drop(_prepared);

        assert_eq!(shard.state().generation.age, 0);
        assert_eq!(shard.state().value.counter, 0);
    }

    // ── PERSIST-07: Save failure prevents age advance ────────────

    #[test]
    fn persist_07_save_failure_prevents_age_advance() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        // Successful mutation to age 3
        for _ in 0..3 {
            let p = shard.prepare_mutation(|s| s.counter += 1, |s| s.clone());
            shard.commit_mutation(p);
        }
        assert_eq!(shard.state().generation.age, 3);

        // Next mutation: prepare succeeds but "save fails" → don't commit
        let prepared = shard.prepare_mutation(|s| s.counter += 1, |s| s.clone());
        assert_eq!(prepared.state().generation.age, 4);
        drop(prepared); // simulate save failure

        // Age is still 3
        assert_eq!(shard.state().generation.age, 3);
        assert_eq!(shard.state().value.counter, 3);
    }

    // ── PERSIST-08: Save failure leaves view_map unchanged ───────

    #[test]
    fn persist_08_save_failure_viewmap_unchanged() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        shard.apply_mutation(|s| s.counter = 5, |s| s.clone());
        let view_before = shard.get_view(&NodeId(1)).unwrap();
        assert_eq!(view_before.value.counter, 5);

        // Prepare mutation but don't commit (save failure)
        let prepared = shard.prepare_mutation(|s| s.counter = 99, |s| s.clone());
        drop(prepared);

        // View map still shows counter=5
        let view_after = shard.get_view(&NodeId(1)).unwrap();
        assert_eq!(view_after.value.counter, 5);
        assert_eq!(view_after.generation.age, 1);
    }

    // ── PERSIST-09: Save failure does not broadcast ──────────────

    #[test]
    fn persist_09_save_failure_no_broadcast() {
        let clock = test_clock();
        let shard = make_shard(clock);

        // Prepare mutation — the outcome that would be broadcast is inside
        // PreparedMutation. If we drop it, no outcome is returned.
        let prepared = shard.prepare_mutation(|s| s.counter = 42, |s| s.clone());

        // Verify the prepared mutation has the view that would have been broadcast
        assert_eq!(prepared.view.counter, 42);

        // Drop it (simulate save failure) — no MutationOutcome returned
        drop(prepared);

        // State unchanged — nothing to broadcast
        assert_eq!(shard.state().generation.age, 0);
    }

    // ── PERSIST-10: Successful save then publish ordering ────────

    #[test]
    fn persist_10_save_then_publish_ordering() {
        let clock = test_clock();
        let mut shard = make_shard(clock);

        // Phase 1: prepare (state not yet changed)
        let prepared = shard.prepare_mutation(|s| s.counter = 42, |s| s.clone());
        assert_eq!(shard.state().generation.age, 0, "state unchanged before commit");
        assert_eq!(
            shard.get_view(&NodeId(1)).unwrap().value.counter, 0,
            "view unchanged before commit"
        );

        // Simulate: persist succeeds here
        let persisted_state = prepared.state().clone();
        assert_eq!(persisted_state.generation.age, 1);

        // Phase 2: commit (state + view updated atomically)
        let outcome = shard.commit_mutation(prepared);
        assert_eq!(shard.state().generation.age, 1, "state updated after commit");
        assert_eq!(
            shard.get_view(&NodeId(1)).unwrap().value.counter, 42,
            "view updated after commit"
        );
        assert_eq!(outcome.generation.age, 1);
        assert_eq!(outcome.view.counter, 42);
    }

    // ── Delta-aware two-phase mutation tests ─────────────────────

    #[test]
    fn persist_delta_prepare_commit_cycle() {
        let clock = test_clock();
        let mut shard = make_delta_shard(clock);

        let prepared = shard.prepare_mutation_with_delta(
            |s| {
                s.counter += 1;
                s.label = "updated".into();
                TestDeltaChange {
                    counter_delta: 1,
                    new_label: Some("updated".into()),
                    accumulator_delta: 0.0,
                }
            },
            |s| TestDeltaState::project_view(s),
            |dc| TestDeltaState::project_delta(dc),
        );

        assert_eq!(shard.state().generation.age, 0, "unchanged before commit");

        let outcome = shard.commit_delta_mutation(prepared);
        assert_eq!(shard.state().generation.age, 1);
        assert_eq!(outcome.generation.age, 1);
        assert_eq!(outcome.view.counter, 1);
    }

    #[test]
    fn persist_delta_save_failure_rollback() {
        let clock = test_clock();
        let mut shard = make_delta_shard(clock);

        // Commit one mutation successfully
        let p1 = shard.prepare_mutation_with_delta(
            |s| {
                s.counter += 1;
                TestDeltaChange {
                    counter_delta: 1,
                    new_label: None,
                    accumulator_delta: 0.0,
                }
            },
            |s| TestDeltaState::project_view(s),
            |dc| TestDeltaState::project_delta(dc),
        );
        shard.commit_delta_mutation(p1);
        assert_eq!(shard.state().generation.age, 1);

        // Prepare second mutation but "save fails"
        let p2 = shard.prepare_mutation_with_delta(
            |s| {
                s.counter += 10;
                TestDeltaChange {
                    counter_delta: 10,
                    new_label: None,
                    accumulator_delta: 0.0,
                }
            },
            |s| TestDeltaState::project_view(s),
            |dc| TestDeltaState::project_delta(dc),
        );
        drop(p2);

        // State unchanged — still at age 1 with counter=1
        assert_eq!(shard.state().generation.age, 1);
        assert_eq!(shard.state().value.counter, 1);
    }
}
