use std::fmt;

use crate::types::node::NodeId;

/// Errors from state registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// No state type registered under the given name.
    StateNotRegistered(String),
    /// A state type is registered under the name, but its Rust type doesn't
    /// match the requested type.
    TypeMismatch {
        name: String,
        expected: &'static str,
        actual: &'static str,
    },
    /// A different state type is already registered under this name.
    DuplicateName(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateNotRegistered(name) => write!(f, "state not registered: {name}"),
            Self::TypeMismatch {
                name,
                expected,
                actual,
            } => write!(f, "type mismatch for '{name}': expected {expected}, got {actual}"),
            Self::DuplicateName(name) => write!(f, "duplicate state name: {name}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Errors from query operations.
#[derive(Debug)]
pub enum QueryError {
    /// The underlying registry lookup failed.
    Registry(RegistryError),
    /// A peer's view is too stale and the peer is unreachable.
    StalePeer { node_id: NodeId, reason: String },
    /// The actor handling the query is no longer running.
    ActorStopped,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(e) => write!(f, "query failed: {e}"),
            Self::StalePeer { node_id, reason } => {
                write!(f, "stale peer {node_id}: {reason}")
            }
            Self::ActorStopped => write!(f, "query failed: actor stopped"),
        }
    }
}

impl std::error::Error for QueryError {}

impl From<RegistryError> for QueryError {
    fn from(e: RegistryError) -> Self {
        Self::Registry(e)
    }
}

/// Errors from mutation operations.
#[derive(Debug)]
pub enum MutationError {
    /// Persistence backend failed to save.
    PersistenceFailed(String),
    /// The actor handling the mutation is no longer running.
    ActorStopped,
}

impl fmt::Display for MutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PersistenceFailed(msg) => write!(f, "persistence failed: {msg}"),
            Self::ActorStopped => write!(f, "mutation failed: actor stopped"),
        }
    }
}

impl std::error::Error for MutationError {}

/// Errors from deserializing state/view/delta data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeserializeError {
    /// The wire/storage version is not supported.
    UnknownVersion(u32),
    /// The data is malformed.
    Malformed(String),
}

impl DeserializeError {
    /// Create an `UnknownVersion` error.
    pub fn unknown_version(version: u32) -> Self {
        Self::UnknownVersion(version)
    }
}

impl fmt::Display for DeserializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownVersion(v) => write!(f, "unknown version: {v}"),
            Self::Malformed(msg) => write!(f, "malformed data: {msg}"),
        }
    }
}

impl std::error::Error for DeserializeError {}
