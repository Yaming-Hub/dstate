//! Actor shell that wraps [`DistributedStateEngine`] as a dactor actor.
//!
//! This module provides [`DstateActor`], a thin shell that bridges the
//! engine's action-based API (`Vec<EngineAction>`) with dactor's
//! message-passing model. It is designed for use with `dactor-mock`
//! functional tests.
//!
//! The shell:
//! - Owns a `DistributedStateEngine<S, V>`
//! - Dispatches `EngineAction`s to peer actors via `TestActorRef`
//! - Checks `MockNetwork::can_deliver()` before routing messages
//! - Handles cluster membership changes (NodeJoined/NodeLeft)

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, Handler};
use dactor::message::Message;
use dactor::test_support::test_runtime::TestActorRef;
use dactor::ActorRef;
use dactor::NodeId;
use dactor_mock::MockNetwork;

use dstate::engine::{
    DistributedStateEngine, EngineAction, EngineQueryResult, WireMessage,
};
use dstate::StateViewObject;
use dstate::{SyncMessage, SyncUrgency};

// ── Shared peer registry ────────────────────────────────────────

/// Shared registry of peer actor refs, keyed by NodeId.
/// Wrapped in `Arc<Mutex<_>>` so it can be shared with cloned refs.
pub type PeerRegistry<S, V> = Arc<Mutex<HashMap<NodeId, TestActorRef<DstateActor<S, V>>>>>;

// ── Messages ────────────────────────────────────────────────────

/// Fire-and-forget: apply a mutation on this node.
pub struct Mutate<S: Send + 'static, V: Clone + Send + Sync + Debug + 'static> {
    pub mutate_fn: Box<dyn FnOnce(&mut S) + Send>,
    pub project_fn: Box<dyn FnOnce(&S) -> V + Send>,
    pub urgency: SyncUrgency,
}

impl<S: Send + 'static, V: Clone + Send + Sync + Debug + 'static> Message for Mutate<S, V> {
    type Reply = ();
}

/// Fire-and-forget: deliver an inbound sync message from a peer.
pub struct InboundSync {
    pub message: SyncMessage,
}

impl Message for InboundSync {
    type Reply = ();
}

/// Fire-and-forget: deliver an inbound wire message (sync or feed).
pub struct InboundWire {
    pub message: WireMessage,
}

impl Message for InboundWire {
    type Reply = ();
}

/// Request-reply: query the view map on this node.
pub struct QueryAll<V: Clone + Send + Sync + Debug + 'static> {
    pub max_staleness: Duration,
    _marker: std::marker::PhantomData<V>,
}

impl<V: Clone + Send + Sync + Debug + 'static> QueryAll<V> {
    pub fn new(max_staleness: Duration) -> Self {
        Self {
            max_staleness,
            _marker: std::marker::PhantomData,
        }
    }
}

/// Query reply: the full view map snapshot.
pub struct QueryReply<V: Clone + Send + Sync + Debug + 'static> {
    pub views: HashMap<NodeId, StateViewObject<V>>,
    pub result: EngineQueryResult<()>,
}

impl<V: Clone + Send + Sync + Debug + 'static> Message for QueryAll<V> {
    type Reply = QueryReply<V>;
}

/// Fire-and-forget: notify this node of a cluster membership change.
pub enum ClusterChange<V: Clone + Send + Sync + Debug + 'static> {
    NodeJoined {
        node_id: NodeId,
        default_view: V,
    },
    NodeLeft {
        node_id: NodeId,
    },
}

impl<V: Clone + Send + Sync + Debug + 'static> Message for ClusterChange<V> {
    type Reply = ();
}

/// Request-reply: trigger periodic sync on this node.
pub struct PeriodicSync;

impl Message for PeriodicSync {
    type Reply = ();
}

/// Request-reply: flush change feed on this node.
pub struct FlushChangeFeed;

impl Message for FlushChangeFeed {
    type Reply = ();
}

// ── Actor ───────────────────────────────────────────────────────

/// Construction arguments for DstateActor.
pub struct DstateActorArgs<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    pub engine: DistributedStateEngine<S, V>,
    pub peers: PeerRegistry<S, V>,
    pub network: Arc<MockNetwork>,
    pub own_node_id: NodeId,
}

/// A dactor actor that wraps a `DistributedStateEngine`.
pub struct DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    engine: DistributedStateEngine<S, V>,
    peers: PeerRegistry<S, V>,
    network: Arc<MockNetwork>,
    own_node_id: NodeId,
}

impl<S, V> DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    /// Route engine actions to peer actors, respecting network partitions.
    fn route_actions(&self, actions: Vec<EngineAction>) {
        let peers = self.peers.lock().unwrap();
        for action in actions {
            match action {
                EngineAction::BroadcastSync(msg) => {
                    for (peer_id, peer_ref) in peers.iter() {
                        if peer_id == &self.own_node_id {
                            continue;
                        }
                        if !self.network.can_deliver(&self.own_node_id, peer_id) {
                            continue;
                        }
                        let _ = peer_ref.tell(InboundSync {
                            message: msg.clone(),
                        });
                    }
                }
                EngineAction::SendSync { target, message } => {
                    if self.network.can_deliver(&self.own_node_id, &target) {
                        if let Some(peer_ref) = peers.get(&target) {
                            let _ = peer_ref.tell(InboundSync { message });
                        }
                    }
                }
                EngineAction::ScheduleDelayed { delay, message } => {
                    // For simplicity in mock tests, broadcast immediately
                    // after delay via tokio::spawn. In production, this would
                    // use dactor's timer facility.
                    let peers_clone = self.peers.clone();
                    let network = self.network.clone();
                    let own_id = self.own_node_id.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let peers = peers_clone.lock().unwrap();
                        for (peer_id, peer_ref) in peers.iter() {
                            if peer_id == &own_id {
                                continue;
                            }
                            if !network.can_deliver(&own_id, peer_id) {
                                continue;
                            }
                            let _ = peer_ref.tell(InboundSync {
                                message: message.clone(),
                            });
                        }
                    });
                }
            }
        }
    }
}

#[async_trait]
impl<S, V> Actor for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    type Args = DstateActorArgs<S, V>;
    type Deps = ();

    fn create(args: Self::Args, _deps: ()) -> Self {
        Self {
            engine: args.engine,
            peers: args.peers,
            network: args.network,
            own_node_id: args.own_node_id,
        }
    }
}

// ── Handlers ────────────────────────────────────────────────────

#[async_trait]
impl<S, V> Handler<Mutate<S, V>> for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, msg: Mutate<S, V>, _ctx: &mut ActorContext) {
        let result = self.engine.mutate(msg.mutate_fn, msg.project_fn, msg.urgency);
        self.route_actions(result.actions);
    }
}

#[async_trait]
impl<S, V> Handler<InboundSync> for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, msg: InboundSync, _ctx: &mut ActorContext) {
        let actions = self.engine.handle_inbound_sync(msg.message);
        self.route_actions(actions);
    }
}

#[async_trait]
impl<S, V> Handler<InboundWire> for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, msg: InboundWire, _ctx: &mut ActorContext) {
        let actions = self.engine.handle_inbound(msg.message);
        self.route_actions(actions);
    }
}

#[async_trait]
impl<S, V> Handler<QueryAll<V>> for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, msg: QueryAll<V>, _ctx: &mut ActorContext) -> QueryReply<V> {
        let (result, actions) = self.engine.query(msg.max_staleness, |view_map| {
            view_map.clone()
        });
        self.route_actions(actions);

        let (views, result) = match result {
            EngineQueryResult::Fresh(views) => (views, EngineQueryResult::Fresh(())),
            EngineQueryResult::Stale {
                result: views,
                stale_peers,
            } => (
                views,
                EngineQueryResult::Stale {
                    result: (),
                    stale_peers,
                },
            ),
        };

        QueryReply { views, result }
    }
}

#[async_trait]
impl<S, V> Handler<ClusterChange<V>> for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, msg: ClusterChange<V>, _ctx: &mut ActorContext) {
        match msg {
            ClusterChange::NodeJoined {
                node_id,
                default_view,
            } => {
                let actions = self.engine.on_node_joined(node_id, default_view);
                self.route_actions(actions);
            }
            ClusterChange::NodeLeft { node_id } => {
                self.engine.on_node_left(node_id);
            }
        }
    }
}

#[async_trait]
impl<S, V> Handler<PeriodicSync> for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, _msg: PeriodicSync, _ctx: &mut ActorContext) {
        let actions = self.engine.periodic_sync();
        self.route_actions(actions);
    }
}

#[async_trait]
impl<S, V> Handler<FlushChangeFeed> for DstateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, _msg: FlushChangeFeed, _ctx: &mut ActorContext) {
        if let Some(feed) = self.engine.flush_change_feed() {
            let peers = self.peers.lock().unwrap();
            for (peer_id, peer_ref) in peers.iter() {
                if peer_id == &self.own_node_id {
                    continue;
                }
                if !self.network.can_deliver(&self.own_node_id, peer_id) {
                    continue;
                }
                let _ = peer_ref.tell(InboundWire {
                    message: WireMessage::Feed(feed.clone()),
                });
            }
        }
    }
}

// ── Test helpers ────────────────────────────────────────────────

/// Helper to build a DstateActor-based test cluster using dactor-mock.
pub struct DstateCluster<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    pub cluster: dactor_mock::MockCluster,
    pub actors: HashMap<String, TestActorRef<DstateActor<S, V>>>,
    pub peers: PeerRegistry<S, V>,
}
