# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] — Unreleased

### Breaking Changes

- **`NodeId` changed from `NodeId(pub u64)` to `NodeId(String)`** — Re-exported from
  `dactor::NodeId`. Constructors change from `NodeId(1)` to `NodeId("1".to_string())`.
  `NodeId` is no longer `Copy` (only `Clone`), so `.copied()` becomes `.cloned()` and
  explicit `.clone()` calls are needed where values were previously copied. Map/set
  ordering is now lexicographic (String) rather than numeric (u64).
- **Removed `ActorRuntime` trait** — Actor runtime abstraction is now provided by
  the [`dactor`](https://crates.io/crates/dactor) crate. Use `dactor-ractor`,
  `dactor-kameo`, or `dactor-coerce` for a concrete backend.
- **Removed `ActorRef<M>` trait** — Use `dactor::ActorRef<A>` instead.
- **Removed `TestRuntime`** — The mock actor runtime for testing. Use
  `dactor`'s test support or test the engine directly with `DistributedStateEngine`.
- **Removed `TestActorRef<M>`** — Replaced by dactor's test actor references.
- **Removed adapter crates** — `dstate-ractor`, `dstate-kameo`, and `dstate-e2e`
  are no longer part of this workspace. Use `dactor` adapter crates directly.
- **Error types re-exported from dactor** — `ActorSendError`, `ClusterError`,
  `GroupError` are now re-exported from `dactor::errors`.
- **`ClusterEvent`, `ClusterEvents`, `SubscriptionId`, `TimerHandle`** are now
  re-exported from `dactor`. `ClusterEvent` is `#[non_exhaustive]`; match
  statements require a wildcard arm.

### Added

- `dactor` dependency for actor framework integration.
- `TestTimerHandle::new()` constructor for creating test timer handles.
- **`dstate-dactor` crate** — Actor hosting for `DistributedStateEngine` using the
  `dactor` framework. Provides `StateActor<S, V>`, a thin wrapper that bridges the
  engine's action-based API with dactor's message-passing model. Includes typed messages
  (`Mutate`, `QueryViews`, `InboundSync`, `ClusterChange`, `PeriodicSync`,
  `FlushChangeFeed`), a `PeerSender` trait for abstracting peer routing, and
  `ActorRefPeerSender` for concrete dactor `ActorRef` integration.
- **Engine demo** (`dstate/examples/engine_demo.rs`) — A runnable multi-node
  example demonstrating the `DistributedStateEngine` API: cluster setup,
  mutations, action routing, node departure, metrics, and health checks.
- **NodeResource example** (`dstate/examples/node_resource.rs`) — A complete
  `DeltaDistributedState` implementation demonstrating private vs. public fields,
  delta synchronization, threshold-based sync urgency, and cross-node queries.
- **Stress tests** (`dstate-integration/tests/stress_tests.rs`) — STRESS-01 to 03:
  high-throughput sequential mutations, 10-node full mesh convergence, large payload sync.
- **Chaos tests** (`dstate-integration/tests/chaos_tests.rs`) — CHAOS-01 to 06:
  delayed messages, best-effort delivery under packet loss, crash/rejoin,
  rapid join/leave cycles, split-brain partition and heal, generation ordering
  under clock skew.
- **Upgrade tests** (`dstate-integration/tests/upgrade_tests.rs`) — UPGRADE-01 to 04:
  rolling phase 1 upgrade (read V1+V2, send V1), rolling phase 2 (send V2),
  partial rollback, mixed-version steady state under load.
- **Harness tests** (`dstate-integration/tests/harness_tests.rs`) — TEST-11 to 14:
  MockCluster creation, settle semantics, crash/restart simulation, clock control.

### Changed

- `TestClusterEvents` and `TestTimerHandle` remain in `test_support::test_runtime`
  for testing cluster event handling and timer cancellation without an actor framework.
- Updated design document (§1, §6.0, §16) to reflect dactor-based architecture.
- **`dstate-integration` now uses `dstate-dactor::StateActor`** for dactor-mock
  functional tests instead of its own `DstateActor` shell. The custom `shell` module
  has been removed. Tests use `PartitionAwarePeerSender` to wrap `ActorRefPeerSender`
  with `MockNetwork::can_deliver()` checks for network partition simulation.

### Migration Guide

Replace your dependency on `dstate-ractor` or `dstate-kameo` with the
corresponding `dactor` adapter crate:

```toml
# Before (v0.1)
dstate = "0.1"
dstate-ractor = "0.1"

# After (v1.0)
dstate = "1"
dactor-ractor = "0.2"
```

Code that implemented `dstate::ActorRuntime` should now use `dactor` traits
directly. The dstate core engine (`DistributedStateEngine`) is unchanged —
it remains runtime-agnostic and does not require any actor framework.

## [0.1.0] — Unreleased

Initial release of the dstate distributed state replication workspace.

### Added

#### Core (`dstate`)
- `DistributedState` and `DeltaDistributedState` traits for defining replicated state types
  with optional view projection and delta synchronization
- `ActorRuntime` trait abstracting actor spawning, timers, and processing groups
- `ClusterEvents` trait for node join/leave subscription
- `StatePersistence` trait for async save/load with version migration
- `Clock` trait with `SystemClock` (production) and `TestClock` (deterministic testing)
- `StateRegistry` for type-safe state registration and lookup
- `ShardCore` replication engine with generation-based conflict resolution
- `SyncStrategy` configuration: active push, change-feed, periodic sync
- `ChangeFeedLogic` for batched change notification aggregation
- `PersistenceStartupLogic` for crash recovery with version migration
- `VersioningLogic` for wire protocol compatibility and two-phase upgrade
- `SyncLogic` for configurable sync action resolution
- `LifecycleLogic` for node join/leave and group management
- `DiagnosticsLogic` for health checks, metrics, and logging hooks
- `StateObject<S>` and `StateViewObject<V>` envelopes with generation metadata
- `SyncMessage` wire protocol (FullSnapshot, DeltaUpdate, RequestSnapshot)
- `test_support` module with `TestRuntime`, `InMemoryPersistence`, `TestState`,
  and `TestDeltaState` for downstream testing

#### Ractor Adapter (`dstate-ractor`)
- `RactorRuntime` implementing `ActorRuntime` using ractor actors
- `RactorClusterEvents` with Arc callback snapshot pattern
- Actor spawning via sync→async bridge thread
- Timer scheduling via tokio tasks
- Local type-erased processing group registry

#### Kameo Adapter (`dstate-kameo`)
- `KameoRuntime` implementing `ActorRuntime` using kameo actors
- `KameoClusterEvents` with Arc callback snapshot pattern
- Synchronous actor spawning via kameo's `Spawn::spawn()`
- Non-blocking `tell().try_send()` for all message delivery
- Timer scheduling via tokio tasks with `Drop`-based abort
- Local type-erased processing group registry
