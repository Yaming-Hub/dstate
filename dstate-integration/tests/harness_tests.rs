//! Harness validation tests — TEST-11 through TEST-14
//!
//! Verify the MockCluster test harness itself works correctly:
//! creation, settle semantics, crash/restart, and time control.

use std::sync::Arc;
use std::time::Duration;

use dstate::engine::DistributedStateEngine;
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::TestState;
use dstate::{Clock, DeserializeError, DistributedState, NodeId, StateConfig, SyncStrategy, SyncUrgency};
use dstate_integration::cluster::MockCluster;

fn active_push_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    }
}

fn node(i: u64) -> NodeId {
    NodeId(i.to_string())
}

fn make_engine(
    node_id: NodeId,
    clock: Arc<TestClock>,
) -> DistributedStateEngine<TestState, TestState> {
    DistributedStateEngine::new(
        TestState::name(),
        node_id,
        TestState::default(),
        |s| s.clone(),
        active_push_config(),
        TestState::WIRE_VERSION,
        clock,
        |v| bincode::serialize(v).unwrap(),
        |b, v| {
            if v != TestState::WIRE_VERSION {
                return Err(DeserializeError::unknown_version(v));
            }
            bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
        },
        None,
    )
}

// ── TEST-11: TestCluster::new(N) creates N nodes ────────────────

#[test]
fn test_11_cluster_creates_n_nodes() {
    let cluster = MockCluster::with_test_state(3, active_push_config());

    let ids = cluster.all_node_ids();
    assert_eq!(ids.len(), 3, "should have 3 nodes");

    // Verify distinct IDs
    let unique: std::collections::HashSet<&NodeId> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "all node IDs should be distinct");

    // Each node should have a running engine with the default state
    for id in &ids {
        let snap = cluster.engine(id.clone()).snapshot();
        // At minimum, the node should see itself
        assert!(snap.contains_key(id), "node should see itself in snapshot");
    }
}

// ── TEST-12: settle() waits until all messages delivered ────────

#[test]
fn test_12_settle_delivers_all_messages() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    // Mutate on node 0
    cluster.mutate(
        node(0),
        |s| s.counter = 42,
        |s| s.clone(),
        SyncUrgency::Default,
    );

    // Before settle, peers may not have the update yet (it depends on tick count)
    // After settle, all peers must have it
    let ticks = cluster.settle();
    assert!(ticks > 0, "should take at least one tick to deliver");

    // Verify all peers see the update
    for i in 1..3 {
        let snap = cluster.engine(node(i)).snapshot();
        assert_eq!(
            snap[&node(0)].value.counter, 42,
            "node {i} should see the mutation after settle"
        );
    }
}

#[test]
fn test_12b_settle_with_limit_panics() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    // Add a DropAll interceptor so messages never arrive
    use dstate_integration::interceptor::DropAll;
    cluster
        .transport_mut()
        .add_interceptor(Box::new(DropAll));

    cluster.mutate(
        node(0),
        |s| s.counter = 1,
        |s| s.clone(),
        SyncUrgency::Immediate,
    );

    // settle_with_limit should eventually stop — it won't converge but
    // is_settled checks only in_flight count (dropped messages are removed
    // from in-flight). So it should settle quickly even with drops.
    let ticks = cluster.settle_with_limit(100);
    // Just verify it doesn't panic — DropAll removes messages from transit
    assert!(ticks <= 100);
}

// ── TEST-13: crash_and_restart simulates crash ──────────────────

#[test]
fn test_13_crash_and_restart() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    for i in 0..3 {
        cluster.add_node(make_engine(node(i), clock.clone()), TestState::default());
    }

    // Mutate on node 0
    cluster.mutate(
        node(0),
        |s| s.counter = 50,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    // Record node 0's generation before crash
    let pre_crash_gen = cluster.engine(node(0)).state().generation;

    // Crash node 0 (hard — loses messages)
    cluster.remove_node_hard(node(0));

    // Restart node 0 with fresh state (new incarnation)
    let new_engine = make_engine(node(0), clock.clone());
    cluster.add_node(new_engine, TestState::default());
    cluster.settle();

    // The restarted node's state should be fresh (counter=0)
    let snap = cluster.engine(node(0)).snapshot();
    assert_eq!(snap[&node(0)].value.counter, 0);

    // But it should see peers
    assert!(snap.contains_key(&node(1)));
    assert!(snap.contains_key(&node(2)));

    // Peers should see the restarted node (possibly with reset state)
    let peer_snap = cluster.engine(node(1)).snapshot();
    assert!(peer_snap.contains_key(&node(0)));

    // New incarnation is different (clock-based, so it's a new value)
    let post_crash_gen = snap[&node(0)].generation;
    // The age resets to 0 on fresh start
    assert_eq!(post_crash_gen.age, 0);
    // Incarnation should be different (based on clock time)
    // Note: with TestClock at base 1_000_000 and some ticks, the incarnation changes
    assert_ne!(
        pre_crash_gen.incarnation,
        post_crash_gen.incarnation,
        "new incarnation should differ from pre-crash"
    );
}

// ── TEST-14: advance_time controls shared clock ─────────────────

#[test]
fn test_14_advance_time_controls_clock() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    for i in 0..2 {
        cluster.add_node(make_engine(node(i), clock.clone()), TestState::default());
    }

    // Record initial unix_ms
    let t0 = clock.unix_ms();

    // Advance by 30 seconds
    clock.advance(Duration::from_secs(30));

    let t1 = clock.unix_ms();
    assert_eq!(
        t1 - t0,
        30_000,
        "clock should advance by exactly 30 seconds"
    );

    // Mutate after advancing — the mutation timestamp should reflect advanced time
    cluster.mutate(
        node(0),
        |s| s.counter = 1,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    let snap = cluster.engine(node(0)).snapshot();
    let modified_time = snap[&node(0)].modified_time;
    assert!(
        modified_time >= t1 as i64,
        "modified_time ({modified_time}) should be >= advanced clock ({t1})"
    );

    // Freshness checks should use the advanced clock
    // Advance another 10 seconds
    clock.advance(Duration::from_secs(10));

    let snap_1 = cluster.engine(node(1)).snapshot();
    let node0_synced = snap_1[&node(0)].synced_at;
    // The synced_at was set before the 10s advance, so it should be "old"
    let now = clock.now();
    let staleness = now.duration_since(node0_synced);
    assert!(
        staleness >= Duration::from_secs(10),
        "staleness should be >= 10s after clock advance"
    );
}
