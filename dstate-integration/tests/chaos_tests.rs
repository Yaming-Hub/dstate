//! Chaos / fault injection tests — CHAOS-01 through CHAOS-06
//!
//! These verify system resilience under adverse network conditions using
//! MockCluster + interceptors for determinism.

use std::sync::Arc;
use std::time::Duration;

use dstate::engine::DistributedStateEngine;
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::TestState;
use dstate::{
    DeserializeError, DistributedState, NodeId, StateConfig, SyncStrategy, SyncUrgency,
};
use dstate_integration::cluster::MockCluster;
use dstate_integration::interceptor::*;

fn active_push_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    }
}

fn node(i: u64) -> NodeId {
    NodeId(i.to_string())
}

// ── CHAOS-01: Random network delays ─────────────────────────────
//
// We use DelayTicks to add extra latency. The system should converge
// even when messages arrive late.

#[test]
fn chaos_01_delayed_messages_converge() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    // Add a 5-tick delay to all messages
    cluster
        .transport_mut()
        .add_interceptor(Box::new(DelayTicks(5)));

    // Mutate on node 0
    cluster.mutate(
        node(0),
        |s| s.counter = 42,
        |s| s.clone(),
        SyncUrgency::Default,
    );

    // Tick a few times — not enough for delivery
    cluster.tick_n(3);

    // Node 1 shouldn't have the update yet (delay = 5 extra ticks)
    let snap = cluster.engine(node(1)).snapshot();
    assert_eq!(
        snap.get(&node(0)).map(|v| v.value.counter).unwrap_or(0),
        0,
        "message should not arrive before delay expires"
    );

    // Settle — all delayed messages will be delivered
    cluster.settle();

    // Now all nodes converge
    for i in 1..3 {
        let snap = cluster.engine(node(i)).snapshot();
        assert_eq!(snap[&node(0)].value.counter, 42);
    }
}

// ── CHAOS-02: Packet loss (10% drop rate) ───────────────────────

#[test]
fn chaos_02_packet_loss_recovery() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    cluster
        .transport_mut()
        .add_interceptor(Box::new(DropRate::new(0.1, 12345)));

    // Perform many mutations — with active_push, each mutation broadcasts
    // a snapshot. Even with 10% loss, later snapshots carry latest state.
    for i in 1..=100 {
        cluster.mutate(
            node(0),
            move |s| s.counter = i,
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }

    cluster.settle();

    // Peers should converge to the latest value, or close to it,
    // because each snapshot carries the full state — even if some were
    // dropped, later ones will deliver the latest value.
    for i in 1..3 {
        let snap = cluster.engine(node(i)).snapshot();
        let val = snap[&node(0)].value.counter;
        assert!(
            val >= 90,
            "node {i} should converge close to 100, got {val}"
        );
    }
}

// ── CHAOS-03: Node crash and rejoin ─────────────────────────────

#[test]
fn chaos_03_node_crash_rejoin() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    // Build 3 nodes manually
    for i in 0..3 {
        let engine = DistributedStateEngine::new(
            TestState::name(),
            node(i),
            TestState::default(),
            |s| s.clone(),
            active_push_config(),
            TestState::WIRE_VERSION,
            clock.clone(),
            |v| bincode::serialize(v).unwrap(),
            |b, v| {
                if v != TestState::WIRE_VERSION {
                    return Err(DeserializeError::unknown_version(v));
                }
                bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        );
        cluster.add_node(engine, TestState::default());
    }

    // Mutate node 0 and settle
    cluster.mutate(
        node(0),
        |s| s.counter = 100,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    // Crash node 1 (hard crash — loses in-flight messages)
    cluster.remove_node_hard(node(1));

    // Mutate node 0 while node 1 is down
    cluster.mutate(
        node(0),
        |s| s.counter = 200,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    // Node 2 should have the update
    let snap = cluster.engine(node(2)).snapshot();
    assert_eq!(snap[&node(0)].value.counter, 200);

    // Rejoin node 1 with new incarnation (simulates restart)
    let new_engine = DistributedStateEngine::new(
        TestState::name(),
        node(1),
        TestState::default(), // fresh state — lost everything
        |s| s.clone(),
        active_push_config(),
        TestState::WIRE_VERSION,
        clock.clone(),
        |v| bincode::serialize(v).unwrap(),
        |b, v| {
            if v != TestState::WIRE_VERSION {
                return Err(DeserializeError::unknown_version(v));
            }
            bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
        },
        None,
    );
    cluster.add_node(new_engine, TestState::default());
    cluster.settle();

    // Rejoined node 1 should see node 0's latest value
    let snap = cluster.engine(node(1)).snapshot();
    assert_eq!(snap[&node(0)].value.counter, 200);
}

// ── CHAOS-04: Rapid join/leave cycles ───────────────────────────

#[test]
fn chaos_04_rapid_join_leave() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    // Stable nodes 0 and 1
    for i in 0..2 {
        let engine = DistributedStateEngine::new(
            TestState::name(),
            node(i),
            TestState::default(),
            |s| s.clone(),
            active_push_config(),
            TestState::WIRE_VERSION,
            clock.clone(),
            |v| bincode::serialize(v).unwrap(),
            |b, v| {
                if v != TestState::WIRE_VERSION {
                    return Err(DeserializeError::unknown_version(v));
                }
                bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        );
        cluster.add_node(engine, TestState::default());
    }

    // Rapidly join and leave node 2 ten times
    for cycle in 0..10u64 {
        let engine = DistributedStateEngine::new(
            TestState::name(),
            node(2),
            TestState { counter: cycle + 1, label: format!("cycle-{cycle}") },
            |s| s.clone(),
            active_push_config(),
            TestState::WIRE_VERSION,
            clock.clone(),
            |v| bincode::serialize(v).unwrap(),
            |b, v| {
                if v != TestState::WIRE_VERSION {
                    return Err(DeserializeError::unknown_version(v));
                }
                bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        );
        cluster.add_node(engine, TestState::default());
        cluster.tick_n(2);
        cluster.remove_node(node(2));
    }

    cluster.settle();

    // Stable nodes should NOT have ghost entries for the departed node
    let snap = cluster.engine(node(0)).snapshot();
    assert!(
        !snap.contains_key(&node(2)),
        "node 2 should have been removed from view map"
    );

    // Stable nodes should still be functional
    cluster.mutate(
        node(0),
        |s| s.counter = 999,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();
    let snap = cluster.engine(node(1)).snapshot();
    assert_eq!(snap[&node(0)].value.counter, 999);
}

// ── CHAOS-05: Split-brain partition (2+3) ───────────────────────

#[test]
fn chaos_05_split_brain_partition_heal() {
    let mut cluster = MockCluster::with_test_state(5, active_push_config());

    // Partition: {0,1} | {2,3,4}
    let partition = Partition::symmetric(
        [node(0), node(1)],
        [node(2), node(3), node(4)],
    );
    cluster
        .transport_mut()
        .add_interceptor(Box::new(partition));

    // Mutate in partition A
    cluster.mutate(
        node(0),
        |s| s.counter = 100,
        |s| s.clone(),
        SyncUrgency::Default,
    );

    // Mutate in partition B
    cluster.mutate(
        node(2),
        |s| s.counter = 200,
        |s| s.clone(),
        SyncUrgency::Default,
    );

    cluster.settle();

    // Partition A nodes see A's mutation but not B's
    let snap_0 = cluster.engine(node(0)).snapshot();
    assert_eq!(snap_0[&node(0)].value.counter, 100);
    let snap_1 = cluster.engine(node(1)).snapshot();
    assert_eq!(snap_1[&node(0)].value.counter, 100);

    // Partition B nodes see B's mutation but not A's
    let snap_3 = cluster.engine(node(3)).snapshot();
    assert_eq!(snap_3[&node(2)].value.counter, 200);

    // Heal partition
    cluster.transport_mut().clear_interceptors();

    // Mutate again to trigger re-sync across healed partition
    cluster.mutate(
        node(0),
        |s| s.counter = 101,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.mutate(
        node(2),
        |s| s.counter = 201,
        |s| s.clone(),
        SyncUrgency::Default,
    );

    cluster.settle();

    // Now all nodes should see both partitions' data
    for i in 0..5 {
        let snap = cluster.engine(node(i)).snapshot();
        assert_eq!(
            snap[&node(0)].value.counter, 101,
            "node {i} should see node 0's latest"
        );
        assert_eq!(
            snap[&node(2)].value.counter, 201,
            "node {i} should see node 2's latest"
        );
    }
}

// ── CHAOS-06: Clock skew between nodes ──────────────────────────
//
// MockCluster uses a shared TestClock, so individual clock skew isn't
// directly modeled. However, we can verify that the system uses
// monotonic ordering (generation-based) rather than wall-clock timestamps
// for state ordering. We advance the shared clock and verify generations
// are used correctly.

#[test]
fn chaos_06_generation_ordering_independent_of_clock() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock.clone(), tick_duration, 42);

    for i in 0..3 {
        let engine = DistributedStateEngine::new(
            TestState::name(),
            node(i),
            TestState::default(),
            |s| s.clone(),
            active_push_config(),
            TestState::WIRE_VERSION,
            clock.clone(),
            |v| bincode::serialize(v).unwrap(),
            |b, v| {
                if v != TestState::WIRE_VERSION {
                    return Err(DeserializeError::unknown_version(v));
                }
                bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        );
        cluster.add_node(engine, TestState::default());
    }

    // Node 0 mutates at t=0
    cluster.mutate(
        node(0),
        |s| s.counter = 1,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    // Advance clock by 5 seconds (simulate skew)
    clock.advance(Duration::from_secs(5));

    // Node 0 mutates again at t=5s
    cluster.mutate(
        node(0),
        |s| s.counter = 2,
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    // All peers should have counter=2 (latest generation wins, not wall-clock)
    for i in 1..3 {
        let snap = cluster.engine(node(i)).snapshot();
        assert_eq!(snap[&node(0)].value.counter, 2);
        // Generation age should be 2 (two mutations)
        assert_eq!(snap[&node(0)].generation.age, 2);
    }
}
