/// ENV-01..ENV-08: Envelope type tests
use std::time::Instant;

use dstate::{NodeId, StateObject, Generation, StateViewObject};

#[test]
fn env_01_state_object_age_starts_at_0() {
    let obj = StateObject {
        version: Generation::new(100, 0),
        storage_version: 1,
        value: "initial",
        created_time: 1000,
        modified_time: 1000,
    };
    assert_eq!(obj.age(), 0);
}

#[test]
fn env_02_state_object_timestamps_set_correctly() {
    let created = 1000i64;
    let modified = 2000i64;
    let obj = StateObject {
        version: Generation::new(100, 1),
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
        version: Generation::new(100, 5),
        wire_version: 1,
        value: "view",
        created_time: 1000,
        modified_time: 2000,
        synced_at: now,
        pending_remote_version: None,
        source_node: NodeId(1),
    };
    // synced_at should be close to now
    assert!(view.synced_at.elapsed().as_millis() < 100);
}

#[test]
fn env_04_storage_version_set() {
    let obj = StateObject {
        version: Generation::new(100, 0),
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
        version: Generation::new(100, 1),
        wire_version: 3,
        value: "test",
        created_time: 1000,
        modified_time: 1000,
        synced_at: Instant::now(),
        pending_remote_version: None,
        source_node: NodeId(1),
    };
    assert_eq!(view.wire_version, 3);
}

#[test]
fn env_06_incarnation_in_state_object() {
    let obj = StateObject {
        version: Generation::new(42, 0),
        storage_version: 1,
        value: "test",
        created_time: 1000,
        modified_time: 1000,
    };
    assert_eq!(obj.incarnation(), 42);
}

#[test]
fn env_07_incarnation_propagated_to_view() {
    let obj = StateObject {
        version: Generation::new(42, 5),
        storage_version: 1,
        value: "test",
        created_time: 1000,
        modified_time: 2000,
    };
    let view = StateViewObject {
        version: obj.version,
        wire_version: 1,
        value: "test",
        created_time: obj.created_time,
        modified_time: obj.modified_time,
        synced_at: Instant::now(),
        pending_remote_version: None,
        source_node: NodeId(0),
    };
    assert_eq!(view.version.incarnation, obj.incarnation());
}

#[test]
fn env_08_pending_remote_version_tracks_restarts() {
    let mut view = StateViewObject {
        version: Generation::new(1, 100),
        wire_version: 1,
        value: "stale",
        created_time: 1000,
        modified_time: 2000,
        synced_at: Instant::now(),
        pending_remote_version: None,
        source_node: NodeId(1),
    };

    // Simulate receiving a change notification after owner restart
    // incarnation=2, age=0 means the owner restarted
    view.pending_remote_version = Some(Generation::new(2, 0));

    // The view knows it is stale: newer incarnation exists
    assert!(view.pending_remote_version.unwrap().incarnation > view.version.incarnation);
}