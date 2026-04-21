//! # dstate-integration
//!
//! Integration test framework for the [`dstate`] distributed state crate.
//!
//! Provides a deterministic mock cluster environment for testing the
//! replication protocol end-to-end without a real actor runtime.
//!
//! # Architecture
//!
//! This framework drives
//! [`DistributedStateEngine`](dstate::engine::DistributedStateEngine)
//! instances directly — no actor runtime needed. The engine's action-based
//! API (`Vec<EngineAction>`) makes this possible — the
//! [`MockCluster`](cluster::MockCluster) executes all outbound effects
//! through a [`MockTransport`](transport::MockTransport) that serializes
//! messages to bytes, routes them through an interceptor pipeline, and
//! delivers them to target engines.
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
