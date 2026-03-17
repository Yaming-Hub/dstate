//! Mock cluster orchestrator for deterministic integration testing.
//!
//! `MockCluster` manages a set of [`MockNode`]s, each containing a
//! [`DistributedStateEngine`], and a [`MockTransport`] for routing
//! serialized messages between them.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use dstate::engine::{DistributedStateEngine, EngineAction, WireMessage};
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::TestState;
use dstate::{DeserializeError, DistributedState, SyncMessage};
use dstate::{NodeId, StateConfig, SyncUrgency};

use crate::transport::MockTransport;

/// A node in the mock cluster containing a `DistributedStateEngine`.
pub struct MockNode<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    pub engine: DistributedStateEngine<S, V>,
    /// Pending scheduled actions (from `EngineAction::ScheduleDelayed`).
    scheduled: Vec<ScheduledAction>,
    /// Ticks since last change-feed flush.
    feed_flush_counter: u64,
    /// Change-feed flush interval in ticks (0 = disabled).
    feed_flush_interval: u64,
}

struct ScheduledAction {
    message: SyncMessage,
    deliver_at_tick: u64,
}

/// Mock cluster orchestrator for deterministic integration testing.
///
/// The cluster manages nodes (each with a `DistributedStateEngine`),
/// a `MockTransport` for byte-level message routing, and a shared
/// `TestClock` for deterministic time.
///
/// # Tick Phase Ordering
///
/// Each `tick()` follows a strict order:
/// 1. Deliver transport queue messages due at this tick
/// 2. Process engine actions from delivery (may generate more messages)
/// 3. Evaluate scheduled timer events
/// 4. Flush change feeds (if interval has elapsed)
/// 5. Advance clock by tick duration
pub struct MockCluster<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    nodes: BTreeMap<NodeId, MockNode<S, V>>,
    transport: MockTransport,
    clock: Arc<TestClock>,
    tick_duration: Duration,
    current_tick: u64,
}

impl<S, V> MockCluster<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    /// Create a new empty cluster.
    ///
    /// - `clock`: Shared test clock (used by all engines).
    /// - `tick_duration`: How much time each tick advances the clock.
    /// - `rng_seed`: Seed for deterministic transport behavior.
    pub fn new(clock: Arc<TestClock>, tick_duration: Duration, rng_seed: u64) -> Self {
        Self {
            nodes: BTreeMap::new(),
            transport: MockTransport::new(rng_seed),
            clock,
            tick_duration,
            current_tick: 0,
        }
    }

    /// Add a pre-built engine as a node in the cluster.
    ///
    /// The node is immediately announced to all existing nodes via
    /// `on_node_joined`, and existing nodes are announced to the new node.
    pub fn add_node(&mut self, engine: DistributedStateEngine<S, V>, default_view: V)
    where
        V: Clone,
    {
        let new_id = engine.node_id();

        // Announce new node to all existing nodes; collect actions first
        let existing_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        let mut all_actions: Vec<(NodeId, Vec<EngineAction>)> = Vec::new();
        for &existing_id in &existing_ids {
            let node = self.nodes.get_mut(&existing_id).unwrap();
            let actions = node.engine.on_node_joined(new_id, default_view.clone());
            if !actions.is_empty() {
                all_actions.push((existing_id, actions));
            }
        }
        for (node_id, actions) in all_actions {
            self.route_actions(node_id, &actions);
        }

        // Insert the new node
        let feed_interval = engine
            .sync_strategy()
            .change_feed
            .batch_interval
            .as_millis() as u64
            / self.tick_duration.as_millis().max(1) as u64;

        let mut new_node = MockNode {
            engine,
            scheduled: Vec::new(),
            feed_flush_counter: 0,
            feed_flush_interval: feed_interval.max(1),
        };

        // Announce existing nodes to the new node; collect actions first
        let mut new_node_actions: Vec<Vec<EngineAction>> = Vec::new();
        for &existing_id in &existing_ids {
            let actions = new_node
                .engine
                .on_node_joined(existing_id, default_view.clone());
            if !actions.is_empty() {
                new_node_actions.push(actions);
            }
        }

        self.nodes.insert(new_id, new_node);

        // Route new node's actions after insertion
        for actions in new_node_actions {
            self.route_actions(new_id, &actions);
        }
    }

    /// Remove a node from the cluster (simulates crash).
    pub fn remove_node(&mut self, node_id: NodeId) {
        self.nodes.remove(&node_id);
        // Announce departure to remaining nodes
        for (_, node) in &mut self.nodes {
            node.engine.on_node_left(node_id);
        }
    }

    /// Process one simulation tick.
    pub fn tick(&mut self) {
        // Phase 1: Deliver transport messages due at this tick
        let delivery = self.transport.deliver_due();
        let mut pending_actions: Vec<(NodeId, Vec<EngineAction>)> = Vec::new();
        for msg in delivery.delivered {
            if let Some(node) = self.nodes.get_mut(&msg.to) {
                let actions = node.engine.handle_inbound(msg.message);
                if !actions.is_empty() {
                    pending_actions.push((msg.to, actions));
                }
            }
        }
        // Route actions outside the node borrow
        for (node_id, actions) in pending_actions {
            self.route_actions(node_id, &actions);
        }
        // Corrupted messages are silently dropped (logged in real systems)

        // Phase 2: Evaluate scheduled timer events
        let node_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        for &node_id in &node_ids {
            let node = self.nodes.get_mut(&node_id).unwrap();
            let mut due = Vec::new();
            let mut remaining = Vec::new();
            for action in node.scheduled.drain(..) {
                if action.deliver_at_tick <= self.current_tick {
                    due.push(action);
                } else {
                    remaining.push(action);
                }
            }
            node.scheduled = remaining;

            for action in due {
                let all_nodes = self.all_node_ids();
                self.transport.broadcast(
                    node_id,
                    &WireMessage::Sync(action.message),
                    &all_nodes,
                );
            }
        }

        // Phase 3: Flush change feeds
        for &node_id in &node_ids {
            let node = self.nodes.get_mut(&node_id).unwrap();
            node.feed_flush_counter += 1;
            if node.feed_flush_interval > 0
                && node.feed_flush_counter >= node.feed_flush_interval
            {
                node.feed_flush_counter = 0;
                if let Some(feed) = node.engine.flush_change_feed() {
                    let all_nodes = self.all_node_ids();
                    self.transport
                        .broadcast(node_id, &WireMessage::Feed(feed), &all_nodes);
                }
            }
        }

        // Phase 4: Advance clock and tick counter
        self.clock.advance(self.tick_duration);
        self.transport.advance_tick();
        self.current_tick += 1;
    }

    /// Tick until no messages are in flight and no scheduled actions are
    /// pending. Returns the number of ticks executed.
    ///
    /// Panics if `max_ticks` is exceeded (likely an infinite loop).
    pub fn settle(&mut self) -> u64 {
        self.settle_with_limit(1000)
    }

    /// Tick until settled, with a custom limit.
    pub fn settle_with_limit(&mut self, max_ticks: u64) -> u64 {
        let mut ticks = 0;
        loop {
            if self.is_settled() {
                return ticks;
            }
            assert!(
                ticks < max_ticks,
                "settle exceeded {max_ticks} ticks — possible infinite loop"
            );
            self.tick();
            ticks += 1;
        }
    }

    /// Whether the cluster has no in-flight messages or pending actions.
    pub fn is_settled(&self) -> bool {
        if self.transport.in_flight_count() > 0 {
            return false;
        }
        // Check for pending scheduled actions
        for (_, node) in &self.nodes {
            if !node.scheduled.is_empty() {
                return false;
            }
            if node.engine.pending_change_feed_count() > 0 {
                return false;
            }
        }
        true
    }

    /// Mutate state on a specific node.
    pub fn mutate(
        &mut self,
        node_id: NodeId,
        mutate_fn: impl FnOnce(&mut S),
        project_fn: impl FnOnce(&S) -> V,
        urgency: SyncUrgency,
    ) {
        let node = self
            .nodes
            .get_mut(&node_id)
            .unwrap_or_else(|| panic!("node {node_id} not in cluster"));

        let result = node.engine.mutate(mutate_fn, project_fn, urgency);
        let actions = result.actions;
        self.route_actions(node_id, &actions);
    }

    /// Get a reference to a node's engine.
    pub fn engine(&self, node_id: NodeId) -> &DistributedStateEngine<S, V> {
        &self.nodes[&node_id].engine
    }

    /// Get a mutable reference to a node's engine.
    pub fn engine_mut(&mut self, node_id: NodeId) -> &mut DistributedStateEngine<S, V> {
        &mut self.nodes.get_mut(&node_id).unwrap().engine
    }

    /// All node IDs in the cluster, sorted.
    pub fn all_node_ids(&self) -> Vec<NodeId> {
        self.nodes.keys().copied().collect()
    }

    /// Number of nodes in the cluster.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of messages currently in flight.
    pub fn in_flight_count(&self) -> usize {
        self.transport.in_flight_count()
    }

    /// Current simulation tick.
    pub fn current_tick(&self) -> u64 {
        self.current_tick
    }

    /// Access the transport (e.g., to add interceptors).
    pub fn transport_mut(&mut self) -> &mut MockTransport {
        &mut self.transport
    }

    // ── Internal ────────────────────────────────────────────────

    fn route_actions(&mut self, from: NodeId, actions: &[EngineAction]) {
        let all_nodes = self.all_node_ids();
        for action in actions {
            match action {
                EngineAction::BroadcastSync(msg) => {
                    self.transport
                        .broadcast(from, &WireMessage::Sync(msg.clone()), &all_nodes);
                }
                EngineAction::SendSync { target, message } => {
                    self.transport
                        .send(from, *target, &WireMessage::Sync(message.clone()));
                }
                EngineAction::ScheduleDelayed { delay, message } => {
                    let delay_ticks =
                        (delay.as_millis() as u64) / (self.tick_duration.as_millis().max(1) as u64);
                    if let Some(node) = self.nodes.get_mut(&from) {
                        node.scheduled.push(ScheduledAction {
                            message: message.clone(),
                            deliver_at_tick: self.current_tick + delay_ticks,
                        });
                    }
                }
            }
        }
    }
}

// ── Convenience factory for TestState clusters ──────────────────

impl MockCluster<TestState, TestState> {
    /// Create a cluster of `n` nodes using `TestState` (simple state).
    ///
    /// Each node gets NodeId(0) through NodeId(n-1).
    pub fn with_test_state(n: usize, config: StateConfig) -> Self {
        let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
        let tick_duration = Duration::from_millis(100);
        let mut cluster = Self::new(clock.clone(), tick_duration, 42);

        for i in 0..n {
            let engine = DistributedStateEngine::new(
                TestState::name(),
                NodeId(i as u64),
                TestState::default(),
                |s| s.clone(),
                config.clone(),
                TestState::WIRE_VERSION,
                clock.clone(),
                |v| bincode::serialize(v).unwrap(),
                |b, v| {
                    if v != TestState::WIRE_VERSION {
                        return Err(DeserializeError::unknown_version(v));
                    }
                    bincode::deserialize(b)
                        .map_err(|e| DeserializeError::Malformed(e.to_string()))
                },
                None,
            );
            cluster.add_node(engine, TestState::default());
        }

        cluster
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> StateConfig {
        StateConfig::default() // ActivePush
    }

    #[test]
    fn create_cluster_with_test_state() {
        let cluster = MockCluster::with_test_state(3, default_config());
        assert_eq!(cluster.node_count(), 3);

        // Each node should see 3 views (self + 2 peers)
        for id in 0..3 {
            assert_eq!(cluster.engine(NodeId(id)).view_count(), 3);
        }
    }

    #[test]
    fn mutate_and_settle_propagates_to_all() {
        let mut cluster = MockCluster::with_test_state(3, default_config());

        // Mutate on node 0
        cluster.mutate(
            NodeId(0),
            |s| {
                s.counter = 42;
                s.label = "hello".into();
            },
            |s| s.clone(),
            SyncUrgency::Default,
        );

        // Settle to propagate
        let ticks = cluster.settle();
        assert!(ticks > 0, "should need at least one tick");

        // All nodes should see node 0's update
        for id in 0..3 {
            let view = cluster.engine(NodeId(id)).get_view(&NodeId(0)).unwrap();
            assert_eq!(view.value.counter, 42);
            assert_eq!(view.value.label, "hello");
        }
    }

    #[test]
    fn multiple_mutations_converge() {
        let mut cluster = MockCluster::with_test_state(3, default_config());

        // Each node mutates its own state
        for i in 0..3 {
            cluster.mutate(
                NodeId(i),
                |s| {
                    s.counter = (i + 1) * 10;
                    s.label = format!("node{i}");
                },
                |s| s.clone(),
                SyncUrgency::Default,
            );
        }

        cluster.settle();

        // Each node should see all 3 unique states
        for observer in 0..3u64 {
            for origin in 0..3u64 {
                let view = cluster
                    .engine(NodeId(observer))
                    .get_view(&NodeId(origin))
                    .unwrap();
                assert_eq!(view.value.counter, (origin + 1) * 10);
                assert_eq!(view.value.label, format!("node{origin}"));
            }
        }
    }

    #[test]
    fn node_removal_stops_message_delivery() {
        let mut cluster = MockCluster::with_test_state(3, default_config());
        cluster.settle();

        // Remove node 2
        cluster.remove_node(NodeId(2));
        assert_eq!(cluster.node_count(), 2);

        // Mutate on node 0 — should only reach node 1
        cluster.mutate(
            NodeId(0),
            |s| s.counter = 99,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        cluster.settle();

        let view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
        assert_eq!(view.value.counter, 99);
        // Node 2 is gone, so no assertion there
    }

    #[test]
    fn network_partition_prevents_propagation() {
        use crate::interceptor::Partition;

        let mut cluster = MockCluster::with_test_state(3, default_config());
        cluster.settle();

        // Partition: node 0 cannot reach node 2 (symmetric)
        cluster
            .transport_mut()
            .add_interceptor(Box::new(Partition::symmetric([NodeId(0)], [NodeId(2)])));

        // Node 0 mutates
        cluster.mutate(
            NodeId(0),
            |s| s.counter = 77,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        cluster.settle();

        // Node 1 should see the update (not partitioned)
        let v1 = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
        assert_eq!(v1.value.counter, 77);

        // Node 2 should NOT see the update (partitioned from node 0)
        let v2 = cluster.engine(NodeId(2)).get_view(&NodeId(0)).unwrap();
        assert_eq!(v2.value.counter, 0); // still default
    }

    #[test]
    fn partition_heal_allows_convergence() {
        use crate::interceptor::Partition;

        let mut cluster = MockCluster::with_test_state(2, default_config());
        cluster.settle();

        // Partition
        cluster
            .transport_mut()
            .add_interceptor(Box::new(Partition::symmetric([NodeId(0)], [NodeId(1)])));

        // Mutate during partition
        cluster.mutate(
            NodeId(0),
            |s| s.counter = 100,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        cluster.settle();

        // Node 1 doesn't see it
        let v = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
        assert_eq!(v.value.counter, 0);

        // Heal partition
        cluster.transport_mut().clear_interceptors();

        // Mutate again — this broadcasts a new snapshot
        cluster.mutate(
            NodeId(0),
            |s| s.counter = 200,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        cluster.settle();

        // Node 1 should now see the latest value
        let v = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
        assert_eq!(v.value.counter, 200);
    }

    #[test]
    fn settle_returns_tick_count() {
        let mut cluster = MockCluster::with_test_state(2, default_config());

        cluster.mutate(
            NodeId(0),
            |s| s.counter = 1,
            |s| s.clone(),
            SyncUrgency::Default,
        );

        let ticks = cluster.settle();
        // With ActivePush and 2 nodes, it should settle quickly
        assert!(ticks >= 1 && ticks <= 10);
    }

    #[test]
    fn duplicate_messages_handled_idempotently() {
        use crate::interceptor::DuplicateAll;

        let mut cluster = MockCluster::with_test_state(2, default_config());
        cluster.settle();

        // Enable duplication
        cluster
            .transport_mut()
            .add_interceptor(Box::new(DuplicateAll));

        // Mutate
        cluster.mutate(
            NodeId(0),
            |s| s.counter = 42,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        cluster.settle();

        // Node 1 should have the correct value despite receiving duplicates
        let v = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
        assert_eq!(v.value.counter, 42);
    }
}
