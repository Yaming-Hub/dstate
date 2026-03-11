use std::collections::HashSet;
use std::fmt::Debug;

use crate::core::shard_core::ShardCore;
use crate::types::node::NodeId;

/// Action the actor shell should take after a node joins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JoinAction {
    /// Send the local node's current snapshot to the new peer so it
    /// can populate its view map.
    SendSnapshotToPeer {
        target: NodeId,
    },
}

/// Action the actor shell should take after a node leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaveAction {
    /// The peer has been removed from the view map.
    /// Discard any in-flight messages from this node.
    PeerRemoved {
        node_id: NodeId,
    },
}

/// Pure logic for handling cluster lifecycle events (node join, leave,
/// crash-restart).
///
/// # Threading model
///
/// This struct is intended for single-actor use. It is owned exclusively by
/// a `StateShard` actor and accessed sequentially within that actor's
/// message loop — no internal locking is needed.
///
/// # Responsibilities
///
/// - Track departed nodes to prevent ghost re-creation of removed views
///   when late in-flight messages arrive.
/// - Return actions for the actor shell (send snapshots, discard messages).
/// - Coordinate with `ShardCore` for view map updates.
///
/// # Data Flow
///
/// ```text
/// ClusterEvent::NodeJoined(peer)
///   └─ LifecycleLogic.on_node_joined(shard, peer, default_view)
///        ├─ ShardCore.on_node_joined(peer, default_view)
///        └─ returns JoinAction::SendSnapshotToPeer { target: peer }
///
/// ClusterEvent::NodeLeft(peer)
///   └─ LifecycleLogic.on_node_left(shard, peer)
///        ├─ ShardCore.on_node_left(peer)
///        ├─ adds peer to departed set
///        └─ returns LeaveAction::PeerRemoved { node_id: peer }
///
/// Inbound sync message from peer
///   └─ LifecycleLogic.should_accept_from(peer) → bool
///        └─ false if peer is in departed set
/// ```
#[allow(dead_code)]
pub(crate) struct LifecycleLogic {
    /// Nodes that have been removed via `on_node_left` and have not yet
    /// re-joined via `on_node_joined`.
    ///
    /// After `on_node_left` removes a peer from the view map, late
    /// in-flight snapshots from that peer could re-create its view map
    /// entry (because `accept_inbound_snapshot` accepts when no existing
    /// entry is found). This set lets `should_accept_from` reject those
    /// stale messages. The entry is cleared when the peer re-joins
    /// (i.e., `on_node_joined` is called again).
    departed_nodes: HashSet<NodeId>,
}

#[allow(dead_code)]
impl LifecycleLogic {
    pub fn new() -> Self {
        Self {
            departed_nodes: HashSet::new(),
        }
    }

    /// Handle a node joining the cluster.
    ///
    /// Updates the view map via `ShardCore` and clears the node from the
    /// departed set (in case of re-join after a previous leave).
    ///
    /// Returns the action for the actor shell to execute.
    pub fn on_node_joined<S, V>(
        &mut self,
        shard: &ShardCore<S, V>,
        node_id: NodeId,
        default_view: V,
    ) -> Vec<JoinAction>
    where
        S: Clone + Send + Sync + 'static,
        V: Clone + Send + Sync + Debug + 'static,
    {
        self.departed_nodes.remove(&node_id);
        shard.on_node_joined(node_id, default_view);

        vec![JoinAction::SendSnapshotToPeer { target: node_id }]
    }

    /// Handle a node leaving the cluster.
    ///
    /// Removes the node from the view map and adds it to the departed set
    /// so that late in-flight messages are filtered.
    pub fn on_node_left<S, V>(
        &mut self,
        shard: &ShardCore<S, V>,
        node_id: NodeId,
    ) -> LeaveAction
    where
        S: Clone + Send + Sync + 'static,
        V: Clone + Send + Sync + Debug + 'static,
    {
        shard.on_node_left(node_id);
        self.departed_nodes.insert(node_id);

        LeaveAction::PeerRemoved { node_id }
    }

    /// Check whether inbound messages from a node should be accepted.
    ///
    /// Returns `false` if the node has departed and not re-joined,
    /// preventing ghost re-creation of removed view map entries.
    pub fn should_accept_from(&self, node_id: &NodeId) -> bool {
        !self.departed_nodes.contains(node_id)
    }

    /// Check if a node is currently tracked as departed.
    pub fn is_departed(&self, node_id: &NodeId) -> bool {
        self.departed_nodes.contains(node_id)
    }

    /// Number of nodes currently in the departed set.
    pub fn departed_count(&self) -> usize {
        self.departed_nodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::shard_core::{
        new_state_object, AcceptResult, DeltaAcceptResult, InboundSnapshot, QueryResult,
    };
    use crate::test_support::test_clock::TestClock;
    use crate::test_support::test_state::TestState;
    use crate::traits::clock::Clock;
    use crate::types::node::Generation;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_clock() -> Arc<dyn Clock> {
        Arc::new(TestClock::with_base_unix_ms(1_000_000))
    }

    /// Create a ShardCore for a given node with a counter-based TestState.
    fn make_shard(
        node_id: NodeId,
        clock: Arc<dyn Clock>,
    ) -> ShardCore<TestState, TestState> {
        let state = new_state_object(
            TestState { counter: 0, label: format!("node_{}", node_id.0) },
            clock.unix_ms() as u64,
            clock.as_ref(),
        );
        let view = state.value.clone();
        ShardCore::new(node_id, state, view, clock)
    }

    /// Simulate one node sending a snapshot to another.
    fn send_snapshot(
        from: &ShardCore<TestState, TestState>,
        to: &ShardCore<TestState, TestState>,
    ) -> AcceptResult {
        let state = from.state();
        let view = from.get_view(&from.node_id()).unwrap();
        to.accept_inbound_snapshot(InboundSnapshot {
            source: from.node_id(),
            generation: state.generation,
            wire_version: state.storage_version,
            view: view.value.clone(),
            created_time: state.created_time,
            modified_time: state.modified_time,
        })
    }

    // ── LIFE-01: New node receives snapshots from all existing nodes ──

    #[test]
    fn life_01_new_node_receives_snapshots() {
        let clock = test_clock();
        let a = make_shard(NodeId(1), clock.clone());
        let b = make_shard(NodeId(2), clock.clone());
        let c = make_shard(NodeId(3), clock.clone());
        let d = make_shard(NodeId(4), clock.clone());

        // D joins — add placeholders for existing nodes
        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&d, NodeId(1), TestState { counter: 0, label: "".into() });
        lifecycle.on_node_joined(&d, NodeId(2), TestState { counter: 0, label: "".into() });
        lifecycle.on_node_joined(&d, NodeId(3), TestState { counter: 0, label: "".into() });

        // Existing nodes send snapshots to D
        assert_eq!(send_snapshot(&a, &d), AcceptResult::Accepted);
        assert_eq!(send_snapshot(&b, &d), AcceptResult::Accepted);
        assert_eq!(send_snapshot(&c, &d), AcceptResult::Accepted);

        // D now has views for A, B, C (plus self = 4 total)
        assert_eq!(d.view_count(), 4);

        let snap = d.snapshot();
        assert!(snap.contains_key(&NodeId(1)));
        assert!(snap.contains_key(&NodeId(2)));
        assert!(snap.contains_key(&NodeId(3)));
        assert!(snap.contains_key(&NodeId(4)));
    }

    // ── LIFE-02: Existing nodes receive snapshot from new node ────────

    #[test]
    fn life_02_existing_nodes_receive_new_node_snapshot() {
        let clock = test_clock();
        let a = make_shard(NodeId(1), clock.clone());
        let b = make_shard(NodeId(2), clock.clone());
        let d = make_shard(NodeId(4), clock.clone());

        let mut lifecycle = LifecycleLogic::new();

        // A and B learn about D joining
        let actions_a = lifecycle.on_node_joined(&a, NodeId(4), TestState { counter: 0, label: "".into() });
        let actions_b = lifecycle.on_node_joined(&b, NodeId(4), TestState { counter: 0, label: "".into() });

        assert_eq!(actions_a, vec![JoinAction::SendSnapshotToPeer { target: NodeId(4) }]);
        assert_eq!(actions_b, vec![JoinAction::SendSnapshotToPeer { target: NodeId(4) }]);

        // D sends its snapshot to A and B
        assert_eq!(send_snapshot(&d, &a), AcceptResult::Accepted);
        assert_eq!(send_snapshot(&d, &b), AcceptResult::Accepted);

        // A and B now have D's view
        assert!(a.snapshot().contains_key(&NodeId(4)));
        assert!(b.snapshot().contains_key(&NodeId(4)));
    }

    // ── LIFE-03: Join with multiple state types ──────────────────────

    #[test]
    fn life_03_join_with_multiple_state_types() {
        let clock = test_clock();

        // Two state types — simulated as two separate ShardCores per node
        let a_state1 = make_shard(NodeId(1), clock.clone());
        let a_state2 = make_shard(NodeId(1), clock.clone());
        let d_state1 = make_shard(NodeId(4), clock.clone());
        let d_state2 = make_shard(NodeId(4), clock.clone());

        let mut lifecycle = LifecycleLogic::new();

        // D joins — both state types add placeholder
        lifecycle.on_node_joined(&a_state1, NodeId(4), TestState { counter: 0, label: "".into() });
        lifecycle.on_node_joined(&a_state2, NodeId(4), TestState { counter: 0, label: "".into() });
        lifecycle.on_node_joined(&d_state1, NodeId(1), TestState { counter: 0, label: "".into() });
        lifecycle.on_node_joined(&d_state2, NodeId(1), TestState { counter: 0, label: "".into() });

        // Exchange snapshots for both state types
        assert_eq!(send_snapshot(&a_state1, &d_state1), AcceptResult::Accepted);
        assert_eq!(send_snapshot(&a_state2, &d_state2), AcceptResult::Accepted);
        assert_eq!(send_snapshot(&d_state1, &a_state1), AcceptResult::Accepted);
        assert_eq!(send_snapshot(&d_state2, &a_state2), AcceptResult::Accepted);

        // Both state types synced
        assert_eq!(a_state1.view_count(), 2);
        assert_eq!(a_state2.view_count(), 2);
        assert_eq!(d_state1.view_count(), 2);
        assert_eq!(d_state2.view_count(), 2);
    }

    // ── LIFE-04: Departed node removed from PublicViewMap ─────────────

    #[test]
    fn life_04_departed_node_removed() {
        let clock = test_clock();
        let a = make_shard(NodeId(1), clock.clone());

        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&a, NodeId(2), TestState { counter: 0, label: "".into() });
        lifecycle.on_node_joined(&a, NodeId(3), TestState { counter: 0, label: "".into() });
        assert_eq!(a.view_count(), 3);

        // Node 3 leaves
        let action = lifecycle.on_node_left(&a, NodeId(3));
        assert_eq!(action, LeaveAction::PeerRemoved { node_id: NodeId(3) });

        assert_eq!(a.view_count(), 2);
        assert!(!a.snapshot().contains_key(&NodeId(3)));
        assert!(a.snapshot().contains_key(&NodeId(2)));
    }

    // ── LIFE-05: In-flight sync from departed node discarded ─────────

    #[test]
    fn life_05_inflight_from_departed_discarded() {
        let clock = test_clock();
        let a = make_shard(NodeId(1), clock.clone());
        let c = make_shard(NodeId(3), clock.clone());

        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&a, NodeId(3), TestState { counter: 0, label: "".into() });
        assert_eq!(a.view_count(), 2);

        // C leaves
        lifecycle.on_node_left(&a, NodeId(3));
        assert_eq!(a.view_count(), 1);

        // Late delta from C — should_accept_from returns false
        assert!(!lifecycle.should_accept_from(&NodeId(3)));

        // Even if we tried to accept a snapshot, the actor shell would
        // have filtered it via should_accept_from(). For deltas,
        // ShardCore itself returns UnknownPeer since the entry was removed.
        let delta_result = a.accept_inbound_delta(
            NodeId(3),
            Generation::new(1, 1),
            1,
            |v| v.clone(),
        );
        assert_eq!(delta_result, DeltaAcceptResult::UnknownPeer);
    }

    // ── LIFE-06: Leave with multiple state types ─────────────────────

    #[test]
    fn life_06_leave_multiple_state_types() {
        let clock = test_clock();
        let a_s1 = make_shard(NodeId(1), clock.clone());
        let a_s2 = make_shard(NodeId(1), clock.clone());

        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&a_s1, NodeId(3), TestState { counter: 0, label: "".into() });
        lifecycle.on_node_joined(&a_s2, NodeId(3), TestState { counter: 0, label: "".into() });

        // Node 3 leaves — both state types updated
        lifecycle.on_node_left(&a_s1, NodeId(3));
        lifecycle.on_node_left(&a_s2, NodeId(3));

        assert_eq!(a_s1.view_count(), 1);
        assert_eq!(a_s2.view_count(), 1);
    }

    // ── LIFE-07: Crash restart without persistence ───────────────────

    #[test]
    fn life_07_crash_restart_no_persistence() {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::with_base_unix_ms(1_000_000));

        // A starts at incarnation=1000000, age progresses to 3
        let mut a = make_shard(NodeId(1), clock.clone());
        a.apply_mutation(|s| s.counter += 1, |s| s.clone());
        a.apply_mutation(|s| s.counter += 1, |s| s.clone());
        a.apply_mutation(|s| s.counter += 1, |s| s.clone());
        assert_eq!(a.state().generation.age, 3);

        let old_incarnation = a.state().generation.incarnation;

        // Peer B has A's view at (old_incarnation, 3)
        let b = make_shard(NodeId(2), clock.clone());
        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });
        send_snapshot(&a, &b);

        // A crashes. Advance clock for new incarnation.
        let test_clock = TestClock::with_base_unix_ms(1_000_000);
        test_clock.advance(Duration::from_millis(5000));
        let clock2: Arc<dyn Clock> = Arc::new(test_clock);

        // A restarts with new incarnation (no persistence)
        let a2 = make_shard(NodeId(1), clock2.clone());
        let new_incarnation = a2.state().generation.incarnation;
        assert!(new_incarnation > old_incarnation, "new incarnation should be higher");
        assert_eq!(a2.state().generation.age, 0);

        // B receives A's new snapshot — accepted because incarnation is higher
        // First, simulate NodeJoined for the restarted A
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });
        let result = send_snapshot(&a2, &b);
        assert_eq!(result, AcceptResult::Accepted);
    }

    // ── LIFE-08: Crash restart with persistence ──────────────────────

    #[test]
    fn life_08_crash_restart_with_persistence() {
        let clock = test_clock();

        // A starts, mutates to age 5
        let mut a = make_shard(NodeId(1), clock.clone());
        for _ in 0..5 {
            a.apply_mutation(|s| s.counter += 1, |s| s.clone());
        }
        let saved_state = a.state().clone();
        assert_eq!(saved_state.generation.age, 5);

        // B has A's view
        let b = make_shard(NodeId(2), clock.clone());
        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });
        send_snapshot(&a, &b);

        let b_view = b.get_view(&NodeId(1)).unwrap();
        assert_eq!(b_view.generation.age, 5);

        // A crashes — NodeLeft fires first
        lifecycle.on_node_left(&b, NodeId(1));
        assert_eq!(b.view_count(), 1);

        // A restarts with persisted state (same incarnation, same age)
        let a2 = ShardCore::new(
            NodeId(1),
            saved_state.clone(),
            saved_state.value.clone(),
            clock.clone(),
        );
        assert_eq!(a2.state().generation, saved_state.generation);

        // NodeJoined fires → creates placeholder (generation::zero)
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });

        // B receives snapshot from restarted A — accepted (generation > zero)
        let result = send_snapshot(&a2, &b);
        assert_eq!(result, AcceptResult::Accepted);

        let b_view = b.get_view(&NodeId(1)).unwrap();
        assert_eq!(b_view.generation.age, 5);
        assert_eq!(b_view.value.counter, 5);
    }

    // ── LIFE-09: Fast restart before NodeLeft detected ───────────────

    #[test]
    fn life_09_fast_restart_before_node_left() {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::with_base_unix_ms(1_000_000));

        let mut a = make_shard(NodeId(1), clock.clone());
        a.apply_mutation(|s| s.counter = 10, |s| s.clone());
        let old_gen = a.state().generation;

        let b = make_shard(NodeId(2), clock.clone());
        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });
        send_snapshot(&a, &b);

        // A crashes and restarts immediately (NodeLeft never fires)
        let test_clock = TestClock::with_base_unix_ms(1_000_000);
        test_clock.advance(Duration::from_millis(1));
        let clock2: Arc<dyn Clock> = Arc::new(test_clock);
        let a2 = make_shard(NodeId(1), clock2.clone());

        // NodeJoined fires for restarted A (no NodeLeft in between)
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });

        // B accepts A's new snapshot (new incarnation > old)
        let result = send_snapshot(&a2, &b);
        assert_eq!(result, AcceptResult::Accepted);

        let b_view = b.get_view(&NodeId(1)).unwrap();
        assert!(b_view.generation.incarnation > old_gen.incarnation
            || b_view.generation.incarnation == a2.state().generation.incarnation);
    }

    // ── LIFE-10: Late NodeLeft after rejoin ──────────────────────────

    #[test]
    fn life_10_late_node_left_after_rejoin() {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::with_base_unix_ms(1_000_000));

        let a = make_shard(NodeId(1), clock.clone());
        let b = make_shard(NodeId(2), clock.clone());
        let mut lifecycle = LifecycleLogic::new();

        // Normal setup: B knows about A
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });
        send_snapshot(&a, &b);
        assert_eq!(b.view_count(), 2);

        // A crashes, restarts with new incarnation
        let test_clock = TestClock::with_base_unix_ms(1_000_000);
        test_clock.advance(Duration::from_millis(100));
        let clock2: Arc<dyn Clock> = Arc::new(test_clock);
        let a2 = make_shard(NodeId(1), clock2);

        // NodeJoined fires for restarted A
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });
        send_snapshot(&a2, &b);
        assert_eq!(b.view_count(), 2);

        // Late NodeLeft arrives
        lifecycle.on_node_left(&b, NodeId(1));
        assert_eq!(b.view_count(), 1);
        assert!(!b.snapshot().contains_key(&NodeId(1)));

        // A is still alive — in real system, another NodeJoined would fire.
        // Simulate: NodeJoined clears departed set, view re-created.
        lifecycle.on_node_joined(&b, NodeId(1), TestState { counter: 0, label: "".into() });
        assert!(lifecycle.should_accept_from(&NodeId(1)));
        send_snapshot(&a2, &b);
        assert_eq!(b.view_count(), 2);
        assert!(b.snapshot().contains_key(&NodeId(1)));
    }

    // ── LIFE-11: Partitioned peer views become stale ─────────────────

    #[test]
    fn life_11_partitioned_peers_stale() {
        let test_clock = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(test_clock.clone());

        let a = make_shard(NodeId(1), clock.clone());
        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&a, NodeId(3), TestState { counter: 0, label: "".into() });

        // Simulate C sending initial snapshot
        let c = make_shard(NodeId(3), clock.clone());
        send_snapshot(&c, &a);

        // Simulate partition: advance clock, C can't sync
        test_clock.advance(Duration::from_secs(30));

        // Query with tight freshness
        let stale = a.stale_peers(Duration::from_secs(10));
        assert!(stale.contains(&NodeId(3)), "C should be stale after partition");
    }

    // ── LIFE-12: Views recover after partition heals ─────────────────

    #[test]
    fn life_12_partition_recovery() {
        let test_clock = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(test_clock.clone());

        let a = make_shard(NodeId(1), clock.clone());
        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&a, NodeId(3), TestState { counter: 0, label: "".into() });

        let c = make_shard(NodeId(3), clock.clone());
        send_snapshot(&c, &a);

        // Partition: advance clock
        test_clock.advance(Duration::from_secs(30));
        assert!(!a.stale_peers(Duration::from_secs(10)).is_empty());

        // Partition heals: C sends fresh snapshot
        let mut c2 = make_shard(NodeId(3), clock.clone());
        c2.apply_mutation(|s| s.counter = 42, |s| s.clone());
        send_snapshot(&c2, &a);

        // A's view of C is now fresh
        assert!(a.stale_peers(Duration::from_secs(10)).is_empty());
        let c_view = a.get_view(&NodeId(3)).unwrap();
        assert_eq!(c_view.value.counter, 42);
    }

    // ── LIFE-13: Query with loose freshness during partition ─────────

    #[test]
    fn life_13_loose_freshness_during_partition() {
        let test_clock = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(test_clock.clone());

        let a = make_shard(NodeId(1), clock.clone());
        let mut lifecycle = LifecycleLogic::new();
        lifecycle.on_node_joined(&a, NodeId(3), TestState { counter: 0, label: "".into() });

        let c = make_shard(NodeId(3), clock.clone());
        send_snapshot(&c, &a);

        // Partition: 30s of no sync
        test_clock.advance(Duration::from_secs(30));

        // Tight freshness (10s) → stale
        let stale = a.stale_peers(Duration::from_secs(10));
        assert!(!stale.is_empty());

        // Loose freshness (60s) → not stale, query succeeds
        use crate::core::shard_core::QueryResult;
        let result = a.query_local(Duration::from_secs(60), |views| {
            views.len()
        });
        match result {
            QueryResult::Fresh(count) => assert_eq!(count, 2),
            _ => panic!("expected Fresh with loose freshness"),
        }
    }
}
