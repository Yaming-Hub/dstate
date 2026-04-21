//! Functional tests using dactor-mock to exercise dstate through the actor shell.
//!
//! These tests complement the deterministic MockCluster tests by proving
//! that dstate works correctly within a dactor actor system with async
//! message passing, network partitions, and node crash/restart.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dactor::actor::ActorRef;
use dactor::test_support::test_runtime::TestActorRef;
use dactor::NodeId;
use dactor_mock::MockCluster;

use dstate::engine::DistributedStateEngine;
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::TestState;
use dstate::{DeserializeError, DistributedState, StateConfig, SyncUrgency};

use dstate_integration::shell::*;

// ── Test helpers ────────────────────────────────────────────────

/// Create a TestState engine for a given node.
fn make_engine(node_id: &str, clock: Arc<TestClock>) -> DistributedStateEngine<TestState, TestState> {
    // Enable periodic sync for partition-heal tests
    let config = StateConfig {
        sync_strategy: dstate::SyncStrategy::feed_with_periodic_sync(Duration::from_secs(1)),
        ..StateConfig::default()
    };
    DistributedStateEngine::new(
        TestState::name(),
        NodeId(node_id.to_string()),
        TestState::default(),
        |s| s.clone(),
        config,
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

/// Spawn DstateActor instances on a dactor-mock cluster and wire them together.
async fn setup_cluster(
    node_ids: &[&str],
) -> (
    MockCluster,
    HashMap<String, TestActorRef<DstateActor<TestState, TestState>>>,
    PeerRegistry<TestState, TestState>,
    Arc<TestClock>,
    Arc<dactor_mock::MockNetwork>,
) {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let cluster = MockCluster::new(node_ids);
    // Create a separate MockNetwork for dstate-level message routing.
    // This is independent from dactor-mock's cluster-level network.
    let network = Arc::new(dactor_mock::MockNetwork::new());
    let peers: PeerRegistry<TestState, TestState> = Arc::new(Mutex::new(HashMap::new()));

    let mut actors = HashMap::new();

    // Spawn an actor on each node
    for &id in node_ids {
        let engine = make_engine(id, clock.clone());
        let args = DstateActorArgs {
            engine,
            peers: peers.clone(),
            network: network.clone(),
            own_node_id: NodeId(id.to_string()),
        };

        let actor_ref = cluster
            .node(id)
            .runtime
            .spawn::<DstateActor<TestState, TestState>>(&format!("dstate-{}", id), args)
            .await
            .expect("spawn should succeed");

        // Register in peer registry
        {
            let mut p = peers.lock().unwrap();
            p.insert(NodeId(id.to_string()), actor_ref.clone());
        }

        actors.insert(id.to_string(), actor_ref);
    }

    // Announce all nodes to each other
    for &id in node_ids {
        let actor = &actors[id];
        for &peer_id in node_ids {
            if peer_id != id {
                actor
                    .tell(ClusterChange::<TestState>::NodeJoined {
                        node_id: NodeId(peer_id.to_string()),
                        default_view: TestState::default(),
                    })
                    .expect("tell should succeed");
            }
        }
    }

    // Give actors time to process cluster join messages and exchange snapshots
    tokio::time::sleep(Duration::from_millis(100)).await;

    (cluster, actors, peers, clock, network)
}

/// Helper: mutate state on a node via the actor shell.
fn tell_mutate(
    actor: &TestActorRef<DstateActor<TestState, TestState>>,
    counter: u64,
    label: &str,
) {
    let label = label.to_string();
    actor
        .tell(Mutate {
            mutate_fn: Box::new(move |s: &mut TestState| {
                s.counter = counter;
                s.label = label;
            }),
            project_fn: Box::new(|s: &TestState| s.clone()),
            urgency: SyncUrgency::Immediate,
        })
        .expect("tell mutate should succeed");
}

/// Helper: query the view map on a node via the actor shell.
async fn ask_query(
    actor: &TestActorRef<DstateActor<TestState, TestState>>,
) -> HashMap<NodeId, dstate::StateViewObject<TestState>> {
    let reply = actor
        .ask(QueryAll::<TestState>::new(Duration::from_secs(60)), None)
        .expect("ask should succeed")
        .await
        .expect("query should succeed");
    reply.views
}

// ── Tests ───────────────────────────────────────────────────────

/// MOCK-01: Basic replication across 3 nodes.
///
/// Mutate on node-1, verify node-2 and node-3 receive the update.
#[tokio::test]
async fn mock_01_basic_replication() {
    let (_cluster, actors, _peers, _clock, _network) = setup_cluster(&["n1", "n2", "n3"]).await;

    // Mutate on n1
    tell_mutate(&actors["n1"], 42, "hello");

    // Wait for async message delivery
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Query all nodes
    let views_n2 = ask_query(&actors["n2"]).await;
    let views_n3 = ask_query(&actors["n3"]).await;

    // n2 and n3 should have n1's view
    let n1_id = NodeId("n1".to_string());
    assert!(views_n2.contains_key(&n1_id), "n2 should have n1's view");
    assert_eq!(views_n2[&n1_id].value.counter, 42);
    assert_eq!(views_n2[&n1_id].value.label, "hello");

    assert!(views_n3.contains_key(&n1_id), "n3 should have n1's view");
    assert_eq!(views_n3[&n1_id].value.counter, 42);
    assert_eq!(views_n3[&n1_id].value.label, "hello");
}

/// MOCK-02: Network partition blocks replication.
///
/// Partition n1 from n2, verify n2 doesn't get n1's update but n3 does.
#[tokio::test]
async fn mock_02_network_partition() {
    let (_cluster, actors, _peers, _clock, network) = setup_cluster(&["n1", "n2", "n3"]).await;

    // Partition n1 <-> n2
    let n1_id = NodeId("n1".to_string());
    let n2_id = NodeId("n2".to_string());
    network.partition(&n1_id, &n2_id);

    // Mutate on n1
    tell_mutate(&actors["n1"], 99, "partitioned");

    // Wait for delivery
    tokio::time::sleep(Duration::from_millis(200)).await;

    // n3 should have the update (not partitioned)
    let views_n3 = ask_query(&actors["n3"]).await;
    assert_eq!(views_n3[&n1_id].value.counter, 99);

    // n2 should NOT have the update (partitioned)
    let views_n2 = ask_query(&actors["n2"]).await;
    // n2 might have n1's initial default view (counter=0) from the join phase
    let n1_counter_on_n2 = views_n2
        .get(&n1_id)
        .map(|v| v.value.counter)
        .unwrap_or(0);
    assert_ne!(
        n1_counter_on_n2, 99,
        "n2 should not have received n1's update through partition"
    );

    // Heal partition
    network.remove_partition(&n1_id, &n2_id);

    // Trigger periodic sync on n1 to push state to n2
    actors["n1"]
        .tell(PeriodicSync)
        .expect("tell periodic sync should succeed");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now n2 should have the update
    let views_n2 = ask_query(&actors["n2"]).await;
    assert_eq!(
        views_n2[&n1_id].value.counter, 99,
        "n2 should have n1's update after partition heal"
    );
}

/// MOCK-03: Node crash and restart.
///
/// Node n2 crashes (actor stops), a new n2 is spawned and rejoins.
#[tokio::test]
async fn mock_03_crash_and_restart() {
    let (mut cluster, mut actors, peers, clock, network) = setup_cluster(&["n1", "n2", "n3"]).await;

    // Mutate on n1 before crash
    tell_mutate(&actors["n1"], 10, "before-crash");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify n2 has the state
    let views = ask_query(&actors["n2"]).await;
    let n1_id = NodeId("n1".to_string());
    assert_eq!(views[&n1_id].value.counter, 10);

    // Crash n2: stop the actor and notify peers
    let n2_id = NodeId("n2".to_string());
    actors["n2"].stop();

    // Notify surviving nodes that n2 left
    actors["n1"]
        .tell(ClusterChange::<TestState>::NodeLeft {
            node_id: n2_id.clone(),
        })
        .expect("tell should succeed");
    actors["n3"]
        .tell(ClusterChange::<TestState>::NodeLeft {
            node_id: n2_id.clone(),
        })
        .expect("tell should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Mutate on n1 while n2 is down
    tell_mutate(&actors["n1"], 20, "during-crash");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Restart n2 with fresh engine
    cluster.restart_node("n2");

    let engine = make_engine("n2", clock.clone());
    let args = DstateActorArgs {
        engine,
        peers: peers.clone(),
        network: network.clone(),
        own_node_id: n2_id.clone(),
    };

    let new_n2 = cluster
        .node("n2")
        .runtime
        .spawn::<DstateActor<TestState, TestState>>("dstate-n2", args)
        .await
        .expect("spawn should succeed");

    // Update peer registry
    {
        let mut p = peers.lock().unwrap();
        p.insert(n2_id.clone(), new_n2.clone());
    }

    actors.insert("n2".to_string(), new_n2.clone());

    // Announce n2 rejoining to all nodes
    for id in &["n1", "n3"] {
        actors[*id]
            .tell(ClusterChange::<TestState>::NodeJoined {
                node_id: n2_id.clone(),
                default_view: TestState::default(),
            })
            .expect("tell should succeed");
    }

    // Announce existing nodes to new n2
    for id in &["n1", "n3"] {
        new_n2
            .tell(ClusterChange::<TestState>::NodeJoined {
                node_id: NodeId(id.to_string()),
                default_view: TestState::default(),
            })
            .expect("tell should succeed");
    }

    // Wait for snapshot exchange
    tokio::time::sleep(Duration::from_millis(300)).await;

    // New n2 should have n1's latest state
    let views = ask_query(&actors["n2"]).await;
    assert_eq!(
        views[&n1_id].value.counter, 20,
        "restarted n2 should have n1's latest state"
    );
    assert_eq!(views[&n1_id].value.label, "during-crash");
}

/// MOCK-04: Periodic sync after partition heal.
///
/// Partition n1 from all, mutate on n1, heal, periodic sync propagates.
#[tokio::test]
async fn mock_04_periodic_sync_after_heal() {
    let (_cluster, actors, _peers, _clock, network) = setup_cluster(&["n1", "n2"]).await;

    let n1_id = NodeId("n1".to_string());
    let n2_id = NodeId("n2".to_string());

    // Partition n1 <-> n2
    network.partition(&n1_id, &n2_id);

    // Mutate on both sides while partitioned
    tell_mutate(&actors["n1"], 100, "n1-side");
    tell_mutate(&actors["n2"], 200, "n2-side");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify neither has the other's update
    let views_n1 = ask_query(&actors["n1"]).await;
    let views_n2 = ask_query(&actors["n2"]).await;

    let n2_counter_on_n1 = views_n1.get(&n2_id).map(|v| v.value.counter).unwrap_or(0);
    assert_ne!(n2_counter_on_n1, 200, "n1 should not have n2's update");

    let n1_counter_on_n2 = views_n2.get(&n1_id).map(|v| v.value.counter).unwrap_or(0);
    assert_ne!(n1_counter_on_n2, 100, "n2 should not have n1's update");

    // Heal partition
    network.remove_partition(&n1_id, &n2_id);

    // Trigger periodic sync on both nodes
    actors["n1"].tell(PeriodicSync).unwrap();
    actors["n2"].tell(PeriodicSync).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Both should now have each other's state
    let views_n1 = ask_query(&actors["n1"]).await;
    let views_n2 = ask_query(&actors["n2"]).await;

    assert_eq!(
        views_n1[&n2_id].value.counter, 200,
        "n1 should have n2's state after heal"
    );
    assert_eq!(
        views_n2[&n1_id].value.counter, 100,
        "n2 should have n1's state after heal"
    );
}
