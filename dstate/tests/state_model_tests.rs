/// SM-01..SM-03: DistributedState trait tests
/// DSM-01..DSM-10: DeltaDistributedState trait tests
use dstate::test_support::test_state::*;
use dstate::traits::state::*;

// ---------------------------------------------------------------------------
// SM: DistributedState (simple) trait tests
// ---------------------------------------------------------------------------

#[test]
fn sm_01_serialize_deserialize_round_trip() {
    let state = TestState {
        counter: 42,
        label: "hello".into(),
    };
    let bytes = state.serialize_state();
    let recovered = TestState::deserialize_state(&bytes, TestState::WIRE_VERSION).unwrap();
    assert_eq!(state, recovered);
}

#[test]
fn sm_02_deserialize_rejects_unknown_wire_version() {
    let state = TestState {
        counter: 1,
        label: "x".into(),
    };
    let bytes = state.serialize_state();
    let result = TestState::deserialize_state(&bytes, 999);
    assert!(result.is_err());
    match result.unwrap_err() {
        dstate::types::errors::DeserializeError::UnknownVersion(v) => assert_eq!(v, 999),
        other => panic!("expected UnknownVersion, got {other:?}"),
    }
}

#[test]
fn sm_03_state_is_fully_public() {
    let state = TestState {
        counter: 100,
        label: "visible".into(),
    };
    let bytes = state.serialize_state();
    let peer_view = TestState::deserialize_state(&bytes, TestState::WIRE_VERSION).unwrap();
    assert_eq!(peer_view.counter, 100);
    assert_eq!(peer_view.label, "visible");
}

// ---------------------------------------------------------------------------
// DSM: DeltaDistributedState trait tests
// ---------------------------------------------------------------------------

#[test]
fn dsm_01_project_view_extracts_public_fields_only() {
    let state = TestDeltaStateInner {
        counter: 10,
        label: "pub".into(),
        internal_accumulator: 99.9, // private
    };
    let view = TestDeltaState::project_view(&state);
    assert_eq!(view.counter, 10);
    assert_eq!(view.label, "pub");
    // internal_accumulator is not in the view
}

#[test]
fn dsm_02_project_delta_converts_change_to_view_delta() {
    let change = TestDeltaChange {
        counter_delta: 5,
        new_label: Some("updated".into()),
        accumulator_delta: 1.0, // private change
    };
    let delta = TestDeltaState::project_delta(&change);
    assert_eq!(delta.counter_delta, 5);
    assert_eq!(delta.new_label, Some("updated".into()));
}

#[test]
fn dsm_03_project_delta_apply_delta_round_trip() {
    let old_state = TestDeltaStateInner {
        counter: 10,
        label: "old".into(),
        internal_accumulator: 0.0,
    };
    let change = TestDeltaChange {
        counter_delta: 3,
        new_label: Some("new".into()),
        accumulator_delta: 1.0,
    };
    let new_state = TestDeltaStateInner {
        counter: 13,
        label: "new".into(),
        internal_accumulator: 1.0,
    };

    let old_view = TestDeltaState::project_view(&old_state);
    let delta = TestDeltaState::project_delta(&change);
    let applied_view = TestDeltaState::apply_delta(&old_view, &delta);
    let expected_view = TestDeltaState::project_view(&new_state);
    assert_eq!(applied_view, expected_view);
}

#[test]
fn dsm_04_project_delta_filters_private_changes() {
    let change = TestDeltaChange {
        counter_delta: 0,
        new_label: None,
        accumulator_delta: 42.0, // only private field changes
    };
    let delta = TestDeltaState::project_delta(&change);
    assert_eq!(delta.counter_delta, 0);
    assert_eq!(delta.new_label, None);
}

#[test]
fn dsm_05_multiple_deltas_compose_correctly() {
    let initial = TestDeltaStateInner {
        counter: 0,
        label: "start".into(),
        internal_accumulator: 0.0,
    };
    let change1 = TestDeltaChange {
        counter_delta: 5,
        new_label: None,
        accumulator_delta: 1.0,
    };
    let change2 = TestDeltaChange {
        counter_delta: 3,
        new_label: Some("end".into()),
        accumulator_delta: 2.0,
    };

    let view = TestDeltaState::project_view(&initial);
    let d1 = TestDeltaState::project_delta(&change1);
    let d2 = TestDeltaState::project_delta(&change2);
    let result = TestDeltaState::apply_delta(&TestDeltaState::apply_delta(&view, &d1), &d2);

    let final_state = TestDeltaStateInner {
        counter: 8,
        label: "end".into(),
        internal_accumulator: 3.0,
    };
    assert_eq!(result, TestDeltaState::project_view(&final_state));
}

#[test]
fn dsm_06_apply_delta_advances_view() {
    let view = TestDeltaView {
        counter: 10,
        label: "old".into(),
    };
    let delta = TestDeltaViewDelta {
        counter_delta: 5,
        new_label: Some("new".into()),
    };
    let result = TestDeltaState::apply_delta(&view, &delta);
    assert_eq!(result.counter, 15);
    assert_eq!(result.label, "new");
}

#[test]
fn dsm_07_serialize_deserialize_view_round_trip() {
    let view = TestDeltaView {
        counter: 42,
        label: "hello".into(),
    };
    let bytes = TestDeltaState::serialize_view(&view);
    let recovered = TestDeltaState::deserialize_view(&bytes, TestDeltaState::WIRE_VERSION).unwrap();
    assert_eq!(view, recovered);
}

#[test]
fn dsm_08_serialize_deserialize_delta_round_trip() {
    let delta = TestDeltaViewDelta {
        counter_delta: -3,
        new_label: Some("changed".into()),
    };
    let bytes = TestDeltaState::serialize_delta(&delta);
    let recovered =
        TestDeltaState::deserialize_delta(&bytes, TestDeltaState::WIRE_VERSION).unwrap();
    assert_eq!(delta, recovered);
}

#[test]
fn dsm_09_deserialize_view_rejects_unknown_version() {
    let view = TestDeltaView {
        counter: 1,
        label: "x".into(),
    };
    let bytes = TestDeltaState::serialize_view(&view);
    let result = TestDeltaState::deserialize_view(&bytes, 999);
    assert!(result.is_err());
}

#[test]
fn dsm_10_deserialize_delta_rejects_unknown_version() {
    let delta = TestDeltaViewDelta {
        counter_delta: 1,
        new_label: None,
    };
    let bytes = TestDeltaState::serialize_delta(&delta);
    let result = TestDeltaState::deserialize_delta(&bytes, 999);
    assert!(result.is_err());
}
