# Integration Test Plan — dstate

> **⚠️ Historical Document (v0.1):** This integration test plan was written for the
> v0.1 architecture with `MockRuntime` implementing `dstate::ActorRuntime`. As of v1.0,
> `ActorRuntime` has been removed and integration testing uses `dstate-integration`
> with `MockCluster` and `DistributedStateEngine` directly.
> See [distributed-state-design.md §6.0](./distributed-state-design.md#60-actor-runtime--dactor-integration)
> for the current architecture.

## Overview

This document describes the integration test framework and test matrix for
end-to-end testing of the dstate distributed state replication protocol. Tests
live in a dedicated `dstate-integration` crate and exercise the full pipeline:
mutation → sync → propagation → query — across simulated multi-node clusters.

## Goals

1. **Validate the sync protocol end-to-end** across multiple simulated nodes,
   not just individual logic modules in isolation.
2. **Enforce serialization correctness** by routing all inter-node messages
   through byte-level serialization/deserialization.
3. **Test fault tolerance** via composable interceptors that simulate network
   partitions, message drops, corruption, and node crashes.
4. **Deterministic execution** using single-threaded mailboxes and `TestClock`
   so tests are fast, repeatable, and debuggable.

## Architecture

### Components

```
┌──────────────────────────────────────────────────┐
│                  MockCluster                     │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐ │
│  │  MockNode   │  │  MockNode   │  │  MockNode   │ │
│  │  NodeId(0)  │  │  NodeId(1)  │  │  NodeId(2)  │ │
│  │             │  │             │  │             │ │
│  │ ┌─────────┐│  │ ┌─────────┐│  │ ┌─────────┐│ │
│  │ │  Engine  ││  │ │  Engine  ││  │ │  Engine  ││ │
│  │ │(ShardCore││  │ │(ShardCore││  │ │(ShardCore││ │
│  │ │+SyncLogic││  │ │+SyncLogic││  │ │+SyncLogic││ │
│  │ │+ChangeFd)││  │ │+ChangeFd)││  │ │+ChangeFd)││ │
│  │ └─────────┘│  │ └─────────┘│  │ └─────────┘│ │
│  │ ┌─────────┐│  │ ┌─────────┐│  │ ┌─────────┐│ │
│  │ │ Mailbox  ││  │ │ Mailbox  ││  │ │ Mailbox  ││ │
│  │ │VecDeque  ││  │ │VecDeque  ││  │ │VecDeque  ││ │
│  │ └─────────┘│  │ └─────────┘│  │ └─────────┘│ │
│  └──────┬─────┘  └──────┬─────┘  └──────┬─────┘ │
│         │               │               │       │
│  ┌──────┴───────────────┴───────────────┴─────┐ │
│  │              MockTransport                  │ │
│  │  ┌────────────────────────────────────────┐ │ │
│  │  │     NetworkInterceptor Pipeline        │ │ │
│  │  │  [Partition] → [DropRate] → [Delay]    │ │ │
│  │  └────────────────────────────────────────┘ │ │
│  │  serialize(msg) → Vec<u8> → deserialize()   │ │
│  └─────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────┘
```

### MockRuntime

Implements `dstate::ActorRuntime` with:

- **Single-threaded mailbox**: Each actor has a `VecDeque<M>` processed by
  explicit `drain()` / `tick()` calls. No background threads — fully
  deterministic. Although execution is single-threaded, the `ActorRuntime`
  trait requires `Send + Sync` bounds, so internals use `Arc<Mutex<...>>`
  for type-system compliance.
- **Processing groups**: Local type-erased registry keyed by
  `(group_name, TypeId)` (same pattern as ractor/kameo adapters).
- **Timers**: Driven by `TestClock`. `send_interval` and `send_after` are
  evaluated on each `tick()` by comparing scheduled time against the clock.
- **Cluster events**: Callback-based subscription with snapshot pattern.

### MockTransport

Routes messages between `MockNode` instances:

- **Wire envelope**: All inter-node messages are wrapped in a `WireMessage`
  enum before serialization:
  ```rust
  #[derive(Serialize, Deserialize)]
  pub enum WireMessage {
      Sync(SyncMessage),
      Feed(BatchedChangeFeed),
  }
  ```
  This ensures the receiver can safely deserialize without guessing the type.
- **Byte-level boundary**: `WireMessage` values are serialized to `Vec<u8>`
  (via bincode) before crossing node boundaries and deserialized on the
  receiving end. This enforces that all wire types survive serialization
  round-trips (both envelope and state payload).
- **Interceptor pipeline**: Messages pass through a chain of
  `NetworkInterceptor` implementations before delivery. Each interceptor
  can inspect, modify, delay, or drop messages.
- **Delivery queue**: Messages are queued and delivered on the next `tick()`,
  not immediately. This simulates network latency and allows tests to
  inspect queued messages before delivery.

### NetworkInterceptor

```rust
/// Decision returned by a network interceptor.
pub enum InterceptAction {
    /// Deliver the message (possibly modified) immediately.
    Deliver(Vec<u8>),
    /// Drop the message silently.
    Drop,
    /// Hold the message for `n` ticks before delivery.
    Delay { bytes: Vec<u8>, ticks: usize },
    /// Deliver multiple copies (for duplication testing).
    DeliverMany(Vec<Vec<u8>>),
}

pub trait NetworkInterceptor: Send {
    /// Inspect a message in transit. Return an action controlling delivery.
    fn intercept(
        &mut self,
        from: NodeId,
        to: NodeId,
        msg_bytes: &[u8],
        context: &InterceptContext,
    ) -> InterceptAction;
}

/// Context passed to interceptors for deterministic decisions.
pub struct InterceptContext {
    pub current_tick: u64,
    pub message_id: u64,
    pub rng_seed: u64,
}
```

Built-in interceptors:

| Interceptor | Behavior |
|-------------|----------|
| `PassThrough` | Deliver all messages unmodified |
| `DropAll` | Drop all messages (total network failure) |
| `DropRate(f64, seed)` | Drop messages with given probability (deterministic seeded RNG) |
| `Partition(A, B)` | Drop messages between node sets A and B (supports asymmetric) |
| `CorruptBytes(seed)` | Flip random bits in serialized data (deterministic seeded RNG) |
| `DelayTicks(n)` | Hold messages for n `tick()` calls before delivery |
| `DuplicateAll` | Deliver each message twice |
| `Reorder(seed)` | Shuffle delivery order within a tick window (deterministic) |

### MockCluster

Orchestrates the test:

```rust
pub struct MockCluster {
    nodes: Vec<MockNode>,
    transport: MockTransport,
    clock: TestClock,
}

impl MockCluster {
    /// Create a cluster with `n` nodes, all using the given state type
    /// and sync strategy.
    pub fn new(n: usize, config: StateConfig) -> Self;

    /// Process one round: deliver queued messages, run all mailboxes,
    /// advance clock by one tick.
    pub fn tick(&mut self);

    /// Tick until no messages are in flight and all mailboxes are empty.
    pub fn settle(&mut self);

    /// Mutate state on a specific node.
    pub fn mutate(&mut self, node: NodeId, mutate_fn: impl FnOnce(&mut State));

    /// Query state across the cluster from a specific node's perspective.
    pub fn query(&self, node: NodeId) -> QueryResult;

    /// Simulate a node crash (drop state) and restart (re-initialize
    /// from persistence with new incarnation).
    pub fn crash_and_restart(&mut self, node: NodeId);

    /// Add a network interceptor to the transport pipeline.
    pub fn add_interceptor(&mut self, interceptor: Box<dyn NetworkInterceptor>);

    /// Remove all interceptors (restore clean network).
    pub fn clear_interceptors(&mut self);
}
```

### DistributedStateEngine

A new public type in the `dstate` crate that wraps all core logic modules
for a single state type:

```rust
pub struct DistributedStateEngine<S, V> {
    shard: ShardCore<S, V>,
    sync_logic: SyncLogic,
    change_feed: ChangeFeedLogic,
    versioning: VersioningLogic,
    lifecycle: LifecycleLogic,
    diagnostics: DiagnosticsLogic,
    persistence_startup: PersistenceStartupLogic,
    clock: Arc<dyn Clock>,
}
```

**Per-state, not per-node**: Each engine manages one state type. For
multi-state scenarios (INT-10), a `MockNode` holds a `StateRegistry` of
multiple engines and routes inbound `WireMessage` by `state_name`.

**Clock injection**: The engine accepts `Arc<dyn Clock>` at construction,
ensuring `TestClock` is the single source of truth for `Generation`
incarnation values and timer evaluation. No `SystemTime::now()` calls.

**Action-based API**: Mutation and inbound methods return explicit outbound
actions rather than sending implicitly, so `MockTransport` can capture
and route them:

```rust
pub enum EngineAction {
    SendToAll(WireMessage),
    SendTo(NodeId, WireMessage),
    ScheduleDelayed(Duration, WireMessage),
}
```

This engine is the "missing link" between the pure logic modules and the
actor framework. It provides a single entry point for:

- **Mutations**: `mutate()` → applies mutation, determines sync action,
  returns `Vec<EngineAction>` for outbound messages.
- **Inbound sync**: `handle_inbound_sync(SyncMessage)` → processes
  snapshot/delta, updates views, returns response actions.
- **Inbound change feed**: `handle_inbound_change_feed(BatchedChangeFeed)`
  → marks stale peers.
- **Change feed flush**: `flush_change_feed()` → returns batched
  notifications if any pending (driven by `MockCluster.tick()`).
- **Cluster events**: `on_node_joined()` / `on_node_left()` → updates
  views, returns actions (e.g., send snapshot to new peer).
- **Queries**: `query()` → reads from view map with freshness checking.
- **Diagnostics**: `metrics()` / `health_status()` → sync health info.
- **Tick**: `tick(elapsed: Duration)` → advances internal timers
  (periodic sync, delayed sends), returns any triggered actions.

This type is useful beyond integration testing — it's the natural public
API for embedding dstate in any application.

## Test State Types

### SimpleCounter (DistributedState)

```rust
struct SimpleCounter { value: u64 }
```

Full state is the view. No delta support. Used for basic sync tests.

### ResourceMetrics (DeltaDistributedState)

```rust
struct ResourceState { cpu: f64, memory_mb: u64, internal_counter: u64 }
struct ResourceView { cpu: f64, memory_mb: u64 }
struct ResourceDelta { cpu: Option<f64>, memory_mb: Option<u64> }
enum ResourceChange { CpuUpdate(f64), MemoryUpdate(u64), Full }
```

Includes private fields (`internal_counter`), view projection, and delta
support. sync_urgency returns `Immediate` for large memory changes.

## Test Matrix

### Happy Path (PR 4)

| ID | Scenario | Nodes | Strategy | Validates |
|----|----------|-------|----------|-----------|
| INT-01 | Basic mutation propagates to all peers | 3 | ActivePush | Delta/snapshot delivery, view map update |
| INT-02 | Delta projection round-trip | 3 | ActivePush | project_view → serialize → deserialize → apply_delta |
| INT-03 | Change feed batching | 3 | FeedLazyPull | Multiple mutations → 1 batched notification |
| INT-04 | Change feed deduplication | 3 | FeedLazyPull | Same state mutated 5x → 1 notification per batch |
| INT-05 | Periodic full sync | 3 | PeriodicOnly(1s) | Snapshot arrives after interval elapses |
| INT-06 | Pull on stale query | 3 | FeedLazyPull | Query detects staleness → sends RequestSnapshot → gets FullSnapshot |
| INT-07 | Generation ordering | 2 | ActivePush | Higher generation accepted, lower discarded |
| INT-08 | Node join → snapshot exchange | 3→4 | ActivePush | New node receives current snapshots from existing nodes |
| INT-09 | Node leave → view removal | 4→3 | ActivePush | Departed node's view removed from all remaining nodes |
| INT-10 | Multiple state types in registry | 3 | Mixed | StateRegistry routes sync messages to correct shard |
| INT-11 | SyncUrgency::Immediate bypasses feed | 3 | FeedLazyPull | Critical mutation pushed immediately, not batched |
| INT-12 | SyncUrgency::Suppress skips broadcast | 2 | ActivePush | No outbound message generated |
| INT-13 | SyncUrgency::Delayed schedules timer | 2 | ActivePush | Message sent after delay (via TestClock advance) |
| INT-14 | Query freshness (fast path) | 3 | ActivePush | All views fresh → lock-free read, no actor involvement |
| INT-15 | Query freshness (slow path) | 3 | FeedLazyPull | Stale views detected → pull → fresh result returned |

### Fault Injection & Edge Cases (PR 5)

| ID | Scenario | Fault | Validates |
|----|----------|-------|-----------|
| FAULT-01 | Network partition (2+1 split) | Partition({0,1}, {2}) | Isolated node accumulates stale views; healing restores consistency |
| FAULT-02 | Total network failure | DropAll | Mutations succeed locally; sync resumes on recovery |
| FAULT-03 | Random packet loss (30%) | DropRate(0.3, seed) | Eventually consistent despite losses (periodic sync heals) |
| FAULT-04 | Byte corruption in transit | CorruptBytes(seed) | DeserializeError handled per VersionMismatchPolicy |
| FAULT-05 | Node crash + restart | Stop + rejoin | Incarnation bumps; fresh snapshots exchanged |
| FAULT-06 | Persistence failure on save | FailingPersistence | MutationError propagated to caller |
| FAULT-07 | Persistence failure on load (startup) | FailingPersistence | FallbackToEmpty with new incarnation |
| FAULT-08 | Wire version mismatch (KeepStale) | V1 ↔ V2 | Old view kept; warning logged; no crash |
| FAULT-09 | Wire version mismatch (DropAndWait) | V1 ↔ V2 | Peer view dropped; RequestSnapshot sent |
| FAULT-10 | 100 rapid mutations burst | None (stress) | Change feed batches correctly; no message explosion |
| FAULT-11 | Ghost node rejected | Departed node sends late message | LifecycleLogic.should_accept_from returns false |
| FAULT-12 | Delta gap detection | Skip ages (1→5) | GapDetected → full snapshot pull triggered |
| FAULT-13 | Single-node cluster | 1 node, no peers | Mutations succeed; queries return own view; no sync errors |
| FAULT-14 | Empty cluster bootstrap | 0→3 nodes | Nodes join sequentially; all converge on initial state |
| FAULT-15 | Asymmetric partition (one-way) | A→B dropped, B→A ok | One-way visibility; healing restores |
| FAULT-16 | Out-of-order delta delivery | Reorder(seed) | DeltaUpdate(age=5) before age=4 → GapDetected → pull |
| FAULT-17 | Duplicate + stale delivery | DuplicateAll + DelayTicks | Duplicate arrives after newer update; idempotent |
| FAULT-18 | Unknown state_name in SyncMessage | Manual injection | Ignored without panic; metrics/logging |
| FAULT-19 | RequestSnapshot storm under partition | Partition + stale queries | Bounded outbound requests; no explosion |
| FAULT-20 | Version mismatch recovery | V1→V2 upgrade mid-test | View recovers after compatible version arrives |
| FAULT-21 | Join/leave race with in-flight snapshot | Leave during snapshot send | No panic; no ghost re-add |
| FAULT-22 | Large payload (1MB+ snapshot) | None | Serialization handles large buffers correctly |

## Determinism Guidelines

To ensure fully deterministic, repeatable test execution:

1. **Clock**: All time comes from `TestClock` — no `SystemTime::now()` or
   `Instant::now()` anywhere in the test path. `DistributedStateEngine`
   accepts `Arc<dyn Clock>` for `Generation` incarnation values.

2. **HashMap ordering**: Tests must not assert on iteration order of
   `HashMap`-based collections (e.g., `ChangeFeedLogic` notifications).
   Sort results before comparison, or use `BTreeMap` in test paths.

3. **Seeded RNG**: All probabilistic interceptors (`DropRate`, `CorruptBytes`,
   `Reorder`) accept an explicit seed. Tests use fixed seeds for
   reproducibility.

4. **Tick phase ordering**: Each `tick()` executes phases in strict order:
   1. Deliver transport queue messages due at this tick
   2. Run all actor mailboxes to quiescence (process in `NodeId` order)
   3. Evaluate timer events due at/before current clock time
   4. Advance `TestClock` by tick duration

5. **Node processing order**: When multiple nodes are processed in a single
   tick, they are always processed in ascending `NodeId` order.

## Implementation Schedule

### PR 1 — Integration Plan Document (this PR)
- `docs/integration-test-plan.md` (this document)
- `dstate-integration/` crate scaffold (Cargo.toml, empty lib.rs)

### PR 2 — DistributedStateEngine
- New public type in `dstate` crate
- Wraps ShardCore + SyncLogic + ChangeFeedLogic + VersioningLogic +
  LifecycleLogic + DiagnosticsLogic + PersistenceStartupLogic
- Public methods: `mutate()`, `query()`, `handle_inbound_sync()`,
  `handle_inbound_change_feed()`, `on_node_joined()`, `on_node_left()`,
  `flush_change_feed()`, `metrics()`, `health_status()`
- Unit tests for engine lifecycle within dstate crate

### PR 3 — MockRuntime + MockTransport
- `MockRuntime` implementing `ActorRuntime` with single-threaded mailboxes
- `MockTransport` with byte-level serialization at node boundaries
- `NetworkInterceptor` trait + built-in interceptors
- `MockNode` + `MockCluster` orchestrator
- `TestClock` integration for deterministic time
- Self-tests validating mock framework behavior

### PR 4 — Core Integration Tests
- INT-01 through INT-15 (happy path scenarios)
- Test state fixtures: `SimpleCounter`, `ResourceMetrics`

### PR 5 — Fault Injection + Edge Case Tests
- FAULT-01 through FAULT-22
- Stress tests with high mutation rates
- Partition healing and recovery scenarios
- Out-of-order delivery and duplication
- Large payload handling
