use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Debug;

use crate::types::errors::DeserializeError;

/// Urgency hint returned by `DeltaDistributedState::sync_urgency()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncUrgency {
    /// Push immediately, bypassing any periodic timer.
    Immediate,
    /// Push after the given delay.
    Delayed(std::time::Duration),
    /// Do not push this mutation; accumulate into the next non-suppressed delta.
    Suppress,
    /// Use the configured `SyncStrategy` default.
    Default,
}

/// A distributed state type whose full state is the public view (no projection).
///
/// Implement this for simple state types where the entire `State` is visible to
/// all peers. For state types with private fields or delta support, implement
/// [`DeltaDistributedState`] instead.
pub trait DistributedState: Send + Sync + 'static {
    /// Globally unique name for this state type (e.g., `"node_resource"`).
    fn name() -> &'static str;

    /// Current wire protocol version for serialization.
    const WIRE_VERSION: u32;

    /// Current storage version for persistence.
    const STORAGE_VERSION: u32;

    /// Serialize the state for wire transmission.
    fn serialize_state(&self) -> Vec<u8>;

    /// Deserialize state from bytes at a given wire version.
    fn deserialize_state(bytes: &[u8], wire_version: u32) -> Result<Self, DeserializeError>
    where
        Self: Sized;

    /// Migrate persisted state from an older storage version.
    ///
    /// Default implementation returns an error, causing fallback to empty state.
    fn migrate_state(_bytes: &[u8], _from_version: u32) -> Result<Self, DeserializeError>
    where
        Self: Sized,
    {
        Err(DeserializeError::unknown_version(_from_version))
    }
}

/// A distributed state type with separate State/View/Delta projections.
///
/// The `State` is the full internal state (may contain private fields).
/// The `View` is the public projection visible to peers.
/// The `ViewDelta` is an incremental update to the view.
pub trait DeltaDistributedState: Send + Sync + 'static {
    /// The full internal state (may contain private fields).
    type State: Clone + Send + Sync + Debug + 'static;

    /// The public view visible to peers.
    type View: Clone + Send + Sync + Debug + Serialize + DeserializeOwned + 'static;

    /// An incremental update to the view.
    type ViewDelta: Clone + Send + Sync + Debug + Serialize + DeserializeOwned + 'static;

    /// Application-level description of what changed in a mutation.
    type StateDeltaChange: Clone + Send + Sync + Debug + 'static;

    /// Globally unique name for this state type.
    fn name() -> &'static str;

    /// Current wire protocol version.
    const WIRE_VERSION: u32;

    /// Current storage version.
    const STORAGE_VERSION: u32;

    /// Project the full state into its public view.
    fn project_view(state: &Self::State) -> Self::View;

    /// Project a state-level delta into a view-level delta.
    fn project_delta(change: &Self::StateDeltaChange) -> Self::ViewDelta;

    /// Apply a view delta to an existing view, producing an updated view.
    fn apply_delta(view: &Self::View, delta: &Self::ViewDelta) -> Self::View;

    /// Determine how urgently this mutation should be synchronized.
    fn sync_urgency(
        old_view: &Self::View,
        new_view: &Self::View,
        change: &Self::StateDeltaChange,
    ) -> SyncUrgency {
        let _ = (old_view, new_view, change);
        SyncUrgency::Default
    }

    /// Serialize a view for wire transmission.
    fn serialize_view(view: &Self::View) -> Vec<u8>;

    /// Deserialize a view from bytes at a given wire version.
    fn deserialize_view(bytes: &[u8], wire_version: u32) -> Result<Self::View, DeserializeError>;

    /// Serialize a view delta for wire transmission.
    fn serialize_delta(delta: &Self::ViewDelta) -> Vec<u8>;

    /// Deserialize a view delta from bytes at a given wire version.
    fn deserialize_delta(
        bytes: &[u8],
        wire_version: u32,
    ) -> Result<Self::ViewDelta, DeserializeError>;

    /// Serialize the full state for persistence.
    fn serialize_state(state: &Self::State) -> Vec<u8>;

    /// Deserialize the full state from persistence.
    fn deserialize_state(
        bytes: &[u8],
        storage_version: u32,
    ) -> Result<Self::State, DeserializeError>;

    /// Migrate persisted state from an older storage version.
    fn migrate_state(
        _bytes: &[u8],
        _from_version: u32,
    ) -> Result<Self::State, DeserializeError> {
        Err(DeserializeError::unknown_version(_from_version))
    }
}
