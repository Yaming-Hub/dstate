/// ENV-01..ENV-07: Envelope type tests
use std::time::Instant;

use dstate::types::envelope::*;
use dstate::types::node::NodeId;

#[test]
fn env_01_state_object_age_starts_at_0() {
    let obj = StateObject {
        age: 0,
        incarnation: 100,
        storage_version: 1,
        value: "initial",
        created_time: 1000,
        modified_time: 1000,
    };
    assert_eq!(obj.age, 0);
}

#[test]
fn env_02_state_object_timestamps_set_correctly() {
    let created = 1000i64;
    let modified = 2000i64;
    let obj = StateObject {
        age: 1,
        incarnation: 100,
        storage_version: 1,
        value: "test",
        created_time: created,
        modified_time: modified,
    };
    assert_eq!(obj.created_time, 1000);
    assert_eq!(obj.modified_time, 2000);
}

#[test]
fn env_03_state_view_object_tracks_synced_at() {
    let now = Instant::now();
    let view = StateViewObject {
        age: 5,
        incarnation: 100,
        wire_version: 1,
        value: "view",
        created_time: 1000,
        modified_time: 2000,
        synced_at: now,
        pending_remote_age: None,
        source_node: NodeId(1),
    };
    // synced_at should be close to now
    assert!(view.synced_at.elapsed().as_millis() < 100);
}

#[test]
fn env_04_storage_version_set() {
    let obj = StateObject {
        age: 0,
        incarnation: 100,
        storage_version: 2,
        value: "test",
        created_time: 1000,
        modified_time: 1000,
    };
    assert_eq!(obj.storage_version, 2);
}

#[test]
fn env_05_wire_version_set() {
    let view = StateViewObject {
        age: 1,
        incarnation: 100,
        wire_version: 3,
        value: "test",
        created_time: 1000,
        modified_time: 1000,
        synced_at: Instant::now(),
        pending_remote_age: None,
        source_node: NodeId(1),
    };
    assert_eq!(view.wire_version, 3);
}

#[test]
fn env_06_incarnation_in_state_object() {
    let obj = StateObject {
        age: 0,
        incarnation: 42,
        storage_version: 1,
        value: "test",
        created_time: 1000,
        modified_time: 1000,
    };
    assert_eq!(obj.incarnation, 42);
}

#[test]
fn env_07_incarnation_propagated_to_view() {
    let obj = StateObject {
        age: 5,
        incarnation: 42,
        storage_version: 1,
        value: "test",
        created_time: 1000,
        modified_time: 2000,
    };
    let view = StateViewObject {
        age: obj.age,
        incarnation: obj.incarnation,
        wire_version: 1,
        value: "test",
        created_time: obj.created_time,
        modified_time: obj.modified_time,
        synced_at: Instant::now(),
        pending_remote_age: None,
        source_node: NodeId(0),
    };
    assert_eq!(view.incarnation, obj.incarnation);
}
