# Integration Test Plan — dstate

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
  deterministic.
- **Processing groups**: Local type-erased registry (same pattern as ractor/kameo
  adapters).
- **Timers**: Driven by `TestClock`. `send_interval` and `send_after` are
  evaluated on each `tick()` by comparing scheduled time against the clock.
- **Cluster events**: Callback-based subscription with snapshot pattern.

### MockTransport

Routes messages between `MockNode` instances:

- **Byte-level boundary**: All `SyncMessage` and `BatchedChangeFeed` values
  are serialized to `Vec<u8>` (via bincode) before crossing node boundaries
  and deserialized on the receiving end. This enforces that all wire types
  survive serialization round-trips.
- **Interceptor pipeline**: Messages pass through a chain of
  `NetworkInterceptor` implementations before delivery. Each interceptor
  can inspect, modify, delay, or drop messages.
- **Delivery queue**: Messages are queued and delivered on the next `tick()`,
  not immediately. This simulates network latency and allows tests to
  inspect queued messages before delivery.

### NetworkInterceptor

```rust
pub trait NetworkInterceptor: Send {
    /// Inspect a message in transit. Return `Some(bytes)` to deliver
    /// (possibly modified), or `None` to drop.
    fn intercept(
        &mut self,
        from: NodeId,
        to: NodeId,
        msg_bytes: &[u8],
    ) -> Option<Vec<u8>>;
}
```

Built-in interceptors:

| Interceptor | Behavior |
|-------------|----------|
| `PassThrough` | Deliver all messages unmodified |
| `DropAll` | Drop all messages (total network failure) |
| `DropRate(f64)` | Drop messages with given probability |
| `Partition(A, B)` | Drop messages between node sets A and B |
| `CorruptBytes` | Flip random bits in serialized data |
| `DelayTicks(n)` | Hold messages for n `tick()` calls before delivery |
| `DuplicateAll` | Deliver each message twice |

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

A new public type in the `dstate` crate (gated behind a feature flag or
always public) that wraps all core logic modules:

```rust
pub struct DistributedStateEngine<S, V> {
    shard: ShardCore<S, V>,
    sync_logic: SyncLogic,
    change_feed: ChangeFeedLogic,
    versioning: VersioningLogic,
    lifecycle: LifecycleLogic,
    diagnostics: DiagnosticsLogic,
    persistence_startup: PersistenceStartupLogic,
}
```

This engine is the "missing link" between the pure logic modules and the
actor framework. It provides a single entry point for:

- **Mutations**: `mutate()` → applies mutation, determines sync action,
  returns outbound messages to send.
- **Inbound sync**: `handle_inbound_sync(SyncMessage)` → processes
  snapshot/delta, updates views.
- **Inbound change feed**: `handle_inbound_change_feed(BatchedChangeFeed)`
  → marks stale peers.
- **Cluster events**: `on_node_joined()` / `on_node_left()` → updates
  views, returns actions (e.g., send snapshot to new peer).
- **Queries**: `query()` → reads from view map with freshness checking.
- **Diagnostics**: `metrics()` / `health_status()` → sync health info.

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
| FAULT-03 | Random packet loss (30%) | DropRate(0.3) | Eventually consistent despite losses (periodic sync heals) |
| FAULT-04 | Byte corruption in transit | CorruptBytes | DeserializeError handled per VersionMismatchPolicy |
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
- FAULT-01 through FAULT-14
- Stress tests with high mutation rates
- Partition healing and recovery scenarios
