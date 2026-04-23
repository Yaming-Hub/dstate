//! # dstate-dactor
//!
//! Actor hosting for the [`dstate`] distributed state replication crate,
//! built on the [`dactor`] actor framework.
//!
//! This crate provides [`StateActor`], a thin wrapper around
//! [`DistributedStateEngine`](dstate::DistributedStateEngine) that bridges
//! its action-based API with dactor's message-passing model.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use dstate_dactor::{StateActor, StateActorArgs, new_peer_registry};
//! use dstate::engine::DistributedStateEngine;
//!
//! // 1. Build the engine
//! let engine = DistributedStateEngine::new(/* ... */);
//! let peers = new_peer_registry();
//!
//! // 2. Spawn the actor
//! let args = StateActorArgs { engine, peers: peers.clone() };
//! let actor_ref = runtime.spawn::<StateActor<MyState, MyView>>("my-state", args).await?;
//!
//! // 3. Set up periodic timers
//! let cancel = CancellationToken::new();
//! dactor::timer::send_interval(&actor_ref, || PeriodicSync, interval, cancel.clone());
//! dactor::timer::send_interval(&actor_ref, || FlushChangeFeed, feed_interval, cancel);
//!
//! // 4. Interact via messages
//! actor_ref.tell(Mutate {
//!     mutate_fn: Box::new(|s| s.counter += 1),
//!     project_fn: Box::new(|s| s.clone()),
//!     urgency: SyncUrgency::Default,
//! })?;
//! ```

pub mod messages;
pub mod state_actor;

pub use messages::*;
pub use state_actor::{
    ActorRefPeerSender, PeerRegistry, PeerSender, StateActor, StateActorArgs, new_peer_registry,
};
