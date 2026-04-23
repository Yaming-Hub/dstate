//! Integration tests for StateActor using dactor-mock.

use std::collections::HashMap;
use std::sync::Arc;use std::time::Duration;

use dactor::actor::ActorRef;
use dactor::test_support::test_runtime::TestActorRef;
use dactor::NodeId;
use dactor_mock::MockCluster;

use dstate::engine::DistributedStateEngine;
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_state::TestState;
use dstate::{DeserializeError, DistributedState, StateConfig, SyncStrategy, SyncUrgency};

use dstate_dactor::*;

// ── Helpers ─────────────────────────────────────────────────────

fn make_engine(
    node_id: &str,
    clock: Arc<TestClock>,
    config: StateConfig,
) -> DistributedStateEngine<TestState, TestState> {
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

/// Spawn StateActors on a dactor-mock cluster and wire them together.
async fn setup_cluster(
    node_ids: &[&str],
    config: StateConfig,
) -> (
    MockCluster,
    HashMap<String, TestActorRef<StateActor<TestState, TestState>>>,
    PeerRegistry,
    Arc<TestClock>,
) {
    let clock = Arc::new(TestClock::with_base_unix_ms(1_000_000));
    let cluster = MockCluster::new(node_ids);
    let peers = new_peer_registry();

    let mut actors: HashMap<String, TestActorRef<StateActor<TestState, TestState>>> =
        HashMap::new();

    for &id in node_ids {
        let engine = make_engine(id, clock.clone(), config.clone());
        let args = StateActorArgs {
            engine,
            peers: peers.clone(),
        };

        let actor_ref = cluster
            .node(id)
            .runtime
            .spawn::<StateActor<TestState, TestState>>(&format!("dstate-{id}"), args)
            .await
            .expect("spawn should succeed");

        // Register as peer sender
        {
            let sender = Arc::new(ActorRefPeerSender::new(actor_ref.clone()));
            let mut p = peers.lock().unwrap();
            p.insert(NodeId(id.to_string()), sender);
        }

        actors.insert(id.to_string(), actor_ref);
    }

    // Announce all nodes to each other
    for &id in node_ids {
        let actor = &actors[id];
        for &peer_id in node_ids {
            if peer_id != id {
                let _ = actor.tell(ClusterChange::<TestState>::NodeJoined {
                    node_id: NodeId(peer_id.to_string()),
                    default_view: TestState::default(),
                });
            }
        }
    }

    // Let join messages propagate
    tokio::time::sleep(Duration::from_millis(50)).await;

    (cluster, actors, peers, clock)
}

/// Helper: mutate via the actor
async fn mutate_actor(
    actor: &TestActorRef<StateActor<TestState, TestState>>,
    value: u64,
) {
    let _ = actor.tell(Mutate {
        mutate_fn: Box::new(move |s: &mut TestState| s.counter = value),
        project_fn: Box::new(|s: &TestState| s.clone()),
        urgency: SyncUrgency::Default,
    });
}

/// Helper: query views via the actor
async fn query_actor(
    actor: &TestActorRef<StateActor<TestState, TestState>>,
) -> HashMap<NodeId, dstate::StateViewObject<TestState>> {
    let reply = actor
        .ask(QueryViews::<TestState>::new(Duration::from_secs(60)), None)
        .expect("ask should succeed")
        .await
        .expect("should get reply");
    reply.views
}

// ── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn state_actor_mutate_and_sync() {
    let config = StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    };
    let (_cluster, actors, _peers, _clock) = setup_cluster(&["a", "b", "c"], config).await;

    // Mutate on node-a
    mutate_actor(&actors["a"], 42).await;

    // Let sync messages propagate
    tokio::time::sleep(Duration::from_millis(50)).await;

    // All nodes should see node-a's value
    let views_b = query_actor(&actors["b"]).await;
    let a_id = NodeId("a".to_string());
    assert_eq!(
        views_b[&a_id].value.counter, 42,
        "node-b should see node-a's mutation"
    );

    let views_c = query_actor(&actors["c"]).await;
    assert_eq!(
        views_c[&a_id].value.counter, 42,
        "node-c should see node-a's mutation"
    );
}

#[tokio::test]
async fn state_actor_multi_node_mutations() {
    let config = StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    };
    let (_cluster, actors, _peers, _clock) = setup_cluster(&["a", "b"], config).await;

    mutate_actor(&actors["a"], 10).await;
    mutate_actor(&actors["b"], 20).await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let views_a = query_actor(&actors["a"]).await;
    let views_b = query_actor(&actors["b"]).await;

    let a_id = NodeId("a".to_string());
    let b_id = NodeId("b".to_string());

    assert_eq!(views_a[&a_id].value.counter, 10);
    assert_eq!(views_a[&b_id].value.counter, 20);
    assert_eq!(views_b[&a_id].value.counter, 10);
    assert_eq!(views_b[&b_id].value.counter, 20);
}

#[tokio::test]
async fn state_actor_node_departure() {
    let config = StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    };
    let (_cluster, actors, _peers, _clock) = setup_cluster(&["a", "b", "c"], config).await;

    mutate_actor(&actors["a"], 1).await;
    mutate_actor(&actors["b"], 2).await;
    mutate_actor(&actors["c"], 3).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Node-c departs
    let c_id = NodeId("c".to_string());
    let _ = actors["a"].tell(ClusterChange::<TestState>::NodeLeft {
        node_id: c_id.clone(),
    });
    let _ = actors["b"].tell(ClusterChange::<TestState>::NodeLeft {
        node_id: c_id.clone(),
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let views_a = query_actor(&actors["a"]).await;
    assert!(
        !views_a.contains_key(&c_id),
        "departed node should be removed from view map"
    );
}

#[tokio::test]
async fn state_actor_query_returns_all_views() {
    let config = StateConfig {
        sync_strategy: SyncStrategy::active_push(),
        ..StateConfig::default()
    };
    let (_cluster, actors, _peers, _clock) = setup_cluster(&["x", "y"], config).await;

    // Even without mutations, query should return default views for all nodes
    let views = query_actor(&actors["x"]).await;
    let x_id = NodeId("x".to_string());
    let y_id = NodeId("y".to_string());

    assert!(views.contains_key(&x_id), "should see own view");
    assert!(views.contains_key(&y_id), "should see peer view");
}
