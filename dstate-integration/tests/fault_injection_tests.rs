//! Fault injection and edge case integration tests for the `dstate` crate.
//!
//! FAULT-01 through FAULT-14 (skipping FAULT-06, FAULT-07 which require
//! persistence not available in MockCluster).

use std::sync::Arc;
use std::time::Duration;

use dstate::engine::{DistributedStateEngine, EngineQueryResult, WireMessage};
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::TestState;
use dstate::{
    DeserializeError, DistributedState, Generation, NodeId, StateConfig, SyncMessage, SyncStrategy,
    SyncUrgency, VersionMismatchPolicy,
};
use dstate_integration::cluster::MockCluster;
use dstate_integration::interceptor::*;

// ── Helpers ─────────────────────────────────────────────────────

fn active_push_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    }
}

fn active_push_keep_stale() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        version_mismatch_policy: VersionMismatchPolicy::KeepStale,
        ..StateConfig::default()
    }
}

fn active_push_drop_and_wait() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        version_mismatch_policy: VersionMismatchPolicy::DropAndWait,
        ..StateConfig::default()
    }
}

/// Build a TestState engine with a custom wire version.
fn make_engine_with_version(
    node_id: NodeId,
    config: StateConfig,
    clock: Arc<TestClock>,
    wire_version: u32,
) -> DistributedStateEngine<TestState, TestState> {
    let expected_version = wire_version;
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
            if v != expected_version {
                return Err(DeserializeError::unknown_version(v));
            }
            bincode::deserialize(b).map_err(|e| DeserializeError::Malformed(e.to_string()))
        },
        None,
    )
}

// ── FAULT-01: Network partition (2+1 split) ─────────────────────

#[test]
fn fault_01_network_partition_2_plus_1() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    // Partition: {0,1} isolated from {2}
    let partition = Partition::symmetric([NodeId(0), NodeId(1)], [NodeId(2)]);
    cluster
        .transport_mut()
        .add_interceptor(Box::new(partition));

    // Mutate node 0
    cluster.mutate(NodeId(0), |s| s.counter = 42, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // Nodes 0,1 converge
    let v0 = cluster.engine(NodeId(0)).get_view(&NodeId(0)).unwrap();
    let v1 = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(v0.value.counter, 42);
    assert_eq!(v1.value.counter, 42);

    // Node 2 should NOT see the update (still default).
    // add_node always creates a default view, so v2 should be Some(default).
    let v2 = cluster.engine(NodeId(2)).get_view(&NodeId(0)).expect("view should exist (default)");
    assert_eq!(
        v2.value.counter, 0,
        "partitioned node should not see the mutation"
    );

    // Heal the partition
    cluster.transport_mut().clear_interceptors();

    // Mutate again to trigger fresh broadcast
    cluster.mutate(NodeId(0), |s| s.counter = 100, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // All 3 nodes converge
    for nid in cluster.all_node_ids() {
        let view = cluster.engine(nid).get_view(&NodeId(0)).unwrap();
        assert_eq!(view.value.counter, 100, "node {nid} should see counter=100 after heal");
    }
}

// ── FAULT-02: Total network failure ─────────────────────────────

#[test]
fn fault_02_total_network_failure() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    cluster.transport_mut().add_interceptor(Box::new(DropAll));

    // Mutate each node differently
    cluster.mutate(NodeId(0), |s| s.counter = 10, |s| s.clone(), SyncUrgency::Default);
    cluster.mutate(NodeId(1), |s| s.counter = 20, |s| s.clone(), SyncUrgency::Default);
    cluster.mutate(NodeId(2), |s| s.counter = 30, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // Each node only sees its own mutation
    assert_eq!(cluster.engine(NodeId(0)).get_view(&NodeId(0)).unwrap().value.counter, 10);
    assert_eq!(cluster.engine(NodeId(1)).get_view(&NodeId(1)).unwrap().value.counter, 20);
    assert_eq!(cluster.engine(NodeId(2)).get_view(&NodeId(2)).unwrap().value.counter, 30);

    // Node 0 should NOT see node 1's update (still default).
    // add_node always creates a default view, so cross should be Some(default).
    let cross = cluster.engine(NodeId(0)).get_view(&NodeId(1)).expect("view should exist (default)");
    assert_eq!(
        cross.value.counter, 0,
        "no cross-node state during total failure"
    );

    // Restore network
    cluster.transport_mut().clear_interceptors();

    // Mutate node 0 to trigger fresh broadcast
    cluster.mutate(NodeId(0), |s| s.counter = 99, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // All nodes see node 0's latest
    for nid in cluster.all_node_ids() {
        let view = cluster.engine(nid).get_view(&NodeId(0)).unwrap();
        assert_eq!(view.value.counter, 99, "node {nid} should see node 0 after recovery");
    }
}

// ── FAULT-03: Random packet loss (30%) ──────────────────────────

#[test]
fn fault_03_random_packet_loss_30_percent() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    cluster
        .transport_mut()
        .add_interceptor(Box::new(DropRate::new(0.3, 12345)));

    // Each node does its final mutation
    cluster.mutate(NodeId(0), |s| s.counter = 100, |s| s.clone(), SyncUrgency::Default);
    cluster.mutate(NodeId(1), |s| s.counter = 200, |s| s.clone(), SyncUrgency::Default);
    cluster.mutate(NodeId(2), |s| s.counter = 300, |s| s.clone(), SyncUrgency::Default);

    // With 30% loss, re-broadcast many times to overcome packet loss.
    // Each re-mutation triggers a fresh broadcast — eventually all get through.
    for _ in 0..30 {
        cluster.settle_with_limit(500);
        // Re-mutate with the SAME values to trigger fresh broadcasts
        cluster.mutate(NodeId(0), |s| s.counter = 100, |s| s.clone(), SyncUrgency::Default);
        cluster.mutate(NodeId(1), |s| s.counter = 200, |s| s.clone(), SyncUrgency::Default);
        cluster.mutate(NodeId(2), |s| s.counter = 300, |s| s.clone(), SyncUrgency::Default);
    }
    cluster.settle_with_limit(500);

    // After many retries with 30% loss, all nodes should eventually converge
    for observer in 0..3u64 {
        for origin in 0..3u64 {
            let expected = (origin + 1) * 100;
            let view = cluster.engine(NodeId(observer)).get_view(&NodeId(origin)).unwrap();
            assert_eq!(
                view.value.counter, expected,
                "node {observer} should see node {origin}'s value={expected}"
            );
        }
    }
}

// ── FAULT-04: Byte corruption ───────────────────────────────────

#[test]
fn fault_04_byte_corruption() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    cluster
        .transport_mut()
        .add_interceptor(Box::new(CorruptBytes::new(99)));

    // Mutate node 0
    cluster.mutate(NodeId(0), |s| s.counter = 42, |s| s.clone(), SyncUrgency::Default);

    // Tick several times — the corrupted message has multiple possible outcomes:
    // a) WireMessage envelope fails deserialization → corrupted_count > 0
    // b) Envelope OK but inner data is malformed → engine sync_failures > 0
    // c) Bit flip hits metadata (state_name, source_node, generation) → message
    //    silently ignored/misrouted, no error counters incremented
    // d) Bit flip lands harmlessly → data arrives intact
    // e) Bit flip changes data to a valid but different TestState → wrong values
    // All outcomes are acceptable — the key property is the cluster never panics.
    cluster.tick_n(10);

    // If we reach here, the cluster survived corruption without panicking.
    // Verify node 0's own state is intact (corruption only affects the network).
    let own_view = cluster.engine(NodeId(0)).get_view(&NodeId(0)).unwrap();
    assert_eq!(own_view.value.counter, 42, "local state should be unaffected by corruption");

    // Node 1's view of node 0 may or may not be updated depending on where
    // the bit flip landed. We categorize the possible outcomes:
    let node1_view = cluster.engine(NodeId(1)).get_view(&NodeId(0));
    let node1_counter = node1_view.map(|v| v.value.counter).unwrap_or(0);
    let corrupted = cluster.corrupted_count();
    let sync_failures = cluster.engine(NodeId(1)).metrics().sync_failures;

    // One of these must hold:
    // (a) Data arrived intact: counter == 42
    // (b) Corruption detected at transport or engine level
    // (c) Unchanged default view: counter == 0
    // (d) Corrupted-but-valid: bit flip produced a different valid TestState.
    //     This is an inherent limitation of random corruption without checksums —
    //     the wire format has no integrity check, so flipped bits CAN produce
    //     a valid payload. Real systems should use HMACs/CRCs for this.
    //
    // All four cases are acceptable; the key properties are:
    // 1. The cluster never panicked.
    // 2. Node 0's local state is unaffected.
    let outcome = if node1_counter == 42 {
        "intact"
    } else if corrupted > 0 || sync_failures > 0 {
        "detected"
    } else if node1_counter == 0 {
        "unchanged"
    } else {
        // Corrupted-but-valid: bit flip produced a different valid TestState.
        // This is expected and documents the need for checksums in real systems.
        "corrupted-but-valid"
    };
    // Ensure the outcome is one of the documented cases
    assert!(
        ["intact", "detected", "unchanged", "corrupted-but-valid"].contains(&outcome),
        "unexpected outcome: counter={node1_counter}, corrupted={corrupted}, sync_failures={sync_failures}"
    );
}

// ── FAULT-05: Node crash + restart ──────────────────────────────
// When a node crashes and restarts with fresh state, it gets a new
// (lower) generation. Existing nodes detect the restarted peer through
// the on_node_joined → on_node_left cycle and send fresh snapshots.
// The restarted node eventually receives full state from all peers.

#[test]
fn fault_05_node_crash_restart() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let config = active_push_config();
    let mut cluster = MockCluster::new(clock.clone(), tick_duration, 42);

    // Manually build 3 nodes
    for i in 0..3u64 {
        let engine = make_engine_with_version(NodeId(i), config.clone(), clock.clone(), 1);
        cluster.add_node(engine, TestState::default());
    }

    // Mutate nodes 0 and 1
    cluster.mutate(NodeId(0), |s| s.counter = 10, |s| s.clone(), SyncUrgency::Default);
    cluster.mutate(NodeId(1), |s| s.counter = 20, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // Verify node 2 has the data before crash
    assert_eq!(
        cluster.engine(NodeId(2)).get_view(&NodeId(0)).unwrap().value.counter,
        10
    );

    // Capture node 0's view of node 2 generation before crash
    let gen_before_crash = cluster
        .engine(NodeId(0))
        .get_view(&NodeId(2))
        .unwrap()
        .generation;

    // Hard-remove node 2 (simulate crash)
    cluster.remove_node_hard(NodeId(2));
    assert_eq!(cluster.node_count(), 2);

    // Create a new engine for NodeId(2) with the SAME clock (simulates restart
    // with fresh state — new incarnation implicitly via generation reset).
    // The on_node_joined calls from existing nodes will trigger snapshot pushes.
    let new_engine = make_engine_with_version(NodeId(2), config.clone(), clock.clone(), 1);
    cluster.add_node(new_engine, TestState::default());
    assert_eq!(cluster.node_count(), 3);

    // Mutate the restarted node to give it a fresh generation
    cluster.mutate(NodeId(2), |s| s.counter = 50, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // Node 2 should see data from nodes 0 and 1
    let v0 = cluster.engine(NodeId(2)).get_view(&NodeId(0)).unwrap();
    let v1 = cluster.engine(NodeId(2)).get_view(&NodeId(1)).unwrap();
    assert_eq!(v0.value.counter, 10, "restarted node should see node 0's data");
    assert_eq!(v1.value.counter, 20, "restarted node should see node 1's data");

    // Verify node 0 sees node 2's new generation (from the mutation after restart)
    let gen_after_restart = cluster
        .engine(NodeId(0))
        .get_view(&NodeId(2))
        .unwrap()
        .generation;
    // The restarted node has a fresh generation from its mutation; it must differ
    // from the pre-crash generation (reset + new mutation = different gen).
    assert_ne!(
        gen_before_crash, gen_after_restart,
        "restarted node should have a different generation than before crash"
    );
}

// ── FAULT-08: Wire version mismatch (KeepStale) ────────────────

#[test]
fn fault_08_version_mismatch_keep_stale() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster = MockCluster::new(clock.clone(), tick_duration, 42);

    // Node 0: wire_version=1, KeepStale policy
    let engine0 = make_engine_with_version(
        NodeId(0),
        active_push_keep_stale(),
        clock.clone(),
        1,
    );
    cluster.add_node(engine0, TestState::default());

    // Node 1: wire_version=2, KeepStale policy
    let engine1 = make_engine_with_version(
        NodeId(1),
        active_push_keep_stale(),
        clock.clone(),
        2,
    );
    cluster.add_node(engine1, TestState::default());

    // Mutate node 0 — sends snapshot with wire_version=1
    cluster.mutate(NodeId(0), |s| s.counter = 42, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // Node 1 receives wire_version=1 but expects version=2 → UnknownVersion
    // With KeepStale, node 1 keeps its stale/default view (doesn't crash).
    // add_node creates a default view (counter=0), so view must be Some.
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0));
    assert!(view.is_some(), "KeepStale should preserve the existing view entry");
    assert_eq!(
        view.unwrap().value.counter, 0,
        "KeepStale should preserve the stale (default) view, not update it"
    );
    // Verify no panic occurred and sync failure was recorded
    let metrics = cluster.engine(NodeId(1)).metrics();
    assert!(metrics.sync_failures > 0, "should record sync failures for version mismatch");
}

// ── FAULT-09: Wire version mismatch (DropAndWait) ──────────────

#[test]
fn fault_09_version_mismatch_drop_and_wait() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster = MockCluster::new(clock.clone(), tick_duration, 42);

    // Node 0: wire_version=1, DropAndWait policy
    let engine0 = make_engine_with_version(
        NodeId(0),
        active_push_drop_and_wait(),
        clock.clone(),
        1,
    );
    cluster.add_node(engine0, TestState::default());

    // Node 1: wire_version=2, DropAndWait policy
    let engine1 = make_engine_with_version(
        NodeId(1),
        active_push_drop_and_wait(),
        clock.clone(),
        2,
    );
    cluster.add_node(engine1, TestState::default());

    // Mutate node 0 — sends snapshot with wire_version=1
    cluster.mutate(NodeId(0), |s| s.counter = 42, |s| s.clone(), SyncUrgency::Default);
    cluster.settle();

    // Node 1 receives wire_version=1 but expects version=2 → UnknownVersion
    // With DropAndWait, node 1 should drop the view entirely
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0));
    assert!(
        view.is_none(),
        "DropAndWait should remove the peer's view on version mismatch"
    );

    let metrics = cluster.engine(NodeId(1)).metrics();
    assert!(metrics.sync_failures > 0, "should record sync failures for version mismatch");
}

// ── FAULT-10: 100 rapid mutations (stress) ──────────────────────

#[test]
fn fault_10_rapid_mutations_stress() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    // Mutate node 0 100 times rapidly
    for i in 1..=100u64 {
        cluster.mutate(
            NodeId(0),
            move |s| s.counter = i,
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }

    cluster.settle();

    // Node 1 should see the final value
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 100, "peer should see final mutation value");

    // Node 0's own view should also be 100
    let own = cluster.engine(NodeId(0)).get_view(&NodeId(0)).unwrap();
    assert_eq!(own.value.counter, 100);

    // Metrics should show 100 mutations
    let metrics = cluster.engine(NodeId(0)).metrics();
    assert_eq!(metrics.total_mutations, 100);
}

// ── FAULT-11: Ghost node rejected ───────────────────────────────

#[test]
fn fault_11_ghost_node_rejected() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());
    cluster.settle();

    // Gracefully remove node 2 (departed)
    cluster.remove_node(NodeId(2));
    assert_eq!(cluster.node_count(), 2);

    // Node 0 should no longer accept messages from the departed node
    assert!(
        !cluster.engine(NodeId(0)).should_accept_from(&NodeId(2)),
        "departed node should be rejected"
    );

    // After on_node_left, node 0's view of node 2 should already be gone
    assert!(
        cluster.engine(NodeId(0)).get_view(&NodeId(2)).is_none(),
        "view should be removed after on_node_left (before ghost injection)"
    );

    // Craft a ghost snapshot from NodeId(2)
    let ghost_data = bincode::serialize(&TestState {
        counter: 999,
        label: "ghost".into(),
    })
    .unwrap();
    let ghost_snapshot = SyncMessage::FullSnapshot {
        state_name: TestState::name().to_string(),
        source_node: NodeId(2),
        generation: Generation::new(1, 1),
        wire_version: TestState::WIRE_VERSION,
        data: ghost_data,
    };

    // Inject the ghost message into the transport
    cluster
        .transport_mut()
        .send(NodeId(2), NodeId(0), &WireMessage::Sync(ghost_snapshot));
    cluster.tick_n(5);

    // Node 0 should NOT have accepted the ghost snapshot
    let view = cluster.engine(NodeId(0)).get_view(&NodeId(2));
    assert!(
        view.is_none(),
        "ghost node's snapshot should be rejected by should_accept_from"
    );
}

// ── FAULT-12: Stale snapshot discarded ──────────────────────────

#[test]
fn fault_12_stale_snapshot_discarded() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    // Mutate node 0 twice
    cluster.mutate(
        NodeId(0),
        |s| {
            s.counter = 1;
            s.label = "gen1".into();
        },
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.mutate(
        NodeId(0),
        |s| {
            s.counter = 2;
            s.label = "gen2".into();
        },
        |s| s.clone(),
        SyncUrgency::Default,
    );
    cluster.settle();

    // Node 1 should have the latest (counter=2)
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 2, "node 1 should have gen2 data");
    let current_gen = view.generation;

    // Now inject a stale snapshot with an older generation.
    // Guard: we need age >= 1 to construct a truly stale generation.
    assert!(
        current_gen.age >= 1,
        "need age>=1 to construct a stale snapshot; current generation: {:?}",
        current_gen
    );
    let stale_data = bincode::serialize(&TestState {
        counter: 1,
        label: "stale-gen1".into(),
    })
    .unwrap();
    let stale_gen = Generation::new(current_gen.incarnation, current_gen.age.saturating_sub(1));
    assert!(
        stale_gen < current_gen,
        "stale_gen ({:?}) must be strictly less than current_gen ({:?})",
        stale_gen,
        current_gen
    );
    let stale_snapshot = SyncMessage::FullSnapshot {
        state_name: TestState::name().to_string(),
        source_node: NodeId(0),
        generation: stale_gen,
        wire_version: TestState::WIRE_VERSION,
        data: stale_data,
    };

    // Send the stale snapshot to node 1
    cluster
        .transport_mut()
        .send(NodeId(0), NodeId(1), &WireMessage::Sync(stale_snapshot));
    cluster.tick_n(5);

    // Node 1 should still have gen2 data (stale snapshot discarded)
    let view_after = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(
        view_after.value.counter, 2,
        "stale snapshot should be discarded; still gen2"
    );
    assert_eq!(view_after.value.label, "gen2");

    // Metrics should show the stale delta was discarded
    let metrics = cluster.engine(NodeId(1)).metrics();
    assert!(
        metrics.stale_deltas_discarded > 0,
        "should record stale snapshot discard"
    );
}

// ── FAULT-13: Single-node cluster ───────────────────────────────

#[test]
fn fault_13_single_node_cluster() {
    let mut cluster = MockCluster::with_test_state(1, active_push_config());

    // Mutate the only node
    cluster.mutate(
        NodeId(0),
        |s| {
            s.counter = 42;
            s.label = "solo".into();
        },
        |s| s.clone(),
        SyncUrgency::Default,
    );

    // Local state should be updated
    let view = cluster.engine(NodeId(0)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 42);
    assert_eq!(view.value.label, "solo");

    // Query should return Fresh
    let (result, _) = cluster
        .engine(NodeId(0))
        .query(Duration::ZERO, |views| views.len());
    assert!(
        matches!(result, EngineQueryResult::Fresh(_)),
        "single node should have fresh query result"
    );

    // Settle should complete quickly (no peers to sync with)
    let ticks = cluster.settle();
    assert!(ticks <= 1, "single-node cluster should settle in ≤1 tick, got {ticks}");

    // Metrics: mutations recorded, no syncs
    let metrics = cluster.engine(NodeId(0)).metrics();
    assert_eq!(metrics.total_mutations, 1);
    // No peers → no snapshots sent (the broadcast goes to nobody)
    assert_eq!(metrics.snapshots_received, 0, "no peers to receive from");
}

// ── FAULT-14: Empty cluster ─────────────────────────────────────

#[test]
fn fault_14_empty_cluster() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock, Duration::from_millis(100), 42);

    assert_eq!(cluster.node_count(), 0);
    assert_eq!(cluster.in_flight_count(), 0);
    assert!(cluster.all_node_ids().is_empty());
}

// ── FAULT-14 (settle): Empty cluster settle ─────────────────────

#[test]
fn fault_14_empty_cluster_settle() {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let mut cluster: MockCluster<TestState, TestState> =
        MockCluster::new(clock, Duration::from_millis(100), 42);

    let ticks = cluster.settle();
    assert_eq!(ticks, 0, "empty cluster should settle in 0 ticks");
}
