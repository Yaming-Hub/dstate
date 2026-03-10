use std::time::Instant;

use crate::types::node::{NodeId, StateVersion};

/// The local node's authoritative copy of a distributed state.
#[derive(Debug, Clone)]
pub struct StateObject<S> {
    /// The logical version (incarnation + age) of this state.
    pub version: StateVersion,
    /// The storage format version used when this state was persisted.
    pub storage_version: u32,
    /// The state value.
    pub value: S,
    /// Wall-clock Unix milliseconds when the state was first created.
    pub created_time: i64,
    /// Wall-clock Unix milliseconds of the last mutation.
    pub modified_time: i64,
}

impl<S> StateObject<S> {
    /// Shorthand for `self.version.age`.
    pub fn age(&self) -> u64 {
        self.version.age
    }

    /// Shorthand for `self.version.incarnation`.
    pub fn incarnation(&self) -> u64 {
        self.version.incarnation
    }
}

/// A peer node's replicated view of a distributed state.
#[derive(Debug, Clone)]
pub struct StateViewObject<V> {
    /// The logical version (incarnation + age) of this view.
    pub version: StateVersion,
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
    /// If set, the peer has announced a newer version via a change feed
    /// notification, meaning this view is known to be stale.
    pub pending_remote_version: Option<StateVersion>,
    /// The node that owns this view.
    pub source_node: NodeId,
}
