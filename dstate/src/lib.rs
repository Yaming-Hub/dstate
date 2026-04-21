//! # dstate
//!
//! Framework-agnostic distributed state replication for Rust actor systems.
//!
//! `dstate` provides traits and replication logic for managing state that is
//! replicated across nodes in a cluster. State changes are projected into
//! public views, synchronized via configurable strategies, and persisted
//! through a pluggable storage interface.
//!
//! ## Core Traits
//!
//! - [`DistributedState`] — Simple state where the entire value is the public view
//! - [`DeltaDistributedState`] — State with separate view/delta projections
//! - [`StatePersistence`] — Async save/load for crash recovery
//! - [`Clock`] — Time abstraction for deterministic testing
//!
//! ## Actor Runtime (via dactor)
//!
//! Actor spawning, messaging, timers, groups, and cluster events are provided
//! by the [`dactor`](https://crates.io/crates/dactor) crate. Use `dactor`
//! with a concrete backend (ractor, kameo, or coerce) via its adapter crates.

// ── Traits (what users implement) ────────────────────────────────
pub use traits::state::{DeltaDistributedState, DistributedState, SyncUrgency};
pub use traits::persistence::{PersistError, StatePersistence};
pub use traits::clock::{Clock, SystemClock};

// ── Runtime abstractions (cluster events, timers, errors) ────────
pub use traits::runtime::{
    ActorSendError, ClusterError, ClusterEvent, ClusterEvents, GroupError,
    SubscriptionId, TimerHandle,
};

// ── Types (what users construct / receive) ──────────────────────
pub use types::envelope::{StateObject, StateViewObject};
pub use types::config::{ChangeFeedConfig, PushMode, StateConfig, SyncStrategy};
pub use types::node::{NodeId, Generation, VersionMismatchPolicy};
pub use types::errors::{
    DeserializeError, MutationError, QueryError, RegistryError,
};
pub use types::sync_message::{BatchedChangeFeed, ChangeNotification, SyncMessage};

// ── Registry (state registration and lookup) ────────────────────
pub use registry::{AnyStateShard, StateRegistry};

// ── Engine (public API for driving the replication protocol) ─────
pub mod engine;
pub use engine::{
    DistributedStateEngine, EngineAction, EngineHealth, EngineQueryResult, HealthStatus,
    MutateResult, DeltaMutateResult, SyncMetrics, WireMessage,
};

// ── Test support (public for adapter crates and downstream tests) ─
pub mod test_support;

// ── Internal modules (not part of the public API) ───────────────
mod traits;
mod types;
mod registry;
pub(crate) mod core;
pub(crate) mod messages;
