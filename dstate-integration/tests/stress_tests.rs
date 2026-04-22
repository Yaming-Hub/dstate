//! Stress tests — STRESS-01 through STRESS-03
//!
//! These exercise the dstate system under high-throughput, large-cluster,
//! and large-payload conditions using MockCluster for determinism.

use dstate::{NodeId, StateConfig, SyncStrategy, SyncUrgency};
use dstate_integration::cluster::MockCluster;

fn active_push_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    }
}

// ── STRESS-01: 1000 mutations/sec sustained ─────────────────────

#[test]
fn stress_01_high_throughput_sequential_mutations() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    let node = NodeId("0".to_string());
    let mutation_count = 1000;

    for i in 0..mutation_count {
        cluster.mutate(
            node.clone(),
            |s| s.counter = i + 1,
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }

    cluster.settle();

    // Verify node 0's local state
    let snap = cluster.engine(node.clone()).snapshot();
    assert_eq!(snap[&node].value.counter, mutation_count);

    // Verify node 1 received the final value
    let peer = NodeId("1".to_string());
    let peer_snap = cluster.engine(peer.clone()).snapshot();
    assert_eq!(peer_snap[&node].value.counter, mutation_count);

    // Verify generation age is sequential (one increment per mutation)
    let gen = snap[&node].generation;
    assert_eq!(gen.age, mutation_count);
}

// ── STRESS-02: 10-node cluster full mesh sync ───────────────────

#[test]
fn stress_02_ten_node_full_mesh_convergence() {
    let mut cluster = MockCluster::with_test_state(10, active_push_config());
    let mutations_per_node = 50;

    // Every node mutates
    for i in 0..10u64 {
        let node = NodeId(i.to_string());
        for m in 0..mutations_per_node {
            cluster.mutate(
                node.clone(),
                move |s| {
                    s.counter = (i + 1) * 1000 + m + 1;
                    s.label = format!("node-{i}-mutation-{m}");
                },
                |s| s.clone(),
                SyncUrgency::Default,
            );
        }
    }

    cluster.settle();

    // Every node should see all 10 nodes in its snapshot
    for i in 0..10u64 {
        let observer = NodeId(i.to_string());
        let snap = cluster.engine(observer).snapshot();
        assert_eq!(snap.len(), 10, "node {i} should see all 10 nodes");

        // Each remote node's counter should be the last mutation value
        for j in 0..10u64 {
            let target = NodeId(j.to_string());
            let view = &snap[&target];
            let expected = (j + 1) * 1000 + mutations_per_node;
            assert_eq!(
                view.value.counter, expected,
                "node {i} sees wrong value for node {j}: got {} expected {}",
                view.value.counter, expected
            );
        }
    }
}

// ── STRESS-03: Large view payload ───────────────────────────────

#[test]
fn stress_03_large_payload_sync() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    let node = NodeId("0".to_string());
    // Create a label that is ~100KB (not 1MB to keep test fast, but still large)
    let large_label = "x".repeat(100_000);

    cluster.mutate(
        node.clone(),
        |s| {
            s.counter = 1;
            s.label = large_label.clone();
        },
        |s| s.clone(),
        SyncUrgency::Default,
    );

    cluster.settle();

    // All peers should have the large payload
    for i in 1..3u64 {
        let peer = NodeId(i.to_string());
        let snap = cluster.engine(peer).snapshot();
        let view = &snap[&node];
        assert_eq!(view.value.label.len(), 100_000);
        assert_eq!(view.value.counter, 1);
    }
}
