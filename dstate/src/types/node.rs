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

/// Logical version of a state, used for ordering and staleness detection.
///
/// Ordered lexicographically: `(incarnation, age)`. A higher incarnation
/// always wins regardless of age; within the same incarnation a higher
/// age wins.
///
/// ```text
/// (inc=2, age=0) > (inc=1, age=999)   // new incarnation wins
/// (inc=1, age=5) > (inc=1, age=4)     // same incarnation, newer age
/// (inc=1, age=5) == (inc=1, age=5)    // duplicate
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Generation {
    /// Unique lifetime identifier; changes on crash-restart without persistence.
    pub incarnation: u64,
    /// Monotonically increasing mutation counter within an incarnation.
    pub age: u64,
}

impl Generation {
    /// Create a new version.
    pub fn new(incarnation: u64, age: u64) -> Self {
        Self { incarnation, age }
    }

    /// The initial version for a brand-new state (incarnation 0, age 0).
    pub fn zero() -> Self {
        Self {
            incarnation: 0,
            age: 0,
        }
    }

    /// Whether this is a newer incarnation than `other` (owner restarted).
    pub fn is_new_incarnation(&self, other: &Self) -> bool {
        self.incarnation > other.incarnation
    }
}

impl PartialOrd for Generation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Generation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.incarnation
            .cmp(&other.incarnation)
            .then(self.age.cmp(&other.age))
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}:{}", self.incarnation, self.age)
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
