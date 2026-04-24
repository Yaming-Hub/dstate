//! # dstate-integration
//!
//! Integration test framework for the [`dstate`] distributed state crate.
//!
//! Provides two testing approaches:
//!
//! 1. **Deterministic MockCluster** — drives `DistributedStateEngine` instances
//!    directly with a tick-based simulation loop. Fast, repeatable, and used for
//!    the primary protocol correctness test suite.
//!
//! 2. **dactor-mock + dstate-dactor** — uses [`dstate_dactor::StateActor`] with
//!    `dactor-mock`'s `MockCluster`. Proves dstate works within a dactor actor
//!    system with network partitions and node crash/restart.
//!
//! # Modules
//!
//! - [`interceptor`] — Network interceptor trait + built-in fault injectors
//! - [`transport`] — Byte-level message routing between nodes
//! - [`cluster`] — MockCluster orchestrator with `tick()`/`settle()`
//!
//! See `docs/integration-test-plan.md` for the full test matrix.

pub mod interceptor;
pub mod transport;
pub mod cluster;
