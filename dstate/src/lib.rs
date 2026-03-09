// ── Traits (what users implement) ────────────────────────────────
pub use traits::state::{DeltaDistributedState, DistributedState, SyncUrgency};
pub use traits::runtime::{
    ActorRef, ActorRuntime, ClusterEvent, ClusterEvents, TimerHandle,
};
pub use traits::persistence::{PersistError, StatePersistence};
pub use traits::clock::{Clock, SystemClock};

// ── Types (what users construct / receive) ──────────────────────
pub use types::envelope::{StateObject, StateViewObject};
pub use types::config::{ChangeFeedConfig, PushMode, StateConfig, SyncStrategy};
pub use types::node::{NodeId, VersionMismatchPolicy};
pub use types::errors::{
    DeserializeError, MutationError, QueryError, RegistryError,
};
pub use traits::runtime::{
    ActorSendError, ClusterError, GroupError, SubscriptionId,
};
pub use types::sync_message::{BatchedChangeFeed, ChangeNotification, SyncMessage};

// ── Test support (public for adapter crates and downstream tests) ─
pub mod test_support;

// ── Internal modules (not part of the public API) ───────────────
mod traits;
mod types;
pub(crate) mod core;
pub(crate) mod messages;
