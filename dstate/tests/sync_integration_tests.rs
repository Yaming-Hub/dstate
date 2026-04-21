//! Integration tests for public sync API surface.
//!
//! These tests exercise the public types (SyncMessage, SyncStrategy, PushMode,
//! ChangeNotification, BatchedChangeFeed) from the external `dstate` crate
//! perspective. The deeper inbound-wire and sync-logic tests live as unit tests
//! inside `dstate::core::shard_core` and `dstate::core::sync_logic` because
//! `ShardCore` and `SyncLogic` are `pub(crate)`.

use std::time::Duration;

use dstate::{
    BatchedChangeFeed, ChangeNotification, Generation, NodeId, PushMode, SyncStrategy,
};

// ── SyncStrategy factory tests (public API) ─────────────────────

#[test]
fn active_push_strategy_defaults() {
    let s = SyncStrategy::active_push();
    assert_eq!(s.push_mode, Some(PushMode::ActivePush));
    assert_eq!(s.periodic_full_sync, None);
    assert!(!s.pull_on_query);
}

#[test]
fn feed_lazy_pull_strategy_defaults() {
    let s = SyncStrategy::feed_lazy_pull();
    assert_eq!(s.push_mode, Some(PushMode::ActiveFeedLazyPull));
    assert_eq!(s.periodic_full_sync, None);
    assert!(s.pull_on_query);
}

#[test]
fn feed_with_periodic_sync_strategy() {
    let s = SyncStrategy::feed_with_periodic_sync(Duration::from_secs(300));
    assert_eq!(s.push_mode, Some(PushMode::ActiveFeedLazyPull));
    assert_eq!(s.periodic_full_sync, Some(Duration::from_secs(300)));
    assert!(s.pull_on_query);
}

#[test]
fn periodic_only_strategy() {
    let s = SyncStrategy::periodic_only(Duration::from_secs(600));
    assert_eq!(s.push_mode, None);
    assert_eq!(s.periodic_full_sync, Some(Duration::from_secs(600)));
    assert!(!s.pull_on_query);
}

// ── SyncMessage wire type construction ──────────────────────────

#[test]
fn sync_message_full_snapshot_fields() {
    use dstate::SyncMessage;

    let msg = SyncMessage::FullSnapshot {
        state_name: "counters".into(),
        source_node: NodeId("1".to_string()),
        generation: Generation::new(1, 5),
        wire_version: 1,
        data: vec![1, 2, 3],
    };

    match msg {
        SyncMessage::FullSnapshot {
            state_name,
            source_node,
            generation,
            wire_version,
            data,
        } => {
            assert_eq!(state_name, "counters");
            assert_eq!(source_node, NodeId("1".to_string()));
            assert_eq!(generation, Generation::new(1, 5));
            assert_eq!(wire_version, 1);
            assert_eq!(data, vec![1, 2, 3]);
        }
        _ => panic!("expected FullSnapshot"),
    }
}

#[test]
fn sync_message_delta_update_fields() {
    use dstate::SyncMessage;

    let msg = SyncMessage::DeltaUpdate {
        state_name: "sessions".into(),
        source_node: NodeId("2".to_string()),
        generation: Generation::new(1, 10),
        wire_version: 1,
        data: vec![4, 5, 6],
    };

    match msg {
        SyncMessage::DeltaUpdate {
            state_name,
            source_node,
            generation,
            ..
        } => {
            assert_eq!(state_name, "sessions");
            assert_eq!(source_node, NodeId("2".to_string()));
            assert_eq!(generation, Generation::new(1, 10));
        }
        _ => panic!("expected DeltaUpdate"),
    }
}

#[test]
fn sync_message_request_snapshot_fields() {
    use dstate::SyncMessage;

    let msg = SyncMessage::RequestSnapshot {
        state_name: "counters".into(),
        requester: NodeId("3".to_string()),
    };

    match msg {
        SyncMessage::RequestSnapshot {
            state_name,
            requester,
        } => {
            assert_eq!(state_name, "counters");
            assert_eq!(requester, NodeId("3".to_string()));
        }
        _ => panic!("expected RequestSnapshot"),
    }
}

// ── BatchedChangeFeed / ChangeNotification construction ─────────

#[test]
fn batched_change_feed_construction() {
    let batch = BatchedChangeFeed {
        source_node: NodeId("1".to_string()),
        notifications: vec![
            ChangeNotification {
                state_name: "counters".into(),
                source_node: NodeId("2".to_string()),
                generation: Generation::new(1, 10),
            },
            ChangeNotification {
                state_name: "sessions".into(),
                source_node: NodeId("3".to_string()),
                generation: Generation::new(2, 5),
            },
        ],
    };

    assert_eq!(batch.source_node, NodeId("1".to_string()));
    assert_eq!(batch.notifications.len(), 2);
    assert_eq!(batch.notifications[0].state_name, "counters");
    assert_eq!(batch.notifications[1].state_name, "sessions");
}

#[test]
fn change_notification_generation_ordering() {
    let older = Generation::new(1, 5);
    let newer = Generation::new(1, 10);
    let new_incarnation = Generation::new(2, 1);

    assert!(newer > older);
    assert!(new_incarnation > newer);
    assert!(new_incarnation > older);
}

// ── Cross-type integration: wire message → change feed scenario ─

#[test]
fn wire_to_change_feed_scenario() {
    // Simulate the flow: a node receives a BatchedChangeFeed from a peer
    // and needs to determine which shards are stale.
    let self_node = NodeId("1".to_string());

    let batch = BatchedChangeFeed {
        source_node: NodeId("5".to_string()), // relay node
        notifications: vec![
            ChangeNotification {
                state_name: "counters".into(),
                source_node: NodeId("2".to_string()),
                generation: Generation::new(1, 20),
            },
            ChangeNotification {
                state_name: "sessions".into(),
                source_node: NodeId("3".to_string()),
                generation: Generation::new(1, 15),
            },
        ],
    };

    // Verify batch is not from self (would be filtered in real code).
    assert_ne!(batch.source_node, self_node);

    // Each notification targets a different state_name — in real code,
    // each would be routed to its respective StateShard via mark_stale.
    let state_names: Vec<&str> = batch
        .notifications
        .iter()
        .map(|n| n.state_name.as_str())
        .collect();
    assert!(state_names.contains(&"counters"));
    assert!(state_names.contains(&"sessions"));
}
