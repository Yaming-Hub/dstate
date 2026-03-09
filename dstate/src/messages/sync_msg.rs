use crate::types::node::NodeId;

/// Outbound messages from StateShard to SyncEngine.
#[derive(Debug)]
pub(crate) enum SyncEngineMsg {
    /// Broadcast a full snapshot to all peers.
    OutboundSnapshot {
        state_name: String,
        incarnation: u64,
        age: u64,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// Broadcast a delta to all peers.
    OutboundDelta {
        state_name: String,
        incarnation: u64,
        from_age: u64,
        to_age: u64,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// Request a full snapshot from a specific peer.
    PullSnapshot {
        state_name: String,
        target: NodeId,
    },
}
