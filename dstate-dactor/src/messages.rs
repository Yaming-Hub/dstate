//! Message types for the `StateActor`.
//!
//! These messages are local-only (closures are not serializable). For
//! cross-node communication, the actor routes `SyncMessage`/`WireMessage`
//! over the wire via dactor's remote messaging.

use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;

use dactor::message::Message;
use dstate::engine::{EngineQueryResult, WireMessage};
use dstate::{NodeId, StateViewObject, SyncMessage, SyncUrgency};

// ── Mutation ────────────────────────────────────────────────────

/// Fire-and-forget: apply a mutation on this node.
pub struct Mutate<S: Send + 'static, V: Clone + Send + Sync + Debug + 'static> {
    pub mutate_fn: Box<dyn FnOnce(&mut S) + Send>,
    pub project_fn: Box<dyn FnOnce(&S) -> V + Send>,
    pub urgency: SyncUrgency,
}

impl<S: Send + 'static, V: Clone + Send + Sync + Debug + 'static> Message for Mutate<S, V> {
    type Reply = ();
}

/// Fire-and-forget: apply a delta-aware mutation on this node.
pub struct MutateDelta<S, V, VD, DC>
where
    S: Send + 'static,
    V: Clone + Send + Sync + Debug + 'static,
    VD: Clone + Send + Sync + Debug + 'static,
    DC: Send + 'static,
{
    pub mutate_fn: Box<dyn FnOnce(&mut S) -> DC + Send>,
    pub project_view_fn: Box<dyn FnOnce(&S) -> V + Send>,
    pub project_delta_fn: Box<dyn FnOnce(&DC) -> VD + Send>,
    pub urgency: SyncUrgency,
    pub serialize_delta: Box<dyn FnOnce(&VD) -> Vec<u8> + Send>,
}

impl<S, V, VD, DC> Message for MutateDelta<S, V, VD, DC>
where
    S: Send + 'static,
    V: Clone + Send + Sync + Debug + 'static,
    VD: Clone + Send + Sync + Debug + 'static,
    DC: Send + 'static,
{
    type Reply = ();
}

// ── Inbound sync ────────────────────────────────────────────────

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

// ── Query ───────────────────────────────────────────────────────

/// Request-reply: query the view map on this node.
pub struct QueryViews<V: Clone + Send + Sync + Debug + 'static> {
    pub max_staleness: Duration,
    _marker: std::marker::PhantomData<V>,
}

impl<V: Clone + Send + Sync + Debug + 'static> QueryViews<V> {
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

impl<V: Clone + Send + Sync + Debug + 'static> Message for QueryViews<V> {
    type Reply = QueryReply<V>;
}

// ── Cluster membership ──────────────────────────────────────────

/// Fire-and-forget: notify this actor of a cluster membership change.
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

// ── Timer-driven operations ─────────────────────────────────────

/// Fire-and-forget: trigger periodic sync.
pub struct PeriodicSync;

impl Message for PeriodicSync {
    type Reply = ();
}

/// Fire-and-forget: flush pending change feed notifications.
pub struct FlushChangeFeed;

impl Message for FlushChangeFeed {
    type Reply = ();
}
