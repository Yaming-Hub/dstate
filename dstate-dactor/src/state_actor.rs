//! `StateActor` — dactor actor wrapper for [`DistributedStateEngine`].
//!
//! This actor owns a `DistributedStateEngine<S, V>` and bridges its
//! action-based API with dactor's message-passing model.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, Handler};
use dactor::ActorRef;
use dactor::NodeId;
use tokio_util::sync::CancellationToken;

use dstate::engine::{
    DistributedStateEngine, EngineAction, EngineQueryResult, WireMessage,
};
use dstate::{SyncMessage, SyncUrgency};

use crate::messages::*;

// ── Peer sender abstraction ─────────────────────────────────────

/// Trait for sending sync messages to a peer actor.
///
/// This abstracts over `ActorRef<StateActor>` so that routing logic
/// doesn't need to be generic over the concrete ref type. Adapters
/// create `PeerSender` impls wrapping their ActorRef.
pub trait PeerSender: Send + Sync + 'static {
    /// Send a sync message (fire-and-forget).
    fn send_sync(&self, message: SyncMessage);

    /// Send a wire message (fire-and-forget).
    fn send_wire(&self, message: WireMessage);
}

/// A `PeerSender` backed by a dactor `ActorRef<StateActor<S, V>>`.
pub struct ActorRefPeerSender<S, V, R>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
    R: ActorRef<StateActor<S, V>>,
{
    actor_ref: R,
    _marker: std::marker::PhantomData<(S, V)>,
}

impl<S, V, R> ActorRefPeerSender<S, V, R>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
    R: ActorRef<StateActor<S, V>>,
{
    pub fn new(actor_ref: R) -> Self {
        Self {
            actor_ref,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<S, V, R> PeerSender for ActorRefPeerSender<S, V, R>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
    R: ActorRef<StateActor<S, V>>,
{
    fn send_sync(&self, message: SyncMessage) {
        let _ = self.actor_ref.tell(InboundSync { message });
    }

    fn send_wire(&self, message: WireMessage) {
        let _ = self.actor_ref.tell(InboundWire { message });
    }
}

// ── Peer registry ───────────────────────────────────────────────

/// Thread-safe registry of peer senders, keyed by NodeId.
pub type PeerRegistry = Arc<std::sync::Mutex<HashMap<NodeId, Box<dyn PeerSender>>>>;

/// Create an empty peer registry.
pub fn new_peer_registry() -> PeerRegistry {
    Arc::new(std::sync::Mutex::new(HashMap::new()))
}

// ── Construction args ───────────────────────────────────────────

/// Construction arguments for `StateActor`.
pub struct StateActorArgs<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    /// The pre-built engine to wrap.
    pub engine: DistributedStateEngine<S, V>,
    /// Shared peer registry for routing actions.
    pub peers: PeerRegistry,
}

// ── StateActor ──────────────────────────────────────────────────

/// A dactor actor that wraps a [`DistributedStateEngine`].
///
/// Routes `EngineAction`s to peers, handles cluster membership changes,
/// and processes inbound sync/wire messages. Timer-based periodic sync
/// and change feed flushing are bootstrapped externally via
/// [`send_interval`](dactor::timer::send_interval) after spawning.
pub struct StateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    engine: DistributedStateEngine<S, V>,
    peers: PeerRegistry,
    cancel_token: CancellationToken,
}

impl<S, V> StateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    /// Access the underlying engine (for metrics, health, etc.).
    pub fn engine(&self) -> &DistributedStateEngine<S, V> {
        &self.engine
    }

    /// Route engine actions to peer actors.
    fn route_actions(&self, actions: Vec<EngineAction>) {
        let peers = self.peers.lock().unwrap();
        let own_id = self.engine.node_id();

        for action in actions {
            match action {
                EngineAction::BroadcastSync(msg) => {
                    for (peer_id, sender) in peers.iter() {
                        if *peer_id != own_id {
                            sender.send_sync(msg.clone());
                        }
                    }
                }
                EngineAction::SendSync { target, message } => {
                    if let Some(sender) = peers.get(&target) {
                        sender.send_sync(message);
                    }
                }
                EngineAction::ScheduleDelayed { delay, message } => {
                    let peers_clone = self.peers.clone();
                    let own_id_clone = own_id.clone();
                    let token = self.cancel_token.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {
                                let peers = peers_clone.lock().unwrap();
                                for (peer_id, sender) in peers.iter() {
                                    if *peer_id != own_id_clone {
                                        sender.send_sync(message.clone());
                                    }
                                }
                            }
                            _ = token.cancelled() => {}
                        }
                    });
                }
            }
        }
    }
}

// ── Actor impl ──────────────────────────────────────────────────

#[async_trait]
impl<S, V> Actor for StateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    type Args = StateActorArgs<S, V>;
    type Deps = ();

    fn create(args: Self::Args, _deps: ()) -> Self {
        Self {
            engine: args.engine,
            peers: args.peers,
            cancel_token: CancellationToken::new(),
        }
    }

    async fn on_stop(&mut self) {
        self.cancel_token.cancel();
    }
}

// ── Handlers ────────────────────────────────────────────────────

#[async_trait]
impl<S, V> Handler<Mutate<S, V>> for StateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, msg: Mutate<S, V>, _ctx: &mut ActorContext) {
        let result = self
            .engine
            .mutate(msg.mutate_fn, msg.project_fn, msg.urgency);
        self.route_actions(result.actions);
    }
}

#[async_trait]
impl<S, V> Handler<InboundSync> for StateActor<S, V>
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
impl<S, V> Handler<InboundWire> for StateActor<S, V>
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
impl<S, V> Handler<QueryViews<V>> for StateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, msg: QueryViews<V>, _ctx: &mut ActorContext) -> QueryReply<V> {
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
impl<S, V> Handler<ClusterChange<V>> for StateActor<S, V>
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
                self.engine.on_node_left(node_id.clone());
                let mut peers = self.peers.lock().unwrap();
                peers.remove(&node_id);
            }
        }
    }
}

#[async_trait]
impl<S, V> Handler<PeriodicSync> for StateActor<S, V>
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
impl<S, V> Handler<FlushChangeFeed> for StateActor<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    async fn handle(&mut self, _msg: FlushChangeFeed, _ctx: &mut ActorContext) {
        if let Some(feed) = self.engine.flush_change_feed() {
            let peers = self.peers.lock().unwrap();
            let own_id = self.engine.node_id();
            for (peer_id, sender) in peers.iter() {
                if *peer_id != own_id {
                    sender.send_wire(WireMessage::Feed(feed.clone()));
                }
            }
        }
    }
}
