use std::time::Instant;

use crate::types::node::NodeId;

/// The local node's authoritative copy of a distributed state.
#[derive(Debug, Clone)]
pub struct StateObject<S> {
    /// Monotonically increasing mutation counter within an incarnation.
    pub age: u64,
    /// Unique lifetime identifier; changes on crash-restart without persistence.
    pub incarnation: u64,
    /// The storage format version used when this state was persisted.
    pub storage_version: u32,
    /// The state value.
    pub value: S,
    /// Wall-clock Unix milliseconds when the state was first created.
    pub created_time: i64,
    /// Wall-clock Unix milliseconds of the last mutation.
    pub modified_time: i64,
}

/// A peer node's replicated view of a distributed state.
#[derive(Debug, Clone)]
pub struct StateViewObject<V> {
    /// The peer's mutation counter at the time this view was sent.
    pub age: u64,
    /// The peer's incarnation at the time this view was sent.
    pub incarnation: u64,
    /// The wire protocol version used to serialize this view.
    pub wire_version: u32,
    /// The view value.
    pub value: V,
    /// Wall-clock Unix milliseconds when the state was first created on the peer.
    pub created_time: i64,
    /// Wall-clock Unix milliseconds of the last mutation on the peer.
    pub modified_time: i64,
    /// Monotonic instant when this view was last synchronized.
    pub synced_at: Instant,
    /// If set, the peer has announced a higher age via a change feed
    /// notification, meaning this view is known to be stale.
    pub pending_remote_age: Option<u64>,
    /// The node that owns this view.
    pub source_node: NodeId,
}
