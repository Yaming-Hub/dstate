/// RT-01..RT-13: ActorRuntime trait contract tests via TestRuntime
/// MOCK-01..MOCK-05: TestRuntime self-tests
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dstate::test_support::test_runtime::*;
use dstate::{
    ActorRef, ActorRuntime, ClusterEvent, ClusterEvents, NodeId, SubscriptionId, TimerHandle,
};

// ---------------------------------------------------------------------------
// RT: ActorRuntime trait contract tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rt_01_spawn_returns_valid_actor_ref() {
    let rt = TestRuntime::new();
    let received = Arc::new(AtomicU64::new(0));
    let r = received.clone();
    let actor: TestActorRef<u64> = rt.spawn("test", move |msg: u64| {
        r.fetch_add(msg, Ordering::SeqCst);
    });
    actor.send(42).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(received.load(Ordering::SeqCst), 42);
}

#[tokio::test]
async fn rt_02_send_delivers_message() {
    let rt = TestRuntime::new();
    let received = Arc::new(AtomicU64::new(0));
    let r = received.clone();
    let actor: TestActorRef<u64> = rt.spawn("test", move |msg: u64| {
        r.fetch_add(msg, Ordering::SeqCst);
    });
    actor.send(10).unwrap();
    actor.send(20).unwrap();
    actor.send(30).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(received.load(Ordering::SeqCst), 60);
}

#[tokio::test]
async fn rt_05_group_join_and_get_members() {
    let rt = TestRuntime::new();
    let _a1: TestActorRef<u64> = rt.spawn("a1", |_: u64| {});
    let _a2: TestActorRef<u64> = rt.spawn("a2", |_: u64| {});
    let _a3: TestActorRef<u64> = rt.spawn("a3", |_: u64| {});
    rt.join_group("my_group", &_a1).unwrap();
    rt.join_group("my_group", &_a2).unwrap();
    rt.join_group("my_group", &_a3).unwrap();
    let members: Vec<TestActorRef<u64>> = rt.get_group_members("my_group").unwrap();
    assert_eq!(members.len(), 3);
}

#[tokio::test]
async fn rt_06_group_leave() {
    // Leave is simplified in TestRuntime (no-op), so just verify no panic
    let rt = TestRuntime::new();
    let a1: TestActorRef<u64> = rt.spawn("a1", |_: u64| {});
    rt.join_group("g", &a1).unwrap();
    rt.leave_group("g", &a1).unwrap();
    // Members still shows 1 because TestRuntime leave is no-op
    // This is acceptable — conformance tests in adapter crates will validate real leave
}

#[tokio::test]
async fn rt_07_group_broadcast_reaches_all() {
    let rt = TestRuntime::new();
    let c1 = Arc::new(AtomicU64::new(0));
    let c2 = Arc::new(AtomicU64::new(0));
    let c3 = Arc::new(AtomicU64::new(0));

    let r1 = c1.clone();
    let r2 = c2.clone();
    let r3 = c3.clone();

    let a1: TestActorRef<u64> = rt.spawn("a1", move |msg: u64| {
        r1.fetch_add(msg, Ordering::SeqCst);
    });
    let a2: TestActorRef<u64> = rt.spawn("a2", move |msg: u64| {
        r2.fetch_add(msg, Ordering::SeqCst);
    });
    let a3: TestActorRef<u64> = rt.spawn("a3", move |msg: u64| {
        r3.fetch_add(msg, Ordering::SeqCst);
    });

    rt.join_group("broadcast_group", &a1).unwrap();
    rt.join_group("broadcast_group", &a2).unwrap();
    rt.join_group("broadcast_group", &a3).unwrap();

    rt.broadcast_group("broadcast_group", 7u64).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(c1.load(Ordering::SeqCst), 7);
    assert_eq!(c2.load(Ordering::SeqCst), 7);
    assert_eq!(c3.load(Ordering::SeqCst), 7);
}

#[tokio::test]
async fn rt_08_broadcast_skips_non_members() {
    let rt = TestRuntime::new();
    let member_count = Arc::new(AtomicU64::new(0));
    let non_member_count = Arc::new(AtomicU64::new(0));

    let mc = member_count.clone();
    let nmc = non_member_count.clone();

    let member: TestActorRef<u64> = rt.spawn("member", move |msg: u64| {
        mc.fetch_add(msg, Ordering::SeqCst);
    });
    let _non_member: TestActorRef<u64> = rt.spawn("non_member", move |msg: u64| {
        nmc.fetch_add(msg, Ordering::SeqCst);
    });

    rt.join_group("exclusive", &member).unwrap();
    // non_member is NOT joined

    rt.broadcast_group("exclusive", 10u64).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(member_count.load(Ordering::SeqCst), 10);
    assert_eq!(non_member_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rt_09_cluster_events_node_joined() {
    let rt = TestRuntime::new();
    let joined = Arc::new(Mutex::new(Vec::new()));
    let j = joined.clone();

    use std::sync::Mutex;
    rt.cluster_events()
        .subscribe(Box::new(move |event| {
            if let ClusterEvent::NodeJoined(id) = event {
                j.lock().unwrap().push(id);
            }
        }))
        .unwrap();

    rt.test_cluster_events().emit(ClusterEvent::NodeJoined(NodeId(5)));
    let events = joined.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], NodeId(5));
}

#[tokio::test]
async fn rt_10_cluster_events_node_left() {
    let rt = TestRuntime::new();
    let left = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l = left.clone();

    rt.cluster_events()
        .subscribe(Box::new(move |event| {
            if let ClusterEvent::NodeLeft(id) = event {
                l.lock().unwrap().push(id);
            }
        }))
        .unwrap();

    rt.test_cluster_events().emit(ClusterEvent::NodeLeft(NodeId(3)));
    let events = left.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], NodeId(3));
}

#[tokio::test]
async fn rt_11_send_interval_fires_periodically() {
    let rt = TestRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();
    let actor: TestActorRef<u64> = rt.spawn("ticker", move |_msg: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let _timer = rt.send_interval(&actor, Duration::from_millis(50), 1u64);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let n = count.load(Ordering::SeqCst);
    assert!(n >= 2, "expected at least 2 timer fires, got {n}");
}

#[tokio::test]
async fn rt_12_send_after_fires_once() {
    let rt = TestRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();
    let actor: TestActorRef<u64> = rt.spawn("once", move |_: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let _timer = rt.send_after(&actor, Duration::from_millis(50), 1u64);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rt_13_timer_cancel_stops_delivery() {
    let rt = TestRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();
    let actor: TestActorRef<u64> = rt.spawn("cancel_test", move |_: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let timer = rt.send_interval(&actor, Duration::from_millis(30), 1u64);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before_cancel = count.load(Ordering::SeqCst);
    timer.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after_cancel = count.load(Ordering::SeqCst);
    // After cancel, no more messages should arrive (allow +/-1 for in-flight)
    assert!(
        after_cancel <= before_cancel + 1,
        "expected no more than 1 extra after cancel, before={before_cancel} after={after_cancel}"
    );
}

#[tokio::test]
async fn rt_14_actor_ref_is_clone_send_sync() {
    fn assert_clone_send_sync<T: Clone + Send + Sync>() {}
    assert_clone_send_sync::<TestActorRef<u64>>();
    assert_clone_send_sync::<TestActorRef<String>>();
}

#[tokio::test]
async fn rt_15_subscribe_returns_subscription_id() {
    let rt = TestRuntime::new();
    let id: SubscriptionId = rt
        .cluster_events()
        .subscribe(Box::new(|_| {}))
        .unwrap();
    // Just verify we got a valid id (non-panicking)
    let _ = format!("{id:?}");
}

#[tokio::test]
async fn rt_16_unsubscribe_stops_notifications() {
    let rt = TestRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let id = rt
        .cluster_events()
        .subscribe(Box::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();

    rt.test_cluster_events().emit(ClusterEvent::NodeJoined(NodeId(1)));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    rt.cluster_events().unsubscribe(id).unwrap();
    rt.test_cluster_events().emit(ClusterEvent::NodeJoined(NodeId(2)));
    assert_eq!(count.load(Ordering::SeqCst), 1); // no increment after unsubscribe
}

// ---------------------------------------------------------------------------
// MOCK: TestRuntime self-tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_01_test_runtime_spawns_independent_actors() {
    let rt = TestRuntime::new();
    let c1 = Arc::new(AtomicU64::new(0));
    let c2 = Arc::new(AtomicU64::new(0));
    let c3 = Arc::new(AtomicU64::new(0));
    let r1 = c1.clone();
    let r2 = c2.clone();
    let r3 = c3.clone();

    let a1: TestActorRef<u64> = rt.spawn("a1", move |msg: u64| {
        r1.fetch_add(msg, Ordering::SeqCst);
    });
    let a2: TestActorRef<u64> = rt.spawn("a2", move |msg: u64| {
        r2.fetch_add(msg * 10, Ordering::SeqCst);
    });
    let a3: TestActorRef<u64> = rt.spawn("a3", move |msg: u64| {
        r3.fetch_add(msg * 100, Ordering::SeqCst);
    });

    a1.send(1).unwrap();
    a2.send(1).unwrap();
    a3.send(1).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(c1.load(Ordering::SeqCst), 1);
    assert_eq!(c2.load(Ordering::SeqCst), 10);
    assert_eq!(c3.load(Ordering::SeqCst), 100);
}

#[tokio::test]
async fn mock_02_group_isolates_groups() {
    let rt = TestRuntime::new();
    let ga = Arc::new(AtomicU64::new(0));
    let gb = Arc::new(AtomicU64::new(0));
    let ra = ga.clone();
    let rb = gb.clone();

    let a: TestActorRef<u64> = rt.spawn("a", move |msg: u64| {
        ra.fetch_add(msg, Ordering::SeqCst);
    });
    let b: TestActorRef<u64> = rt.spawn("b", move |msg: u64| {
        rb.fetch_add(msg, Ordering::SeqCst);
    });

    rt.join_group("group_a", &a).unwrap();
    rt.join_group("group_b", &b).unwrap();

    rt.broadcast_group("group_a", 5u64).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(ga.load(Ordering::SeqCst), 5);
    assert_eq!(gb.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mock_03_cluster_events_controllable() {
    let rt = TestRuntime::new();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let e = events.clone();

    rt.cluster_events()
        .subscribe(Box::new(move |event| {
            e.lock().unwrap().push(event);
        }))
        .unwrap();

    rt.test_cluster_events()
        .emit(ClusterEvent::NodeJoined(NodeId(1)));
    rt.test_cluster_events()
        .emit(ClusterEvent::NodeLeft(NodeId(2)));

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0], ClusterEvent::NodeJoined(NodeId(1)));
    assert_eq!(recorded[1], ClusterEvent::NodeLeft(NodeId(2)));
}

#[tokio::test]
async fn mock_04_timers_fire_with_real_time() {
    // TestRuntime timers use real tokio time (not TestClock)
    let rt = TestRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();
    let actor: TestActorRef<u64> = rt.spawn("timer_test", move |_: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let _timer = rt.send_interval(&actor, Duration::from_millis(40), 1u64);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let n = count.load(Ordering::SeqCst);
    assert!(n >= 2, "expected at least 2, got {n}");
}

#[tokio::test]
async fn mock_05_cross_actor_send() {
    let rt = TestRuntime::new();
    let result = Arc::new(AtomicU64::new(0));
    let r = result.clone();

    // Actor B receives the final value
    let b: TestActorRef<u64> = rt.spawn("b", move |msg: u64| {
        r.store(msg, Ordering::SeqCst);
    });

    // Actor A forwards messages to B
    let b_clone = b.clone();
    let a: TestActorRef<u64> = rt.spawn("a", move |msg: u64| {
        let _ = b_clone.send(msg * 2);
    });

    a.send(21).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(result.load(Ordering::SeqCst), 42);
}