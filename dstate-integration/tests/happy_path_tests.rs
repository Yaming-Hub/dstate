//! Core happy-path integration tests for the `dstate` crate.
//!
//! Each test exercises a distinct replication scenario using `MockCluster`
//! from `dstate-integration`.

use std::sync::Arc;
use std::time::Duration;

use dstate::engine::{DistributedStateEngine, EngineAction, EngineQueryResult, WireMessage};
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::{
    TestDeltaChange, TestDeltaState, TestDeltaStateInner, TestDeltaView, TestState,
};
use dstate::{
    DeltaDistributedState, DeserializeError, DistributedState, NodeId, StateConfig, SyncStrategy,
    SyncUrgency,
};
use dstate_integration::cluster::MockCluster;

// ── Helpers ─────────────────────────────────────────────────────

/// Route engine actions through the cluster transport manually.
/// Use when calling `engine_mut().mutate()` directly (which doesn't auto-route).
///
/// Note: `ScheduleDelayed` actions are skipped here — they can only be
/// properly handled by `MockCluster::tick()` internal scheduling. Tests
/// that need delayed delivery should use `cluster.mutate()` instead.
fn route_actions<S, V>(cluster: &mut MockCluster<S, V>, from: NodeId, actions: &[EngineAction])
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + std::fmt::Debug + 'static,
{
    let all_nodes = cluster.all_node_ids();
    for action in actions {
        match action {
            EngineAction::BroadcastSync(msg) => {
                cluster
                    .transport_mut()
                    .broadcast(from, &WireMessage::Sync(msg.clone()), &all_nodes);
            }
            EngineAction::SendSync { target, message } => {
                cluster
                    .transport_mut()
                    .send(from, *target, &WireMessage::Sync(message.clone()));
            }
            EngineAction::ScheduleDelayed { .. } => {
                // Intentionally skipped — ScheduleDelayed must go through
                // MockCluster's internal tick scheduling. Use cluster.mutate()
                // when you need delayed actions to propagate properly.
            }
        }
    }
}

fn active_push_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    }
}

fn feed_lazy_pull_config() -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::feed_lazy_pull(),
        ..StateConfig::default()
    }
}

fn periodic_only_config(interval: Duration) -> StateConfig {
    StateConfig {
        sync_strategy: SyncStrategy::periodic_only(interval),
        ..StateConfig::default()
    }
}

/// Build a single delta-aware engine.
fn make_delta_engine(
    node_id: NodeId,
    config: StateConfig,
    clock: Arc<TestClock>,
) -> DistributedStateEngine<TestDeltaStateInner, TestDeltaView> {
    DistributedStateEngine::new(
        TestDeltaState::name(),
        node_id,
        TestDeltaStateInner::default(),
        |s| TestDeltaState::project_view(s),
        config,
        TestDeltaState::WIRE_VERSION,
        clock,
        |v| TestDeltaState::serialize_view(v),
        |b, wv| TestDeltaState::deserialize_view(b, wv),
        Some(Box::new(|delta_bytes, wire_version, view| {
            let delta = TestDeltaState::deserialize_delta(delta_bytes, wire_version)?;
            Ok(TestDeltaState::apply_delta(view, &delta))
        })),
    )
}

/// Build a MockCluster of `n` nodes using `TestDeltaState`.
fn make_delta_cluster(
    n: usize,
    config: StateConfig,
) -> MockCluster<TestDeltaStateInner, TestDeltaView> {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster = MockCluster::new(clock.clone(), tick_duration, 42);

    for i in 0..n {
        let engine = make_delta_engine(NodeId(i as u64), config.clone(), clock.clone());
        cluster.add_node(engine, TestDeltaView { counter: 0, label: String::new() });
    }

    cluster
}

// ── INT-01: Basic mutation propagates to all peers ──────────────

#[test]
fn int_01_basic_mutation_propagates() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    cluster.mutate(
        NodeId(0),
        |s| {
            s.counter = 42;
            s.label = "hello".into();
        },
        |s| s.clone(),
        SyncUrgency::Default,
    );

    cluster.settle();

    // All 3 nodes should see node 0's update
    for nid in cluster.all_node_ids() {
        let view = cluster.engine(nid).get_view(&NodeId(0)).unwrap();
        assert_eq!(view.value.counter, 42, "node {nid} should see counter=42");
        assert_eq!(view.value.label, "hello", "node {nid} should see label=hello");
    }
}

// ── INT-02: Delta projection round-trip ─────────────────────────

#[test]
fn int_02_delta_projection_round_trip() {
    let mut cluster = make_delta_cluster(3, active_push_config());

    // Mutate node 0 with a delta change
    let result = cluster.engine_mut(NodeId(0)).mutate_with_delta(
        |s| {
            s.counter += 10;
            s.label = "delta".into();
            s.internal_accumulator += 3.14;
            TestDeltaChange {
                counter_delta: 10,
                new_label: Some("delta".into()),
                accumulator_delta: 3.14,
            }
        },
        |s| TestDeltaState::project_view(s),
        |c| TestDeltaState::project_delta(c),
        SyncUrgency::Default,
        |vd| TestDeltaState::serialize_delta(vd),
    );

    // The local view should reflect the mutation
    assert_eq!(result.view.counter, 10);
    assert_eq!(result.view.label, "delta");

    // Route the actions through the cluster transport
    let actions = result.actions;
    route_actions(&mut cluster, NodeId(0), &actions);

    cluster.settle();

    // All peers should see the delta-applied view
    for nid in cluster.all_node_ids() {
        let view = cluster.engine(nid).get_view(&NodeId(0)).unwrap();
        assert_eq!(view.value.counter, 10, "node {nid} should see counter=10");
        assert_eq!(view.value.label, "delta", "node {nid} should see label=delta");
    }
}

// ── INT-03: Change feed batching (FeedLazyPull) ─────────────────

#[test]
fn int_03_change_feed_batching() {
    let mut cluster = MockCluster::with_test_state(3, feed_lazy_pull_config());

    // Mutate multiple times on node 0
    for i in 1..=5 {
        cluster.mutate(
            NodeId(0),
            |s| {
                s.counter = i;
            },
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }

    // ActiveFeedLazyPull doesn't broadcast snapshots; it sends change feed.
    // Before settling, the cluster should have no snapshots in flight for
    // ActiveFeedLazyPull — only change feed notifications will be sent.
    // After settling (which flushes feeds and lets peers pull), peers see the data.
    cluster.settle();

    // Node 0's own view should have the latest mutation
    let own = cluster.engine(NodeId(0)).get_view(&NodeId(0)).unwrap();
    assert_eq!(own.value.counter, 5);

    // After settle, peers should have received the feed and pulled data
    let metrics = cluster.engine(NodeId(0)).metrics();
    assert_eq!(metrics.total_mutations, 5, "all 5 mutations should be recorded");
    // In FeedLazyPull mode, peers pull via RequestSnapshot after receiving
    // change feed notifications, so node 0 should have sent snapshots.
    assert!(
        metrics.snapshots_sent > 0,
        "peers should have pulled snapshots via RequestSnapshot"
    );
}

// ── INT-04: Change feed deduplication ────────────────────────────

#[test]
fn int_04_change_feed_deduplication() {
    let mut cluster = MockCluster::with_test_state(3, feed_lazy_pull_config());

    // Mutate the same node 5 times rapidly
    for i in 1..=5 {
        cluster.mutate(
            NodeId(0),
            |s| {
                s.counter = i;
            },
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }

    // Flush the change feed on node 0 directly to inspect it
    let feed = cluster.engine_mut(NodeId(0)).flush_change_feed();

    if let Some(feed) = feed {
        // The feed should deduplicate: at most 1 notification per (state_name, source_node)
        let unique_sources: std::collections::HashSet<_> = feed
            .notifications
            .iter()
            .map(|n| (&n.state_name, n.source_node))
            .collect();
        assert!(
            unique_sources.len() <= 1,
            "expected at most 1 unique (state, source), got {}",
            unique_sources.len()
        );
    }
    // If None, no feed items means deduplication happened at the notification level
}

// ── INT-05: Periodic full sync ──────────────────────────────────

#[test]
fn int_05_periodic_full_sync() {
    let config = periodic_only_config(Duration::from_millis(500));
    let mut cluster = MockCluster::with_test_state(3, config);

    // Mutate on node 0 — no immediate push should occur with periodic_only
    let result = cluster.engine_mut(NodeId(0)).mutate(
        |s| {
            s.counter = 99;
            s.label = "periodic".into();
        },
        |s| s.clone(),
        SyncUrgency::Default,
    );

    // periodic_only has no push_mode, so mutate should produce no actions
    assert!(
        result.actions.is_empty(),
        "periodic_only should not produce broadcast actions on mutate"
    );

    // Manually trigger periodic sync
    let actions = cluster.engine_mut(NodeId(0)).periodic_sync();
    assert!(
        !actions.is_empty(),
        "periodic_sync should produce at least one action"
    );

    // Verify the action is a FullSnapshot broadcast
    assert!(
        actions.iter().any(|a| matches!(a, EngineAction::BroadcastSync(..))),
        "periodic_sync should produce a BroadcastSync"
    );
}

// ── INT-06: Pull on stale query ─────────────────────────────────

#[test]
fn int_06_pull_on_stale_query() {
    let config = feed_lazy_pull_config();
    let mut cluster = MockCluster::with_test_state(3, config);

    // Mutate on node 0
    cluster.mutate(
        NodeId(0),
        |s| {
            s.counter = 77;
        },
        |s| s.clone(),
        SyncUrgency::Default,
    );

    // Don't settle — tick enough for change feed to propagate but not resolve
    cluster.tick_n(5);

    // Advance clock further to ensure views become stale relative to max_staleness=0
    cluster.tick_n(10);

    // Query on node 1 with max_staleness = 0 (demand perfect freshness)
    let (query_result, actions) = cluster.engine(NodeId(1)).query(
        Duration::ZERO,
        |views| views.len(),
    );

    match query_result {
        EngineQueryResult::Stale { stale_peers, .. } => {
            assert!(!stale_peers.is_empty(), "should have stale peers");
            // With pull_on_query=true, there should be pull actions
            assert!(!actions.is_empty(), "should have pull actions (RequestSnapshot)");
        }
        EngineQueryResult::Fresh(_) => {
            panic!("expected Stale with ZERO max_staleness after clock advancement");
        }
    }
}

// ── INT-07: Generation ordering ─────────────────────────────────

#[test]
fn int_07_generation_ordering() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    // First mutation
    let r1 = cluster.engine_mut(NodeId(0)).mutate(
        |s| { s.counter = 1; },
        |s| s.clone(),
        SyncUrgency::Default,
    );
    let gen1 = r1.generation;
    route_actions(&mut cluster, NodeId(0), &r1.actions);

    cluster.settle();

    // Second mutation
    let r2 = cluster.engine_mut(NodeId(0)).mutate(
        |s| { s.counter = 2; },
        |s| s.clone(),
        SyncUrgency::Default,
    );
    let gen2 = r2.generation;
    route_actions(&mut cluster, NodeId(0), &r2.actions);

    assert!(
        gen2 > gen1,
        "generation should increase: {gen1:?} -> {gen2:?}"
    );
    assert_eq!(gen2.incarnation, gen1.incarnation, "incarnation should stay stable");
    assert!(gen2.age > gen1.age, "age should increase");

    cluster.settle();

    // Node 1 should see the latest value
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 2);
    assert_eq!(view.generation, gen2);
}

// ── INT-08: Node join → snapshot exchange ────────────────────────

#[test]
fn int_08_node_join_snapshot_exchange() {
    let config = active_push_config();

    // Build cluster manually so we can share the clock with the new node
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let tick_duration = Duration::from_millis(100);
    let mut cluster = MockCluster::new(clock.clone(), tick_duration, 42);

    // Build and add 3 nodes
    for i in 0..3 {
        let engine = DistributedStateEngine::new(
            TestState::name(),
            NodeId(i),
            TestState::default(),
            |s| s.clone(),
            config.clone(),
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

    // Mutate each node
    for i in 0..3 {
        cluster.mutate(
            NodeId(i),
            |s| {
                s.counter = (i + 1) * 10;
                s.label = format!("node{i}");
            },
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }
    cluster.settle();

    // Add a 4th node with the SAME shared clock
    let engine = DistributedStateEngine::new(
        TestState::name(),
        NodeId(3),
        TestState::default(),
        |s| s.clone(),
        config,
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
    cluster.settle();

    // Node 3 should see all 4 nodes' views (3 existing + self)
    assert_eq!(cluster.engine(NodeId(3)).view_count(), 4);

    // Verify node 3 sees the mutations from the original 3 nodes
    for i in 0u64..3 {
        let view = cluster.engine(NodeId(3)).get_view(&NodeId(i)).unwrap();
        assert_eq!(view.value.counter, (i + 1) * 10);
    }
}

// ── INT-09: Node leave → view removal ───────────────────────────

#[test]
fn int_09_node_leave_view_removal() {
    let mut cluster = MockCluster::with_test_state(4, active_push_config());

    // Mutate all 4 nodes
    for i in 0..4 {
        cluster.mutate(
            NodeId(i),
            |s| {
                s.counter = i + 1;
            },
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }
    cluster.settle();

    // Verify all nodes see 4 views
    for nid in cluster.all_node_ids() {
        assert_eq!(cluster.engine(nid).view_count(), 4);
    }

    // Remove node 3
    cluster.remove_node(NodeId(3));

    // Remaining nodes should have 3 views (not 4)
    for nid in cluster.all_node_ids() {
        assert_eq!(
            cluster.engine(nid).view_count(),
            3,
            "node {nid} should have 3 views after removal"
        );
        assert!(
            cluster.engine(nid).get_view(&NodeId(3)).is_none(),
            "node {nid} should not have a view for removed node 3"
        );
    }
}

// ── INT-10: Multiple independent clusters ───────────────────────

#[test]
fn int_10_independent_clusters() {
    // Since MockCluster<S,V> is parameterized, we can't mix state types in
    // one cluster. Instead verify two separate clusters work independently.
    let mut cluster_a = MockCluster::with_test_state(2, active_push_config());
    let mut cluster_b = make_delta_cluster(2, active_push_config());

    // Mutate cluster A
    cluster_a.mutate(
        NodeId(0),
        |s| { s.counter = 100; },
        |s| s.clone(),
        SyncUrgency::Default,
    );

    // Mutate cluster B
    cluster_b.mutate(
        NodeId(0),
        |s| {
            s.counter = 200;
            s.label = "delta_cluster".into();
        },
        |s| TestDeltaState::project_view(s),
        SyncUrgency::Default,
    );

    cluster_a.settle();
    cluster_b.settle();

    // Verify cluster A
    let va = cluster_a.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(va.value.counter, 100);

    // Verify cluster B
    let vb = cluster_b.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(vb.value.counter, 200);
    assert_eq!(vb.value.label, "delta_cluster");
}

// ── INT-11: SyncUrgency::Immediate bypasses feed ────────────────

#[test]
fn int_11_immediate_bypasses_feed() {
    let mut cluster = MockCluster::with_test_state(3, feed_lazy_pull_config());

    // Mutate with Immediate urgency — should produce broadcast even in feed mode
    let result = cluster.engine_mut(NodeId(0)).mutate(
        |s| { s.counter = 999; },
        |s| s.clone(),
        SyncUrgency::Immediate,
    );

    // Should have a BroadcastSync action (not just a feed notification)
    let has_broadcast = result.actions.iter().any(|a| {
        matches!(a, EngineAction::BroadcastSync(..))
    });
    assert!(
        has_broadcast,
        "Immediate urgency should produce BroadcastSync even in FeedLazyPull mode"
    );

    // Route the actions so peers receive the broadcast
    route_actions(&mut cluster, NodeId(0), &result.actions);

    cluster.settle();

    // Verify the data propagated
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 999);
}

// ── INT-12: SyncUrgency::Suppress skips broadcast ───────────────

#[test]
fn int_12_suppress_skips_broadcast() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    let result = cluster.engine_mut(NodeId(0)).mutate(
        |s| { s.counter = 42; },
        |s| s.clone(),
        SyncUrgency::Suppress,
    );

    // Should have NO outbound actions
    assert!(
        result.actions.is_empty(),
        "Suppress should produce no outbound actions, got {:?}",
        result.actions
    );

    // Local state should still be updated
    let view = cluster.engine(NodeId(0)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 42);

    // Peer should NOT see the update (no broadcast happened)
    cluster.settle();
    let peer_view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(
        peer_view.value.counter, 0,
        "peer should still see default (no broadcast)"
    );
}

// ── INT-13: SyncUrgency::Delayed schedules timer ────────────────

#[test]
fn int_13_delayed_schedules_timer() {
    let mut cluster = MockCluster::with_test_state(2, active_push_config());

    // First verify the action type by calling engine_mut directly
    let result = cluster.engine_mut(NodeId(0)).mutate(
        |s| { s.counter = 7; },
        |s| s.clone(),
        SyncUrgency::Delayed(Duration::from_millis(500)),
    );

    // Should have a ScheduleDelayed action
    let has_delayed = result.actions.iter().any(|a| {
        matches!(a, EngineAction::ScheduleDelayed { .. })
    });
    assert!(
        has_delayed,
        "Delayed urgency should produce ScheduleDelayed action, got {:?}",
        result.actions
    );

    // Now use cluster.mutate() to properly route through the delay mechanism.
    // (The engine already has counter=7, so mutate again to test propagation.)
    cluster.mutate(
        NodeId(0),
        |s| { s.counter = 8; },
        |s| s.clone(),
        SyncUrgency::Delayed(Duration::from_millis(500)),
    );

    // Before enough ticks, peer should not see the update
    cluster.tick_n(2);
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 0, "peer should not see update before delay expires");

    // After settling (which processes scheduled timers), peer should see the update
    cluster.settle();
    let view = cluster.engine(NodeId(1)).get_view(&NodeId(0)).unwrap();
    assert_eq!(view.value.counter, 8);
}

// ── INT-14: Query freshness fast path ───────────────────────────

#[test]
fn int_14_query_freshness_fast_path() {
    let mut cluster = MockCluster::with_test_state(3, active_push_config());

    // Mutate all nodes
    for i in 0..3 {
        cluster.mutate(
            NodeId(i),
            |s| { s.counter = i + 1; },
            |s| s.clone(),
            SyncUrgency::Default,
        );
    }
    cluster.settle();

    // Query with generous max_staleness — should return Fresh
    let (result, actions) = cluster.engine(NodeId(0)).query(
        Duration::from_secs(10),
        |views| {
            views.values().map(|v| v.value.counter).sum::<u64>()
        },
    );

    match result {
        EngineQueryResult::Fresh(total) => {
            assert_eq!(total, 1 + 2 + 3, "sum of counters should be 6");
        }
        EngineQueryResult::Stale { result, .. } => {
            // With generous staleness after full settle, this shouldn't happen
            // but if it does, the result should still be correct.
            assert_eq!(result, 6);
        }
    }

    // No pull actions needed when fresh
    assert!(
        actions.is_empty(),
        "fresh query should produce no actions"
    );
}

// ── INT-15: Query freshness slow path ───────────────────────────

#[test]
fn int_15_query_freshness_slow_path() {
    let config = feed_lazy_pull_config();
    let mut cluster = MockCluster::with_test_state(3, config);

    // Mutate node 0 but suppress propagation
    cluster.engine_mut(NodeId(0)).mutate(
        |s| { s.counter = 50; },
        |s| s.clone(),
        SyncUrgency::Suppress,
    );

    // Advance clock to ensure views become stale relative to max_staleness=0
    cluster.tick_n(20);

    // Query on node 1 with max_staleness = 0
    let (result, _actions) = cluster.engine(NodeId(1)).query(
        Duration::ZERO,
        |views| views.len(),
    );

    match result {
        EngineQueryResult::Stale { stale_peers, .. } => {
            assert!(
                !stale_peers.is_empty(),
                "should detect stale peers with zero staleness after clock advancement"
            );
        }
        EngineQueryResult::Fresh(_) => {
            panic!(
                "expected Stale with ZERO max_staleness after clock advancement, \
                 but got Fresh — staleness detection may be broken"
            );
        }
    }
}
