use async_trait::async_trait;
use std::sync::Mutex;

use crate::traits::persistence::{PersistError, StatePersistence};

/// In-memory persistence for testing. Stores a single serialized state.
pub struct InMemoryPersistence<S: Clone + Send + Sync + 'static> {
    stored: Mutex<Option<S>>,
}

impl<S: Clone + Send + Sync + 'static> InMemoryPersistence<S> {
    pub fn new() -> Self {
        Self {
            stored: Mutex::new(None),
        }
    }
}

impl<S: Clone + Send + Sync + 'static> Default for InMemoryPersistence<S> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<S: Clone + Send + Sync + 'static> StatePersistence<S> for InMemoryPersistence<S> {
    type StateDeltaChange = ();

    async fn save(
        &self,
        state: &S,
        _state_delta: Option<&Self::StateDeltaChange>,
    ) -> Result<(), PersistError> {
        let mut stored = self.stored.lock().unwrap();
        *stored = Some(state.clone());
        Ok(())
    }

    async fn load(&self) -> Result<Option<S>, PersistError> {
        let stored = self.stored.lock().unwrap();
        Ok(stored.clone())
    }
}

/// Persistence that always fails, for testing error paths.
pub struct FailingPersistence;

#[async_trait]
impl<S: Send + Sync + 'static> StatePersistence<S> for FailingPersistence {
    type StateDeltaChange = ();

    async fn save(
        &self,
        _state: &S,
        _state_delta: Option<&Self::StateDeltaChange>,
    ) -> Result<(), PersistError> {
        Err(PersistError::StorageUnavailable(
            "FailingPersistence always fails".into(),
        ))
    }

    async fn load(&self) -> Result<Option<S>, PersistError> {
        Err(PersistError::StorageUnavailable(
            "FailingPersistence always fails".into(),
        ))
    }
}
