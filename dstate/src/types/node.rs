use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for a node in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({})", self.0)
    }
}

/// Policy for handling wire version mismatches on inbound sync messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionMismatchPolicy {
    /// Keep the last successfully deserialized view; log a warning.
    KeepStale,
    /// Remove the peer's view from the PublicViewMap until a compatible
    /// version arrives.
    DropAndWait,
}
