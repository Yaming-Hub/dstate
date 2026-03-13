//! # dstate-integration
//!
//! Integration test framework for the [`dstate`] distributed state crate.
//!
//! Provides a mock actor framework (`MockRuntime`) with single-threaded
//! mailboxes, byte-level inter-node transport, and composable network
//! fault injection — enabling deterministic end-to-end testing of the
//! full distributed state replication protocol.
//!
//! See `docs/integration-test-plan.md` for the full test matrix.
