use serde::{Deserialize, Serialize};

use crate::types::node::{NodeId, Generation};

/// Wire-level synchronization messages exchanged between nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncMessage {
    /// A complete snapshot of a peer's view.
    FullSnapshot {
        state_name: String,
        source_node: NodeId,
        generation: Generation,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// An incremental delta update. The delta advances the view
    /// from `generation.age - 1` to `generation.age`.
    DeltaUpdate {
        state_name: String,
        source_node: NodeId,
        generation: Generation,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// Request a full snapshot from the owner.
    RequestSnapshot {
        state_name: String,
        requester: NodeId,
    },
}

/// A notification that a state has changed on a node (used by the change feed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeNotification {
    pub state_name: String,
    pub source_node: NodeId,
    pub generation: Generation,
}

/// A batched collection of change notifications broadcast by the aggregator.
/// No target node is specified because this message is broadcast to all nodes in the cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchedChangeFeed {
    pub source_node: NodeId,
    pub notifications: Vec<ChangeNotification>,
}
