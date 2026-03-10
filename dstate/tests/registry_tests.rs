use dstate::{AnyStateShard, NodeId, RegistryError, StateRegistry};
use std::any::Any;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// ── Test implementations of AnyStateShard ───────────────────────

struct TestShard {
    name: String,
    joined_count: Arc<AtomicU32>,
    left_count: Arc<AtomicU32>,
    stale_count: Arc<AtomicU32>,
}

impl TestShard {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            joined_count: Arc::new(AtomicU32::new(0)),
            left_count: Arc::new(AtomicU32::new(0)),
            stale_count: Arc::new(AtomicU32::new(0)),
        }
    }

    fn with_counters(
        name: &str,
        joined: Arc<AtomicU32>,
        left: Arc<AtomicU32>,
        stale: Arc<AtomicU32>,
    ) -> Self {
        Self {
            name: name.to_string(),
            joined_count: joined,
            left_count: left,
            stale_count: stale,
        }
    }
}

impl AnyStateShard for TestShard {
    fn state_name(&self) -> &str {
        &self.name
    }

    fn on_node_joined(&self, _node_id: NodeId) {
        self.joined_count.fetch_add(1, Ordering::SeqCst);
    }

    fn on_node_left(&self, _node_id: NodeId) {
        self.left_count.fetch_add(1, Ordering::SeqCst);
    }

    fn on_mark_stale(&self, _source: NodeId, _incarnation: u64, _age: u64) {
        self.stale_count.fetch_add(1, Ordering::SeqCst);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// A different concrete type for type-mismatch testing.
#[derive(Debug)]
struct OtherShard {
    name: String,
}

impl AnyStateShard for OtherShard {
    fn state_name(&self) -> &str {
        &self.name
    }

    fn on_node_joined(&self, _: NodeId) {}
    fn on_node_left(&self, _: NodeId) {}
    fn on_mark_stale(&self, _: NodeId, _: u64, _: u64) {}

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── REG-01: Register and lookup ─────────────────────────────────

#[test]
fn reg_01_register_and_lookup() {
    let mut registry = StateRegistry::new();
    let shard = TestShard::new("node_resource");

    registry
        .register(shard)
        .expect("registration should succeed");

    let found = registry.lookup("node_resource").expect("should find");
    assert_eq!(found.state_name(), "node_resource");
}

// ── REG-02: Lookup returns StateNotRegistered ───────────────────

#[test]
fn reg_02_lookup_not_found() {
    let registry = StateRegistry::new();
    match registry.lookup("nonexistent") {
        Err(RegistryError::StateNotRegistered(_)) => {}
        Err(e) => panic!("expected StateNotRegistered, got: {e:?}"),
        Ok(_) => panic!("expected error, got Ok"),
    }
}

// ── REG-03: Lookup with wrong type returns TypeMismatch ─────────

#[test]
fn reg_03_lookup_typed_mismatch() {
    let mut registry = StateRegistry::new();
    let shard = TestShard::new("my_state");

    registry
        .register(shard)
        .expect("should register");

    let result = registry.lookup_typed::<OtherShard>("my_state");
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RegistryError::TypeMismatch { .. }
    ));
}

// ── REG-04: Typed lookup succeeds with correct type ─────────────

#[test]
fn reg_04_lookup_typed_correct() {
    let mut registry = StateRegistry::new();
    let shard = TestShard::new("my_state");

    registry
        .register(shard)
        .expect("should register");

    let found = registry
        .lookup_typed::<TestShard>("my_state")
        .expect("should find with correct type");
    assert_eq!(found.state_name(), "my_state");
}

// ── REG-04a: Duplicate name returns DuplicateName ───────────────

#[test]
fn reg_04a_duplicate_name() {
    let mut registry = StateRegistry::new();
    registry
        .register(TestShard::new("dup"))
        .expect("first should succeed");

    let err = registry
        .register(TestShard::new("dup"))
        .unwrap_err();
    assert!(matches!(err, RegistryError::DuplicateName(_)));
}

// ── REG-05: Broadcast NodeJoined reaches all shards ─────────────

#[test]
fn reg_05_broadcast_node_joined() {
    let mut registry = StateRegistry::new();

    let j1 = Arc::new(AtomicU32::new(0));
    let j2 = Arc::new(AtomicU32::new(0));
    let j3 = Arc::new(AtomicU32::new(0));

    registry
        .register(TestShard::with_counters(
            "s1",
            j1.clone(),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU32::new(0)),
        ))
        .unwrap();
    registry
        .register(TestShard::with_counters(
            "s2",
            j2.clone(),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU32::new(0)),
        ))
        .unwrap();
    registry
        .register(TestShard::with_counters(
            "s3",
            j3.clone(),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU32::new(0)),
        ))
        .unwrap();

    registry.broadcast_node_joined(NodeId(42));

    assert_eq!(j1.load(Ordering::SeqCst), 1);
    assert_eq!(j2.load(Ordering::SeqCst), 1);
    assert_eq!(j3.load(Ordering::SeqCst), 1);
}

// ── REG-06: Broadcast NodeLeft reaches all shards ───────────────

#[test]
fn reg_06_broadcast_node_left() {
    let mut registry = StateRegistry::new();

    let l1 = Arc::new(AtomicU32::new(0));
    let l2 = Arc::new(AtomicU32::new(0));

    registry
        .register(TestShard::with_counters(
            "s1",
            Arc::new(AtomicU32::new(0)),
            l1.clone(),
            Arc::new(AtomicU32::new(0)),
        ))
        .unwrap();
    registry
        .register(TestShard::with_counters(
            "s2",
            Arc::new(AtomicU32::new(0)),
            l2.clone(),
            Arc::new(AtomicU32::new(0)),
        ))
        .unwrap();

    registry.broadcast_node_left(NodeId(7));

    assert_eq!(l1.load(Ordering::SeqCst), 1);
    assert_eq!(l2.load(Ordering::SeqCst), 1);
}

// ── REG-07: len, is_empty, state_names ──────────────────────────

#[test]
fn reg_07_len_and_state_names() {
    let mut registry = StateRegistry::new();
    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);

    registry
        .register(TestShard::new("alpha"))
        .unwrap();
    registry
        .register(TestShard::new("beta"))
        .unwrap();

    assert_eq!(registry.len(), 2);
    assert!(!registry.is_empty());

    let mut names: Vec<&str> = registry.state_names().collect();
    names.sort();
    assert_eq!(names, vec!["alpha", "beta"]);
}

// ── Additional: Default trait ───────────────────────────────────

#[test]
fn registry_default() {
    let registry = StateRegistry::default();
    assert!(registry.is_empty());
}
