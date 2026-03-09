use serde::{Deserialize, Serialize};

use crate::types::node::NodeId;

/// Wire-level synchronization messages exchanged between nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncMessage {
    /// A complete snapshot of a peer's view.
    FullSnapshot {
        state_name: String,
        source_node: NodeId,
        incarnation: u64,
        age: u64,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// An incremental delta update.
    DeltaUpdate {
        state_name: String,
        source_node: NodeId,
        incarnation: u64,
        from_age: u64,
        to_age: u64,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// Request a full snapshot from the owner.
    RequestSnapshot {
        state_name: String,
        requester: NodeId,
        min_required_age: Option<u64>,
    },
}

/// A notification that a state has changed on a node (used by the change feed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeNotification {
    pub state_name: String,
    pub source_node: NodeId,
    pub incarnation: u64,
    pub age: u64,
}

/// A batched collection of change notifications broadcast by the aggregator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchedChangeFeed {
    pub source_node: NodeId,
    pub notifications: Vec<ChangeNotification>,
}
