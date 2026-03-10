use dstate::{BatchedChangeFeed, ChangeNotification, Generation, NodeId, SyncMessage};

// WIRE-01
#[test]
fn full_snapshot_round_trip() {
    let msg = SyncMessage::FullSnapshot {
        state_name: "node_resource".into(),
        source_node: NodeId(42),
        generation: Generation::new(3, 15),
        wire_version: 2,
        data: vec![1, 2, 3, 4],
    };

    let bytes = bincode::serialize(&msg).expect("serialize");
    let decoded: SyncMessage = bincode::deserialize(&bytes).expect("deserialize");

    match decoded {
        SyncMessage::FullSnapshot {
            state_name,
            source_node,
            generation,
            wire_version,
            data,
        } => {
            assert_eq!(state_name, "node_resource");
            assert_eq!(source_node, NodeId(42));
            assert_eq!(generation, Generation::new(3, 15));
            assert_eq!(wire_version, 2);
            assert_eq!(data, vec![1, 2, 3, 4]);
        }
        other => panic!("expected FullSnapshot, got {other:?}"),
    }
}

// WIRE-02
#[test]
fn delta_update_round_trip() {
    let msg = SyncMessage::DeltaUpdate {
        state_name: "session_map".into(),
        source_node: NodeId(7),
        generation: Generation::new(1, 100),
        wire_version: 1,
        data: vec![10, 20, 30],
    };

    let bytes = bincode::serialize(&msg).expect("serialize");
    let decoded: SyncMessage = bincode::deserialize(&bytes).expect("deserialize");

    match decoded {
        SyncMessage::DeltaUpdate {
            state_name,
            source_node,
            generation,
            wire_version,
            data,
        } => {
            assert_eq!(state_name, "session_map");
            assert_eq!(source_node, NodeId(7));
            assert_eq!(generation, Generation::new(1, 100));
            assert_eq!(wire_version, 1);
            assert_eq!(data, vec![10, 20, 30]);
        }
        other => panic!("expected DeltaUpdate, got {other:?}"),
    }
}

// WIRE-03
#[test]
fn batched_change_feed_round_trip() {
    let feed = BatchedChangeFeed {
        source_node: NodeId(1),
        notifications: vec![
            ChangeNotification {
                state_name: "alpha".into(),
                source_node: NodeId(1),
                generation: Generation::new(1, 0),
            },
            ChangeNotification {
                state_name: "beta".into(),
                source_node: NodeId(2),
                generation: Generation::new(2, 5),
            },
            ChangeNotification {
                state_name: "gamma".into(),
                source_node: NodeId(3),
                generation: Generation::new(0, 99),
            },
        ],
    };

    let bytes = bincode::serialize(&feed).expect("serialize");
    let decoded: BatchedChangeFeed = bincode::deserialize(&bytes).expect("deserialize");

    assert_eq!(decoded.source_node, NodeId(1));
    assert_eq!(decoded.notifications.len(), 3);

    assert_eq!(decoded.notifications[0].state_name, "alpha");
    assert_eq!(decoded.notifications[0].source_node, NodeId(1));
    assert_eq!(decoded.notifications[0].generation, Generation::new(1, 0));

    assert_eq!(decoded.notifications[1].state_name, "beta");
    assert_eq!(decoded.notifications[1].source_node, NodeId(2));
    assert_eq!(decoded.notifications[1].generation, Generation::new(2, 5));

    assert_eq!(decoded.notifications[2].state_name, "gamma");
    assert_eq!(decoded.notifications[2].source_node, NodeId(3));
    assert_eq!(decoded.notifications[2].generation, Generation::new(0, 99));
}

// WIRE-04
#[test]
fn request_snapshot_round_trip() {
    let msg = SyncMessage::RequestSnapshot {
        state_name: "cluster_config".into(),
        requester: NodeId(99),
    };

    let bytes = bincode::serialize(&msg).expect("serialize");
    let decoded: SyncMessage = bincode::deserialize(&bytes).expect("deserialize");

    match decoded {
        SyncMessage::RequestSnapshot {
            state_name,
            requester,
        } => {
            assert_eq!(state_name, "cluster_config");
            assert_eq!(requester, NodeId(99));
        }
        other => panic!("expected RequestSnapshot, got {other:?}"),
    }
}

#[test]
fn empty_data_round_trip() {
    let msg = SyncMessage::FullSnapshot {
        state_name: "empty_state".into(),
        source_node: NodeId(0),
        generation: Generation::new(0, 0),
        wire_version: 1,
        data: vec![],
    };

    let bytes = bincode::serialize(&msg).expect("serialize");
    let decoded: SyncMessage = bincode::deserialize(&bytes).expect("deserialize");

    match decoded {
        SyncMessage::FullSnapshot { data, .. } => {
            assert!(data.is_empty());
        }
        other => panic!("expected FullSnapshot, got {other:?}"),
    }
}

#[test]
fn large_payload_round_trip() {
    let payload: Vec<u8> = (0u8..=255).cycle().take(10 * 1024).collect();

    let msg = SyncMessage::FullSnapshot {
        state_name: "big_state".into(),
        source_node: NodeId(5),
        generation: Generation::new(10, 500),
        wire_version: 3,
        data: payload.clone(),
    };

    let bytes = bincode::serialize(&msg).expect("serialize");
    let decoded: SyncMessage = bincode::deserialize(&bytes).expect("deserialize");

    match decoded {
        SyncMessage::FullSnapshot {
            state_name,
            source_node,
            generation,
            wire_version,
            data,
        } => {
            assert_eq!(state_name, "big_state");
            assert_eq!(source_node, NodeId(5));
            assert_eq!(generation, Generation::new(10, 500));
            assert_eq!(wire_version, 3);
            assert_eq!(data.len(), 10 * 1024);
            assert_eq!(data, payload);
        }
        other => panic!("expected FullSnapshot, got {other:?}"),
    }
}
