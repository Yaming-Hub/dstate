use serde::{Deserialize, Serialize};

use crate::traits::state::{DeltaDistributedState, DistributedState, SyncUrgency};
use crate::types::errors::DeserializeError;

// ---------------------------------------------------------------------------
// Simple test state (implements DistributedState)
// ---------------------------------------------------------------------------

/// A simple test state where State == View (no projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestState {
    pub counter: u64,
    pub label: String,
}

impl Default for TestState {
    fn default() -> Self {
        Self {
            counter: 0,
            label: String::new(),
        }
    }
}

impl DistributedState for TestState {
    fn name() -> &'static str {
        "test_state"
    }

    const WIRE_VERSION: u32 = 1;
    const STORAGE_VERSION: u32 = 1;

    fn serialize_state(&self) -> Vec<u8> {
        bincode::serialize(self).expect("TestState serialization should not fail")
    }

    fn deserialize_state(bytes: &[u8], wire_version: u32) -> Result<Self, DeserializeError> {
        if wire_version != Self::WIRE_VERSION {
            return Err(DeserializeError::unknown_version(wire_version));
        }
        bincode::deserialize(bytes)
            .map_err(|e| DeserializeError::Malformed(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Delta-aware test state (implements DeltaDistributedState)
// ---------------------------------------------------------------------------

/// Internal state with private fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestDeltaStateInner {
    pub counter: u64,
    pub label: String,
    /// Private field not exposed in the view.
    pub internal_accumulator: f64,
}

impl Default for TestDeltaStateInner {
    fn default() -> Self {
        Self {
            counter: 0,
            label: String::new(),
            internal_accumulator: 0.0,
        }
    }
}

/// Public view (no private fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestDeltaView {
    pub counter: u64,
    pub label: String,
}

/// Incremental view update.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestDeltaViewDelta {
    pub counter_delta: i64,
    pub new_label: Option<String>,
}

/// Application-level change description.
#[derive(Debug, Clone, PartialEq)]
pub struct TestDeltaChange {
    pub counter_delta: i64,
    pub new_label: Option<String>,
    pub accumulator_delta: f64,
}

/// Marker struct implementing `DeltaDistributedState`.
pub struct TestDeltaState;

impl DeltaDistributedState for TestDeltaState {
    type State = TestDeltaStateInner;
    type View = TestDeltaView;
    type ViewDelta = TestDeltaViewDelta;
    type StateDeltaChange = TestDeltaChange;

    fn name() -> &'static str {
        "test_delta_state"
    }

    const WIRE_VERSION: u32 = 1;
    const STORAGE_VERSION: u32 = 1;

    fn project_view(state: &Self::State) -> Self::View {
        TestDeltaView {
            counter: state.counter,
            label: state.label.clone(),
        }
    }

    fn project_delta(change: &Self::StateDeltaChange) -> Self::ViewDelta {
        TestDeltaViewDelta {
            counter_delta: change.counter_delta,
            new_label: change.new_label.clone(),
        }
    }

    fn apply_delta(view: &Self::View, delta: &Self::ViewDelta) -> Self::View {
        TestDeltaView {
            counter: (view.counter as i64 + delta.counter_delta) as u64,
            label: delta.new_label.clone().unwrap_or_else(|| view.label.clone()),
        }
    }

    fn sync_urgency(
        _old_view: &Self::View,
        _new_view: &Self::View,
        _change: &Self::StateDeltaChange,
    ) -> SyncUrgency {
        SyncUrgency::Default
    }

    fn serialize_view(view: &Self::View) -> Vec<u8> {
        bincode::serialize(view).expect("view serialization should not fail")
    }

    fn deserialize_view(bytes: &[u8], wire_version: u32) -> Result<Self::View, DeserializeError> {
        if wire_version != Self::WIRE_VERSION {
            return Err(DeserializeError::unknown_version(wire_version));
        }
        bincode::deserialize(bytes)
            .map_err(|e| DeserializeError::Malformed(e.to_string()))
    }

    fn serialize_delta(delta: &Self::ViewDelta) -> Vec<u8> {
        bincode::serialize(delta).expect("delta serialization should not fail")
    }

    fn deserialize_delta(
        bytes: &[u8],
        wire_version: u32,
    ) -> Result<Self::ViewDelta, DeserializeError> {
        if wire_version != Self::WIRE_VERSION {
            return Err(DeserializeError::unknown_version(wire_version));
        }
        bincode::deserialize(bytes)
            .map_err(|e| DeserializeError::Malformed(e.to_string()))
    }

    fn serialize_state(state: &Self::State) -> Vec<u8> {
        bincode::serialize(state).expect("state serialization should not fail")
    }

    fn deserialize_state(
        bytes: &[u8],
        storage_version: u32,
    ) -> Result<Self::State, DeserializeError> {
        if storage_version != Self::STORAGE_VERSION {
            return Err(DeserializeError::unknown_version(storage_version));
        }
        bincode::deserialize(bytes)
            .map_err(|e| DeserializeError::Malformed(e.to_string()))
    }
}
