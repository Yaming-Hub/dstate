# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
