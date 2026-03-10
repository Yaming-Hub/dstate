use crate::types::node::{NodeId, Generation};

/// Outbound messages from StateShard to SyncEngine.
#[derive(Debug)]
pub(crate) enum SyncEngineMsg {
    /// Broadcast a full snapshot to all peers.
    OutboundSnapshot {
        state_name: String,
        generation: Generation,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// Broadcast a delta to all peers. The delta advances views
    /// from `generation.age - 1` to `generation.age`.
    OutboundDelta {
        state_name: String,
        generation: Generation,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// Request a full snapshot from a specific peer.
    PullSnapshot {
        state_name: String,
        target: NodeId,
    },
}
