use std::collections::HashSet;
use std::time::Duration;

use dstate::engine::EngineQueryResult;
use dstate::{NodeId, StateConfig, SyncStrategy, SyncUrgency};

use crate::cluster::TestCluster;
use crate::interceptor::{CorruptBytes, DelayAll, DropAll, DropRate, Partition};
use crate::TestableRuntime;

// ── Config helpers ──────────────────────────────────────────────────

pub fn active_push_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..Default::default()
    }
}

pub fn periodic_only_config(interval: Duration) -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::periodic_only(interval),
        ..Default::default()
    }
}

pub fn feed_lazy_pull_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::feed_lazy_pull(),
        ..Default::default()
    }
}

pub fn periodic_push_config(interval: Duration) -> StateConfig {
    let mut strategy = SyncStrategy::active_push();
    strategy.periodic_full_sync = Some(interval);
    StateConfig {
        sync_strategy: strategy,
        ..Default::default()
    }
}

const TIMEOUT: Duration = Duration::from_secs(5);

// ── E2E-01: Basic mutation propagates ───────────────────────────────

pub async fn test_basic_mutation<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 42, TIMEOUT)
        .await;
    assert!(ok, "node 1 should see node 0's counter = 42");

    cluster.shutdown().await;
}

// ── E2E-02: Bidirectional sync ──────────────────────────────────────

pub async fn test_bidirectional_sync<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 10, SyncUrgency::Default)
        .await;
    cluster
        .mutate(NodeId(1), |s| s.counter = 20, SyncUrgency::Default)
        .await;

    let ok0 = cluster
        .wait_for_value(NodeId(0), NodeId(1), 20, TIMEOUT)
        .await;
    let ok1 = cluster
        .wait_for_value(NodeId(1), NodeId(0), 10, TIMEOUT)
        .await;

    assert!(ok0, "node 0 should see node 1's counter = 20");
    assert!(ok1, "node 1 should see node 0's counter = 10");

    cluster.shutdown().await;
}

// ── E2E-03: Multi-node fan-out ──────────────────────────────────────

pub async fn test_multi_node_fanout<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(4, config).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 99, SyncUrgency::Default)
        .await;

    for i in 1..4 {
        let ok = cluster
            .wait_for_value(NodeId(i), NodeId(0), 99, TIMEOUT)
            .await;
        assert!(
            ok,
            "node {} should see node 0's counter = 99",
            i
        );
    }

    cluster.shutdown().await;
}

// ── E2E-04: Delta sync round-trip (counter + label) ────────────────

pub async fn test_delta_sync_roundtrip<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    cluster
        .mutate(
            NodeId(0),
            |s| {
                s.counter = 7;
                s.label = "hello".into();
            },
            SyncUrgency::Default,
        )
        .await;

    let ok = cluster
        .wait_for_convergence(
            |snapshot| {
                snapshot
                    .get(&NodeId(0))
                    .map_or(false, |v| v.value.counter == 7 && v.value.label == "hello")
            },
            TIMEOUT,
        )
        .await;
    // Check specifically from node 1's perspective
    let snap = cluster.query(NodeId(1)).await;
    assert!(ok || snap.get(&NodeId(0)).map_or(false, |v| v.value.counter == 7 && v.value.label == "hello"),
        "node 1 should see node 0's counter=7 and label='hello'"
    );

    cluster.shutdown().await;
}

// ── E2E-05: Node join receives snapshot ─────────────────────────────

pub async fn test_node_join_snapshot<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config.clone()).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    // Wait for node 1 to get the value (ensures it has been synced)
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 42, TIMEOUT)
        .await;
    assert!(ok, "node 1 should see counter=42 before adding node 2");

    // Add a third node — it should receive the snapshot
    let mut cluster = cluster;
    cluster.add_node(NodeId(2)).await;

    let ok = cluster
        .wait_for_value(NodeId(2), NodeId(0), 42, TIMEOUT)
        .await;
    assert!(ok, "node 2 (newly joined) should see node 0's counter = 42");

    cluster.shutdown().await;
}

// ── E2E-06: Node leave cleans views ─────────────────────────────────

pub async fn test_node_leave_cleans_views<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(3, config).await;

    cluster
        .mutate(NodeId(2), |s| s.counter = 77, SyncUrgency::Default)
        .await;

    let ok = cluster
        .wait_for_value(NodeId(0), NodeId(2), 77, TIMEOUT)
        .await;
    assert!(ok, "node 0 should see node 2's counter=77");

    let mut cluster = cluster;
    cluster.remove_node(NodeId(2));

    // Give time for the leave to propagate
    tokio::time::sleep(Duration::from_millis(100)).await;

    let snap = cluster.query(NodeId(0)).await;
    assert!(
        !snap.contains_key(&NodeId(2)),
        "node 0 should no longer have a view for removed node 2"
    );

    cluster.shutdown().await;
}

// ── E2E-07: Periodic sync ───────────────────────────────────────────

pub async fn test_periodic_sync<R: TestableRuntime>() {
    let config = periodic_only_config(Duration::from_millis(200));
    let cluster = TestCluster::<R>::new(2, config).await;

    // Mutate — with periodic_only, no immediate push
    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    // Wait long enough for at least one periodic sync to fire
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 42, Duration::from_secs(5))
        .await;
    assert!(ok, "node 1 should see counter=42 after periodic sync");

    cluster.shutdown().await;
}

// ── E2E-08: Feed + lazy pull ────────────────────────────────────────

pub async fn test_feed_lazy_pull<R: TestableRuntime>() {
    let config = feed_lazy_pull_config();
    let cluster = TestCluster::<R>::new(2, config).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 55, SyncUrgency::Default)
        .await;

    // Wait for the change feed flush interval + processing
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Query with freshness to trigger pull
    let result = cluster
        .query_fresh(NodeId(1), Duration::from_millis(100))
        .await;

    // Even if initially stale, wait for the pull to complete
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 55, TIMEOUT)
        .await;
    assert!(
        ok,
        "node 1 should eventually see counter=55 via feed+pull. query result: {:?}",
        matches!(result, EngineQueryResult::Fresh(_))
    );

    cluster.shutdown().await;
}

// ── E2E-09: Concurrent mutations ────────────────────────────────────

pub async fn test_concurrent_mutations<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(3, config).await;

    // All three nodes mutate concurrently
    tokio::join!(
        cluster.mutate(NodeId(0), |s| s.counter = 100, SyncUrgency::Default),
        cluster.mutate(NodeId(1), |s| s.counter = 200, SyncUrgency::Default),
        cluster.mutate(NodeId(2), |s| s.counter = 300, SyncUrgency::Default),
    );

    // All nodes should converge: each sees the others' counters
    let ok = cluster
        .wait_for_convergence(
            |snap| {
                snap.get(&NodeId(0)).map_or(false, |v| v.value.counter == 100)
                    && snap.get(&NodeId(1)).map_or(false, |v| v.value.counter == 200)
                    && snap.get(&NodeId(2)).map_or(false, |v| v.value.counter == 300)
            },
            TIMEOUT,
        )
        .await;
    assert!(ok, "all nodes should converge after concurrent mutations");

    cluster.shutdown().await;
}

// ── E2E-10: Query freshness ─────────────────────────────────────────

pub async fn test_query_freshness<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 1, SyncUrgency::Default)
        .await;

    // Wait for sync
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 1, TIMEOUT)
        .await;
    assert!(ok, "sync should complete");

    // Query with generous staleness — should be Fresh
    let result = cluster
        .query_fresh(NodeId(1), Duration::from_secs(60))
        .await;
    assert!(
        matches!(result, EngineQueryResult::Fresh(_)),
        "query should be Fresh after recent sync"
    );

    cluster.shutdown().await;
}

// ── E2E-11: Multiple rapid mutations ────────────────────────────────

pub async fn test_multiple_mutations<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    for i in 1..=10 {
        cluster
            .mutate(NodeId(0), move |s| s.counter = i, SyncUrgency::Default)
            .await;
    }

    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 10, TIMEOUT)
        .await;
    assert!(ok, "node 1 should see final counter = 10");

    cluster.shutdown().await;
}

// ── E2E-12: Cluster events — NodeJoined triggers snapshot push ──────

pub async fn test_cluster_events<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config.clone()).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 88, SyncUrgency::Default)
        .await;

    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 88, TIMEOUT)
        .await;
    assert!(ok, "pre-condition: node 1 sees counter=88");

    // Add node 2 — triggers NodeJoined event and snapshot push
    let mut cluster = cluster;
    cluster.add_node(NodeId(2)).await;

    let ok = cluster
        .wait_for_value(NodeId(2), NodeId(0), 88, TIMEOUT)
        .await;
    assert!(
        ok,
        "new node 2 should receive snapshot via cluster join event"
    );

    cluster.shutdown().await;
}

// ── E2E-F01: Network partition ──────────────────────────────────────

pub async fn test_network_partition<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    // Inject partition
    let partition = Partition::new(
        [NodeId(0)].into_iter().collect::<HashSet<_>>(),
        [NodeId(1)].into_iter().collect::<HashSet<_>>(),
    );
    cluster
        .transport()
        .add_interceptor(Box::new(partition));

    // Mutate under partition
    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    // Node 1 should NOT see the mutation
    tokio::time::sleep(Duration::from_millis(200)).await;
    let snap = cluster.query(NodeId(1)).await;
    let isolated = snap
        .get(&NodeId(0))
        .map_or(true, |v| v.value.counter != 42);
    assert!(isolated, "partition should isolate node 1 from node 0's mutation");

    // Heal partition
    cluster.transport().clear_interceptors();

    // Mutate again to trigger a push
    cluster
        .mutate(NodeId(0), |s| s.counter = 43, SyncUrgency::Default)
        .await;

    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 43, TIMEOUT)
        .await;
    assert!(ok, "after healing, node 1 should see counter=43");

    cluster.shutdown().await;
}

// ── E2E-F02: Total network failure ──────────────────────────────────

pub async fn test_total_network_failure<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    // Drop all messages
    cluster
        .transport()
        .add_interceptor(Box::new(DropAll));

    cluster
        .mutate(NodeId(0), |s| s.counter = 10, SyncUrgency::Default)
        .await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    let snap = cluster.query(NodeId(1)).await;
    let isolated = snap
        .get(&NodeId(0))
        .map_or(true, |v| v.value.counter != 10);
    assert!(isolated, "total failure should isolate nodes");

    // Restore network and mutate again
    cluster.transport().clear_interceptors();
    cluster
        .mutate(NodeId(0), |s| s.counter = 11, SyncUrgency::Default)
        .await;

    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 11, TIMEOUT)
        .await;
    assert!(ok, "after restoring network, node 1 should see counter=11");

    cluster.shutdown().await;
}

// ── E2E-F03: Packet loss with periodic sync ─────────────────────────

pub async fn test_packet_loss<R: TestableRuntime>() {
    let mut config = periodic_push_config(Duration::from_millis(200));
    config.sync_strategy.periodic_full_sync = Some(Duration::from_millis(200));
    let cluster = TestCluster::<R>::new(2, config).await;

    // 30% packet loss
    cluster
        .transport()
        .add_interceptor(Box::new(DropRate::new(0.3, 42)));

    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    // With periodic sync, should eventually converge despite loss
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 42, Duration::from_secs(10))
        .await;
    assert!(
        ok,
        "should converge despite 30% packet loss (periodic sync helps)"
    );

    cluster.shutdown().await;
}

// ── E2E-F04: Corruption ─────────────────────────────────────────────

pub async fn test_corruption<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    // Corrupt all messages
    cluster
        .transport()
        .add_interceptor(Box::new(CorruptBytes::new(123)));

    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    // Should not panic — just silently discard corrupted messages
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Local state should still be intact
    let _snap = cluster.query(NodeId(0)).await;
    // Node 0 doesn't see itself in snapshot (that's peer views), so check metrics
    let metrics = cluster.get_metrics(NodeId(0)).await;
    assert!(metrics.total_mutations >= 1, "local mutation should succeed");

    // Node 1 may or may not have received valid data (bit flip might not break deserialization)
    // The important assertion is that no panic occurred and local state is intact
    let snap1 = cluster.query(NodeId(1)).await;
    if let Some(v) = snap1.get(&NodeId(0)) {
        // If data got through despite corruption, it's still consistent
        assert!(v.value.counter == 42 || v.value.counter == 0,
            "corrupted view should be either correct (lucky bit flip) or default");
    }

    cluster.shutdown().await;
}

// ── E2E-F05: Node crash + restart ───────────────────────────────────

pub async fn test_node_crash_restart<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config.clone()).await;

    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 42, TIMEOUT)
        .await;
    assert!(ok, "pre-condition: node 1 sees counter=42");

    // Crash node 1 (remove)
    let mut cluster = cluster;
    cluster.remove_node(NodeId(1));

    // Re-add node 1 — fresh state
    cluster.add_node(NodeId(1)).await;

    // After rejoin, node 1 should receive a snapshot from node 0
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 42, TIMEOUT)
        .await;
    assert!(
        ok,
        "restarted node 1 should get node 0's state via snapshot"
    );

    cluster.shutdown().await;
}

// ── E2E-F06: Message delay ──────────────────────────────────────────

pub async fn test_message_delay<R: TestableRuntime>(config: StateConfig) {
    let cluster = TestCluster::<R>::new(2, config).await;

    cluster
        .transport()
        .add_interceptor(Box::new(DelayAll::new(Duration::from_millis(200))));

    cluster
        .mutate(NodeId(0), |s| s.counter = 42, SyncUrgency::Default)
        .await;

    // With 200ms delay, give generous timeout
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 42, TIMEOUT)
        .await;
    assert!(ok, "delayed messages should still arrive");

    cluster.shutdown().await;
}

// ── E2E-F07: Rapid mutations under packet loss ──────────────────────

pub async fn test_rapid_mutations_under_loss<R: TestableRuntime>() {
    let config = periodic_push_config(Duration::from_millis(200));
    let cluster = TestCluster::<R>::new(2, config).await;

    // 10% loss
    cluster
        .transport()
        .add_interceptor(Box::new(DropRate::new(0.1, 99)));

    for i in 1..=50 {
        cluster
            .mutate(NodeId(0), move |s| s.counter = i, SyncUrgency::Default)
            .await;
    }

    // Periodic sync should eventually deliver the final state
    let ok = cluster
        .wait_for_value(NodeId(1), NodeId(0), 50, Duration::from_secs(10))
        .await;
    assert!(
        ok,
        "50 rapid mutations under 10% loss should converge to counter=50"
    );

    cluster.shutdown().await;
}
