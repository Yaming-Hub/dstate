//! # dstate-ractor
//!
//! Ractor adapter for the [`dstate`] distributed state crate.
//!
//! This crate provides [`RactorRuntime`], an implementation of
//! [`dstate::ActorRuntime`] backed by ractor actors.
//! Actors are spawned as real ractor actors via [`ractor::Actor::spawn`],
//! and messages are delivered through ractor's mailbox system.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use dstate_ractor::RactorRuntime;
//! use dstate::{ActorRuntime, ActorRef};
//!
//! #[tokio::main]
//! async fn main() {
//!     let runtime = RactorRuntime::new();
//!     let actor = runtime.spawn("greeter", |msg: String| {
//!         println!("Got: {msg}");
//!     });
//!     actor.send("hello".into()).unwrap();
//! }
//! ```

pub mod cluster;
pub mod runtime;

pub use cluster::RactorClusterEvents;
pub use runtime::{RactorActorRef, RactorRuntime, RactorTimerHandle};

// Re-export the core dstate crate for convenience
pub use dstate;
