//! Runtime abstractions re-exported from dactor.
//!
//! These types bridge dactor's actor framework abstractions into dstate's
//! replication protocol. They are re-exported at the crate root for
//! convenience.

// Error types
pub use dactor::errors::{ActorSendError, ClusterError, GroupError};

// Cluster events
pub use dactor::cluster::{ClusterEvent, ClusterEvents, SubscriptionId};

// Timer
pub use dactor::timer::TimerHandle;