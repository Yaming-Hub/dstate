use async_trait::async_trait;
use std::fmt;

/// Errors from persistence operations.
#[derive(Debug, Clone)]
pub enum PersistError {
    /// The storage backend is unavailable.
    StorageUnavailable(String),
    /// Failed to deserialize stored data.
    DeserializationFailed(String),
    /// An I/O error occurred.
    Io(String),
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StorageUnavailable(msg) => write!(f, "storage unavailable: {msg}"),
            Self::DeserializationFailed(msg) => write!(f, "deserialization failed: {msg}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for PersistError {}

/// Async trait for persisting state to durable storage.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async tasks.
#[async_trait]
pub trait StatePersistence<S: Send + Sync + 'static>: Send + Sync {
    /// The delta type passed alongside saves when delta information is available.
    type StateDeltaChange: Send + Sync + 'static;

    /// Persist the current state, optionally with a delta describing what changed.
    async fn save(
        &self,
        state: &S,
        state_delta: Option<&Self::StateDeltaChange>,
    ) -> Result<(), PersistError>;

    /// Load the most recently persisted state, if any.
    async fn load(&self) -> Result<Option<S>, PersistError>;
}
