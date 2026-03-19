use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dstate::engine::{
    DistributedStateEngine, EngineAction, EngineQueryResult, SyncMetrics, WireMessage,
};
use dstate::StateViewObject;
use dstate::test_support::test_state::TestState;
use dstate::{
    ActorRef, ClusterEvent, DeserializeError, DistributedState, NodeId, StateConfig, SyncUrgency,
    SystemClock,
};
use tokio::sync::{mpsc, oneshot};

use crate::transport::InProcessTransport;
use crate::TestableRuntime;

/// Global counter for unique actor names (avoids ractor registry collisions).
static ACTOR_COUNTER: AtomicU64 = AtomicU64::new(0);

// ── Commands sent to node actors ────────────────────────────────────

pub enum NodeCommand {
    Mutate {
        mutate_fn: Box<dyn FnOnce(&mut TestState) + Send>,
        project_fn: Box<dyn FnOnce(&TestState) -> TestState + Send>,
        urgency: SyncUrgency,
        reply: oneshot::Sender<()>,
    },
    Query {
        reply: oneshot::Sender<HashMap<NodeId, StateViewObject<TestState>>>,
    },
    QueryFresh {
        max_staleness: Duration,
        reply: oneshot::Sender<EngineQueryResult<HashMap<NodeId, StateViewObject<TestState>>>>,
    },
    HandleInbound {
        wire_bytes: Vec<u8>,
    },
    OnNodeJoined {
        node_id: NodeId,
        reply: oneshot::Sender<()>,
    },
    OnNodeLeft {
        node_id: NodeId,
    },
    GetMetrics {
        reply: oneshot::Sender<SyncMetrics>,
    },
    PeriodicSync,
    FlushChangeFeed,
}

// ── Per-node handle ─────────────────────────────────────────────────

struct NodeHandle<R: TestableRuntime> {
    actor_ref: R::Ref<NodeCommand>,
    #[allow(dead_code)]
    node_id: NodeId,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

// ── Route engine actions through the transport ──────────────────────

fn route_actions(transport: &Arc<InProcessTransport>, source: NodeId, actions: &[EngineAction]) {
    for action in actions {
        match action {
            EngineAction::BroadcastSync(msg) => {
                let wire = WireMessage::Sync(msg.clone());
                let bytes = bincode::serialize(&wire).unwrap();
                transport.broadcast(source, bytes);
            }
            EngineAction::SendSync { target, message } => {
                let wire = WireMessage::Sync(message.clone());
                let bytes = bincode::serialize(&wire).unwrap();
                transport.send(source, *target, bytes);
            }
            EngineAction::ScheduleDelayed { delay, message } => {
                let wire = WireMessage::Sync(message.clone());
                let bytes = bincode::serialize(&wire).unwrap();
                let transport = Arc::clone(transport);
                let delay = *delay;
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    transport.broadcast(source, bytes);
                });
            }
        }
    }
}

// ── TestCluster ─────────────────────────────────────────────────────

/// A cluster of real actor-backed nodes for E2E testing.
pub struct TestCluster<R: TestableRuntime> {
    runtime: R,
    transport: Arc<InProcessTransport>,
    nodes: HashMap<NodeId, NodeHandle<R>>,
    config: StateConfig,
}

impl<R: TestableRuntime> TestCluster<R> {
    /// Create a cluster with `node_count` nodes, all interconnected.
    pub async fn new(node_count: u64, config: StateConfig) -> Self {
        let runtime = R::new_for_testing();
        let transport = Arc::new(InProcessTransport::new());
        let mut cluster = Self {
            runtime,
            transport,
            nodes: HashMap::new(),
            config,
        };
        for i in 0..node_count {
            cluster.add_node(NodeId(i)).await;
        }
        cluster
    }

    /// Add a new node to the cluster, notifying all existing nodes.
    pub async fn add_node(&mut self, node_id: NodeId) {
        // Build engine
        let engine = DistributedStateEngine::new(
            TestState::name(),
            node_id,
            TestState::default(),
            |s: &TestState| s.clone(),
            self.config.clone(),
            TestState::WIRE_VERSION,
            Arc::new(SystemClock),
            |v: &TestState| bincode::serialize(v).unwrap(),
            |b: &[u8], _v: u32| {
                bincode::deserialize(b)
                    .map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        );

        // Inbound channel
        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        self.transport.register(node_id, inbound_tx);

        // Spawn state actor
        let transport_for_actor = Arc::clone(&self.transport);
        let mut engine = engine;
        let unique_id = ACTOR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let actor_ref = self.runtime.spawn(
            &format!("e2e-node-{}-{}", node_id.0, unique_id),
            move |cmd: NodeCommand| {
                match cmd {
                    NodeCommand::Mutate {
                        mutate_fn,
                        project_fn,
                        urgency,
                        reply,
                    } => {
                        let result = engine.mutate(mutate_fn, project_fn, urgency);
                        route_actions(&transport_for_actor, node_id, &result.actions);
                        let _ = reply.send(());
                    }
                    NodeCommand::Query { reply } => {
                        let snapshot = engine.snapshot();
                        let _ = reply.send(snapshot);
                    }
                    NodeCommand::QueryFresh {
                        max_staleness,
                        reply,
                    } => {
                        let (result, actions) =
                            engine.query(max_staleness, |m| m.clone());
                        route_actions(&transport_for_actor, node_id, &actions);
                        let _ = reply.send(result);
                    }
                    NodeCommand::HandleInbound { wire_bytes } => {
                        match bincode::deserialize::<WireMessage>(&wire_bytes) {
                            Ok(msg) => {
                                let actions = engine.handle_inbound(msg);
                                route_actions(&transport_for_actor, node_id, &actions);
                            }
                            Err(_) => { /* corrupted message — ignore */ }
                        }
                    }
                    NodeCommand::OnNodeJoined {
                        node_id: joined_id,
                        reply,
                    } => {
                        let actions =
                            engine.on_node_joined(joined_id, TestState::default());
                        route_actions(&transport_for_actor, node_id, &actions);
                        let _ = reply.send(());
                    }
                    NodeCommand::OnNodeLeft {
                        node_id: left_id,
                    } => {
                        engine.on_node_left(left_id);
                    }
                    NodeCommand::GetMetrics { reply } => {
                        let _ = reply.send(engine.metrics());
                    }
                    NodeCommand::PeriodicSync => {
                        let actions = engine.periodic_sync();
                        route_actions(&transport_for_actor, node_id, &actions);
                    }
                    NodeCommand::FlushChangeFeed => {
                        if let Some(feed) = engine.flush_change_feed() {
                            let wire = WireMessage::Feed(feed);
                            let bytes = bincode::serialize(&wire).unwrap();
                            transport_for_actor.broadcast(node_id, bytes);
                        }
                    }
                }
            },
        );

        // Background task: inbound receiver → actor
        let actor_ref_for_inbound = actor_ref.clone();
        let inbound_task = tokio::spawn(async move {
            while let Some(wire_bytes) = inbound_rx.recv().await {
                let _ = actor_ref_for_inbound
                    .send(NodeCommand::HandleInbound { wire_bytes });
            }
        });

        let mut tasks = vec![inbound_task];

        // Background task: periodic sync (if configured)
        if let Some(interval) = self.config.sync_strategy.periodic_full_sync {
            let actor_ref_ps = actor_ref.clone();
            let ps_task = tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.tick().await; // first tick fires immediately — skip
                loop {
                    ticker.tick().await;
                    if actor_ref_ps.send(NodeCommand::PeriodicSync).is_err() {
                        break;
                    }
                }
            });
            tasks.push(ps_task);
        }

        // Background task: change-feed flush (if ActiveFeedLazyPull)
        if matches!(
            self.config.sync_strategy.push_mode,
            Some(dstate::PushMode::ActiveFeedLazyPull)
        ) {
            let batch_interval = self.config.sync_strategy.change_feed.batch_interval;
            let actor_ref_cf = actor_ref.clone();
            let cf_task = tokio::spawn(async move {
                let mut ticker = tokio::time::interval(batch_interval);
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    if actor_ref_cf.send(NodeCommand::FlushChangeFeed).is_err() {
                        break;
                    }
                }
            });
            tasks.push(cf_task);
        }

        // Notify new node about all existing nodes first
        let existing_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        for existing_id in &existing_ids {
            let (tx, rx) = oneshot::channel();
            actor_ref
                .send(NodeCommand::OnNodeJoined {
                    node_id: *existing_id,
                    reply: tx,
                })
                .expect("send OnNodeJoined to new node failed");
            rx.await.expect("OnNodeJoined reply from new node failed");
        }

        // Notify existing nodes about the new node
        for existing_id in &existing_ids {
            let existing = self.nodes.get(existing_id).unwrap();
            let (tx, rx) = oneshot::channel();
            existing
                .actor_ref
                .send(NodeCommand::OnNodeJoined {
                    node_id,
                    reply: tx,
                })
                .expect("send OnNodeJoined to existing node failed");
            rx.await
                .expect("OnNodeJoined reply from existing node failed");
        }

        self.nodes.insert(
            node_id,
            NodeHandle {
                actor_ref,
                node_id,
                tasks,
            },
        );

        // Emit cluster event
        self.runtime
            .emit_cluster_event(ClusterEvent::NodeJoined(node_id));
    }

    /// Remove a node from the cluster.
    pub fn remove_node(&mut self, node_id: NodeId) {
        if let Some(handle) = self.nodes.remove(&node_id) {
            for task in handle.tasks {
                task.abort();
            }
        }
        self.transport.unregister(node_id);

        // Notify remaining nodes
        for (_, handle) in &self.nodes {
            let _ = handle.actor_ref.send(NodeCommand::OnNodeLeft { node_id });
        }
        self.runtime
            .emit_cluster_event(ClusterEvent::NodeLeft(node_id));
    }

    /// Mutate a node's state.
    pub async fn mutate(
        &self,
        node_id: NodeId,
        mutate_fn: impl FnOnce(&mut TestState) + Send + 'static,
        urgency: SyncUrgency,
    ) {
        let handle = self.nodes.get(&node_id).expect("node not found");
        let (tx, rx) = oneshot::channel();
        handle
            .actor_ref
            .send(NodeCommand::Mutate {
                mutate_fn: Box::new(mutate_fn),
                project_fn: Box::new(|s: &TestState| s.clone()),
                urgency,
                reply: tx,
            })
            .expect("send mutate failed");
        rx.await.expect("mutate reply failed");
    }

    /// Query a node's view of all peers.
    pub async fn query(
        &self,
        node_id: NodeId,
    ) -> HashMap<NodeId, StateViewObject<TestState>> {
        let handle = self.nodes.get(&node_id).expect("node not found");
        let (tx, rx) = oneshot::channel();
        handle
            .actor_ref
            .send(NodeCommand::Query { reply: tx })
            .expect("send query failed");
        rx.await.expect("query reply failed")
    }

    /// Query a node with a freshness requirement.
    pub async fn query_fresh(
        &self,
        node_id: NodeId,
        max_staleness: Duration,
    ) -> EngineQueryResult<HashMap<NodeId, StateViewObject<TestState>>> {
        let handle = self.nodes.get(&node_id).expect("node not found");
        let (tx, rx) = oneshot::channel();
        handle
            .actor_ref
            .send(NodeCommand::QueryFresh {
                max_staleness,
                reply: tx,
            })
            .expect("send query_fresh failed");
        rx.await.expect("query_fresh reply failed")
    }

    /// Get sync metrics for a node.
    pub async fn get_metrics(&self, node_id: NodeId) -> SyncMetrics {
        let handle = self.nodes.get(&node_id).expect("node not found");
        let (tx, rx) = oneshot::channel();
        handle
            .actor_ref
            .send(NodeCommand::GetMetrics { reply: tx })
            .expect("send get_metrics failed");
        rx.await.expect("get_metrics reply failed")
    }

    /// Poll all nodes until `check` returns true for every node's snapshot, or timeout.
    pub async fn wait_for_convergence(
        &self,
        check: impl Fn(&HashMap<NodeId, StateViewObject<TestState>>) -> bool,
        timeout: Duration,
    ) -> bool {
        let start = tokio::time::Instant::now();
        loop {
            let mut all_pass = true;
            for node_id in self.nodes.keys() {
                let snapshot = self.query(*node_id).await;
                if !check(&snapshot) {
                    all_pass = false;
                    break;
                }
            }
            if all_pass {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Poll until `observer` sees `target`'s counter equal to `expected_counter`.
    pub async fn wait_for_value(
        &self,
        observer: NodeId,
        target: NodeId,
        expected_counter: u64,
        timeout: Duration,
    ) -> bool {
        let start = tokio::time::Instant::now();
        loop {
            let snapshot = self.query(observer).await;
            if let Some(view) = snapshot.get(&target) {
                if view.value.counter == expected_counter {
                    return true;
                }
            }
            if start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Access the transport (e.g., for adding interceptors).
    pub fn transport(&self) -> &Arc<InProcessTransport> {
        &self.transport
    }

    /// Current node IDs in the cluster.
    pub fn node_ids(&self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        ids.sort();
        ids
    }

    /// Shut down the cluster, aborting all background tasks.
    pub async fn shutdown(self) {
        for (_, handle) in self.nodes {
            for task in handle.tasks {
                task.abort();
            }
        }
    }
}
