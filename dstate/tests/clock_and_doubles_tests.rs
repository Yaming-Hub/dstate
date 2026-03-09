/// TEST-01..TEST-04: Clock abstraction tests
/// TEST-08..TEST-10: Test doubles tests
use std::time::Duration;

use dstate::{Clock, StateObject, StatePersistence};
use dstate::test_support::test_clock::TestClock;
use dstate::test_support::test_persist::{FailingPersistence, InMemoryPersistence};

// ---------------------------------------------------------------------------
// TEST-01..TEST-04: Clock
// ---------------------------------------------------------------------------

#[test]
fn test_01_test_clock_starts_frozen() {
    let clock = TestClock::with_base_unix_ms(1000);
    let initial_ms = clock.unix_ms();
    assert_eq!(initial_ms, 1000);
    // now() returns an Instant — just verify it doesn't move
    let i1 = clock.now();
    let i2 = clock.now();
    assert!(i2.duration_since(i1).as_millis() < 1);
}

#[test]
fn test_02_test_clock_advances_on_demand() {
    let clock = TestClock::with_base_unix_ms(1000);
    let before = clock.now();
    clock.advance(Duration::from_secs(5));
    let after = clock.now();
    let elapsed = after.duration_since(before);
    assert!(elapsed >= Duration::from_secs(5));
    assert_eq!(clock.unix_ms(), 1000 + 5000);
}

#[test]
fn test_03_injected_clock_controls_time() {
    let clock = TestClock::with_base_unix_ms(50000);
    assert_eq!(clock.unix_ms(), 50000);
    clock.advance(Duration::from_millis(100));
    assert_eq!(clock.unix_ms(), 50100);
}

#[test]
fn test_04_freshness_check_uses_clock() {
    let clock = TestClock::new();
    let synced_at = clock.now();
    clock.advance(Duration::from_secs(10));
    let current = clock.now();
    let elapsed = current.duration_since(synced_at);
    assert!(elapsed >= Duration::from_secs(10));
}

// ---------------------------------------------------------------------------
// TEST-08..TEST-10: Test doubles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_08_in_memory_persistence_round_trip() {
    let persist = InMemoryPersistence::<String>::new();
    let state = StateObject {
        age: 5,
        incarnation: 1,
        storage_version: 1,
        value: "hello world".to_string(),
        created_time: 1000,
        modified_time: 2000,
    };
    persist.save(&state, None).await.unwrap();
    let loaded = persist.load().await.unwrap().unwrap();
    assert_eq!(loaded.value, "hello world");
    assert_eq!(loaded.age, 5);
    assert_eq!(loaded.incarnation, 1);
    assert_eq!(loaded.storage_version, 1);
    assert_eq!(loaded.created_time, 1000);
    assert_eq!(loaded.modified_time, 2000);
}

#[tokio::test]
async fn test_09_failing_persistence_always_returns_error() {
    let persist = FailingPersistence;
    let state = StateObject {
        age: 0,
        incarnation: 1,
        storage_version: 1,
        value: "test".to_string(),
        created_time: 0,
        modified_time: 0,
    };
    let save_result = StatePersistence::<String>::save(&persist, &state, None).await;
    assert!(save_result.is_err());
    let load_result = StatePersistence::<String>::load(&persist).await;
    assert!(load_result.is_err());
}

#[tokio::test]
async fn test_10_in_memory_persistence_load_returns_none_initially() {
    let persist = InMemoryPersistence::<String>::new();
    let loaded = persist.load().await.unwrap();
    assert!(loaded.is_none());
}