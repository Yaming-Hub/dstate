//! Rolling upgrade tests — UPGRADE-01 through UPGRADE-04
//!
//! Simulates two-phase rolling upgrade using MockCluster with engines
//! that have different wire_version values. Tests that the system handles
//! mixed-version clusters gracefully.

use std::sync::Arc;
use std::time::Duration;

use dstate::engine::DistributedStateEngine;
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::TestState;
use dstate::{
    DeserializeError, DistributedState, NodeId, StateConfig, SyncStrategy, SyncUrgency,
    VersionMismatchPolicy,
};
use dstate_integration::cluster::MockCluster;

fn node(i: u64) -> NodeId {
    NodeId(i.to_string())
}

fn active_push_keep_stale() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        version_mismatch_policy: VersionMismatchPolicy::KeepStale,
        ..StateConfig::default()
    }
}

/// Build an engine that sends with `wire_version` but can read any version
/// in `accepted_versions`.
fn make_multi_version_engine(
    node_id: NodeId,
    config: StateConfig,
    clock: Arc<TestClock>,
    wire_version: u32,
    accepted_versions: Vec<u32>,
) -> DistributedStateEngine<TestState, TestState> {
    DistributedStateEngine::new(
        TestState::name(),
        node_id,
        TestState::default(),
        |s| s.clone(),
        config,
        wire_version,
        clock,
        |v| bincode::serialize(v).unwrap(),
        move |b, v| {
            if !accepted_versions.contains(&v) {
                return Err(DeserializeError::unknown_version(v));
            }
            bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
        },
        None,
    )
}

// ── UPGRADE-01: Rolling upgrade phase 1 ─────────────────────────
// 5-node cluster. Upgrade nodes one at a time to phase 1 code
// (read V1+V2, send V1). Verify no sync failures during rollout.

#[test]
fn upgrade_01_rolling_phase1() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let config = active_push_keep_stale();

    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    // Start all 5 nodes as V1-only
    for i in 0..5 {
        let engine = make_multi_version_engine(
            node(i),
            config.clone(),
            clock.clone(),
            1, // send V1
            vec![1],
        );
        cluster.add_node(engine, TestState::default());
    }

    // Mutate and verify baseline
    cluster.mutate(
        node(0),
        |s| s.counter = 10,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    for i in 1..5 {
        let snap = cluster.engine(node(i)).snapshot();
        assert_eq!(snap[&node(0)].value.counter, 10);
    }

    // Upgrade nodes one at a time to phase 1 (read V1+V2, still send V1)
    for upgrade_idx in 0..5u64 {
        cluster.remove_node(node(upgrade_idx));

        let engine = make_multi_version_engine(
            node(upgrade_idx),
            config.clone(),
            clock.clone(),
            1,           // still send V1
            vec![1, 2],  // can read both
        );
        cluster.add_node(engine, TestState::default());

        // Each upgraded node mutates to verify it works
        cluster.mutate(
            node(upgrade_idx),
            move |s| s.counter = 100 + upgrade_idx,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        cluster.settle();

        // All remaining nodes should see the upgrade node's mutation
        let all_ids = cluster.all_node_ids();
        for observer_id in &all_ids {
            if observer_id == &node(upgrade_idx) {
                continue;
            }
            let snap = cluster.engine(observer_id.clone()).snapshot();
            assert_eq!(
                snap[&node(upgrade_idx)].value.counter,
                100 + upgrade_idx,
                "observer {} should see upgraded node {}'s value",
                observer_id,
                upgrade_idx,
            );
        }
    }
}

// ── UPGRADE-02: Rolling upgrade phase 2 ─────────────────────────
// All nodes at phase 1 (read V1+V2, send V1). Upgrade one at a time
// to phase 2 (send V2). Verify no sync failures.

#[test]
fn upgrade_02_rolling_phase2() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let config = active_push_keep_stale();

    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    // All nodes start at phase 1: read V1+V2, send V1
    for i in 0..5 {
        let engine = make_multi_version_engine(
            node(i),
            config.clone(),
            clock.clone(),
            1,
            vec![1, 2],
        );
        cluster.add_node(engine, TestState::default());
    }

    // Upgrade nodes to phase 2 (send V2) one at a time
    for upgrade_idx in 0..5u64 {
        cluster.remove_node(node(upgrade_idx));

        let engine = make_multi_version_engine(
            node(upgrade_idx),
            config.clone(),
            clock.clone(),
            2,           // now send V2
            vec![1, 2],  // still read both
        );
        cluster.add_node(engine, TestState::default());

        cluster.mutate(
            node(upgrade_idx),
            move |s| s.counter = 200 + upgrade_idx,
            |s| s.clone(),
            SyncUrgency::Default,
        );
        cluster.settle();

        // All nodes (both V1-sending and V2-sending) should receive the message
        // because all can read V1+V2
        let all_ids = cluster.all_node_ids();
        for observer_id in &all_ids {
            if observer_id == &node(upgrade_idx) {
                continue;
            }
            let snap = cluster.engine(observer_id.clone()).snapshot();
            assert_eq!(
                snap[&node(upgrade_idx)].value.counter,
                200 + upgrade_idx,
            );
        }
    }
}

// ── UPGRADE-03: Partial rollback during phase 2 ─────────────────
// 3 of 5 nodes upgraded to phase 2. Roll back 1 node to phase 1.

#[test]
fn upgrade_03_partial_rollback() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let config = active_push_keep_stale();

    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    // Nodes 0,1,2 at phase 2 (send V2, read V1+V2)
    for i in 0..3 {
        let engine = make_multi_version_engine(
            node(i),
            config.clone(),
            clock.clone(),
            2,
            vec![1, 2],
        );
        cluster.add_node(engine, TestState::default());
    }

    // Nodes 3,4 still at phase 1 (send V1, read V1+V2)
    for i in 3..5 {
        let engine = make_multi_version_engine(
            node(i),
            config.clone(),
            clock.clone(),
            1,
            vec![1, 2],
        );
        cluster.add_node(engine, TestState::default());
    }

    // Verify mixed cluster works
    cluster.mutate(
        node(0),
        |s| s.counter = 300,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    for i in 1..5 {
        let snap = cluster.engine(node(i)).snapshot();
        assert_eq!(snap[&node(0)].value.counter, 300);
    }

    // Rollback node 2 from phase 2 back to phase 1
    cluster.remove_node(node(2));
    let engine = make_multi_version_engine(
        node(2),
        config.clone(),
        clock.clone(),
        1,           // rolled back to V1
        vec![1, 2],  // still reads both
    );
    cluster.add_node(engine, TestState::default());

    // Verify rolled-back node can still sync with V2 senders
    cluster.mutate(
        node(0),
        |s| s.counter = 301, // node 0 sends V2
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    let snap = cluster.engine(node(2)).snapshot();
    assert_eq!(snap[&node(0)].value.counter, 301);

    // Verify rolled-back node's V1 messages are received by V2 nodes
    cluster.mutate(
        node(2),
        |s| s.counter = 999,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    for i in 0..5 {
        if i == 2 {
            continue;
        }
        let snap = cluster.engine(node(i)).snapshot();
        assert_eq!(snap[&node(2)].value.counter, 999);
    }
}

// ── UPGRADE-04: Mixed version steady state ──────────────────────
// Hold a mixed V1/V2 cluster under load. Verify no anomalies.

#[test]
fn upgrade_04_mixed_version_steady_state() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let config = active_push_keep_stale();

    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    // Nodes 0,1 send V1 (read V1+V2)
    for i in 0..2 {
        let engine = make_multi_version_engine(
            node(i),
            config.clone(),
            clock.clone(),
            1,
            vec![1, 2],
        );
        cluster.add_node(engine, TestState::default());
    }

    // Nodes 2,3,4 send V2 (read V1+V2)
    for i in 2..5 {
        let engine = make_multi_version_engine(
            node(i),
            config.clone(),
            clock.clone(),
            2,
            vec![1, 2],
        );
        cluster.add_node(engine, TestState::default());
    }

    // Sustained load: each node mutates 100 times
    let mutations_per_node = 100u64;
    for round in 1..=mutations_per_node {
        for i in 0..5u64 {
            cluster.mutate(
                node(i),
                move |s| {
                    s.counter = i * 1000 + round;
                    s.label = format!("node-{i}-round-{round}");
                },
                |s| s.clone(),
                SyncUrgency::Default,
            );
        }
        // Tick a few times per round
        cluster.tick_n(3);
    }

    cluster.settle();

    // All nodes should converge to the final values
    for observer in 0..5u64 {
        let snap = cluster.engine(node(observer)).snapshot();
        assert_eq!(snap.len(), 5, "node {observer} should see all 5 nodes");
        for target in 0..5u64 {
            let view = &snap[&node(target)];
            let expected = target * 1000 + mutations_per_node;
            assert_eq!(
                view.value.counter, expected,
                "node {observer} sees wrong value for node {target}"
            );
        }
    }
}
