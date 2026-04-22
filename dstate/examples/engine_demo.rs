//! Multi-node engine demo — shows `DistributedStateEngine` driving replication.
//!
//! This example creates 3 in-process engines (simulating 3 nodes) and manually
//! routes sync messages between them. It demonstrates:
//!
//! 1. Creating engines with `DistributedState` types
//! 2. Mutating state and routing the resulting `EngineAction`s
//! 3. Querying the cluster-wide view map
//! 4. Handling node join/leave events
//! 5. Periodic sync and change feed flushing
//!
//! In a real system, the routing would be done by an actor shell (see
//! `dstate-integration/src/shell.rs` for an example using dactor-mock).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dstate::engine::{DistributedStateEngine, EngineAction, EngineQueryResult, WireMessage};
use dstate::{
    DeserializeError, DistributedState, NodeId, StateConfig, SyncStrategy, SyncUrgency,
};

// ── Simple counter state ────────────────────────────────────────

/// A minimal distributed counter for demonstration.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Counter {
    value: u64,
    label: String,
}

impl DistributedState for Counter {
    fn name() -> &'static str {
        "counter"
    }
    const WIRE_VERSION: u32 = 1;
    const STORAGE_VERSION: u32 = 1;

    fn serialize_state(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }
    fn deserialize_state(bytes: &[u8], _version: u32) -> Result<Self, DeserializeError> {
        bincode::deserialize(bytes).map_err(|e| DeserializeError::Malformed(e.to_string()))
    }
}

// ── Cluster simulation ──────────────────────────────────────────

/// A simple in-process cluster that manually routes engine actions.
struct InProcessCluster {
    engines: HashMap<NodeId, DistributedStateEngine<Counter, Counter>>,
}

impl InProcessCluster {
    fn new(node_count: usize, config: StateConfig) -> Self {
        let clock: Arc<dyn dstate::Clock> = Arc::new(dstate::SystemClock);

        let mut engines = HashMap::new();
        let mut ids = Vec::new();

        // Create engines
        for i in 0..node_count {
            let id = NodeId(format!("node-{i}"));
            let engine = DistributedStateEngine::new(
                Counter::name(),
                id.clone(),
                Counter::default(),
                |s| s.clone(),
                config.clone(),
                Counter::WIRE_VERSION,
                clock.clone(),
                |v| bincode::serialize(v).unwrap(),
                |b, v| {
                    if v != Counter::WIRE_VERSION {
                        return Err(DeserializeError::unknown_version(v));
                    }
                    bincode::deserialize(b)
                        .map_err(|e| DeserializeError::Malformed(e.to_string()))
                },
                None,
            );
            engines.insert(id.clone(), engine);
            ids.push(id);
        }

        // Announce all nodes to each other
        for i in 0..ids.len() {
            for j in 0..ids.len() {
                if i != j {
                    let peer_id = ids[j].clone();
                    let engine = engines.get_mut(&ids[i]).unwrap();
                    engine.on_node_joined(peer_id, Counter::default());
                }
            }
        }

        Self { engines }
    }

    /// Mutate a node's state and route resulting actions to peers.
    fn mutate(&mut self, node_id: &NodeId, f: impl FnOnce(&mut Counter)) {
        let result = self.engines.get_mut(node_id).unwrap().mutate(
            f,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        self.route_actions(node_id.clone(), result.actions);
    }

    /// Deliver all pending actions (one round of message passing).
    fn route_actions(&mut self, from: NodeId, actions: Vec<EngineAction>) {
        // Collect messages to deliver (can't mutate engines while iterating)
        let mut deliveries: Vec<(NodeId, WireMessage)> = Vec::new();

        let all_ids: Vec<NodeId> = self.engines.keys().cloned().collect();
        for action in actions {
            match action {
                EngineAction::BroadcastSync(msg) => {
                    for peer in &all_ids {
                        if peer != &from {
                            deliveries.push((peer.clone(), WireMessage::Sync(msg.clone())));
                        }
                    }
                }
                EngineAction::SendSync { target, message } => {
                    deliveries.push((target, WireMessage::Sync(message)));
                }
                EngineAction::ScheduleDelayed { message, .. } => {
                    // In a real system this would be a timer; here we deliver immediately
                    for peer in &all_ids {
                        if peer != &from {
                            deliveries.push((
                                peer.clone(),
                                WireMessage::Sync(message.clone()),
                            ));
                        }
                    }
                }
            }
        }

        // Deliver messages and recursively route any responses
        for (target, wire_msg) in deliveries {
            let response_actions = self
                .engines
                .get_mut(&target)
                .unwrap()
                .handle_inbound(wire_msg);

            if !response_actions.is_empty() {
                self.route_actions(target, response_actions);
            }
        }
    }

    /// Query the view map from a node's perspective.
    fn query(&self, node_id: &NodeId) -> HashMap<NodeId, u64> {
        let (result, _actions) = self.engines[node_id].query(
            Duration::from_secs(60),
            |views| {
                views
                    .iter()
                    .map(|(id, vo)| (id.clone(), vo.value.value))
                    .collect::<HashMap<_, _>>()
            },
        );
        match result {
            EngineQueryResult::Fresh(map) => map,
            EngineQueryResult::Stale { result: map, .. } => map,
        }
    }
}

// ── Main ────────────────────────────────────────────────────────

fn main() {
    println!("=== dstate Engine Demo ===\n");

    // 1. Create a 3-node cluster with active-push sync
    let config = StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    };
    let mut cluster = InProcessCluster::new(3, config);
    println!("✓ Created 3-node cluster with active-push sync strategy");

    // 2. Mutate state on node-0
    let n0 = NodeId("node-0".to_string());
    let n1 = NodeId("node-1".to_string());
    let n2 = NodeId("node-2".to_string());

    cluster.mutate(&n0, |s| {
        s.value = 42;
        s.label = "hello from node-0".to_string();
    });
    println!("✓ Mutated node-0: value=42, label='hello from node-0'");

    // 3. Query from each node — all should see node-0's value
    for node in [&n0, &n1, &n2] {
        let views = cluster.query(node);
        println!("  {node} sees: {views:?}");
    }

    // 4. Mutate on all nodes
    cluster.mutate(&n1, |s| {
        s.value = 100;
        s.label = "node-1 reporting".to_string();
    });
    cluster.mutate(&n2, |s| {
        s.value = 200;
        s.label = "node-2 active".to_string();
    });
    println!("\n✓ All nodes mutated");

    // 5. Query cluster-wide view from node-0
    let views = cluster.query(&n0);
    println!("  node-0 cluster view: {views:?}");
    assert_eq!(views[&n0], 42);
    assert_eq!(views[&n1], 100);
    assert_eq!(views[&n2], 200);

    // 6. Node departure
    cluster.engines.get_mut(&n0).unwrap().on_node_left(n2.clone());
    cluster.engines.get_mut(&n1).unwrap().on_node_left(n2.clone());
    cluster.engines.remove(&n2);
    println!("\n✓ node-2 departed");

    let views = cluster.query(&n0);
    println!("  node-0 cluster view after departure: {views:?}");
    assert!(!views.contains_key(&n2), "departed node should be removed");

    // 7. Show metrics
    let metrics = cluster.engines[&n0].metrics();
    println!("\n✓ node-0 metrics:");
    println!("  mutations: {}", metrics.total_mutations);
    println!("  snapshots sent: {}", metrics.snapshots_sent);

    // 8. Health check
    let health = cluster.engines[&n0].health(Duration::from_secs(30));
    println!("\n✓ node-0 health: {:?}", health.status);

    println!("\n=== Demo complete ===");
}
