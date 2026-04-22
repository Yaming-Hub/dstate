# dstate

Distributed state replication for Rust actor frameworks.

`dstate` provides a framework-agnostic core for managing replicated state
across nodes in a cluster. State changes are projected into public views,
synchronized via configurable strategies (active push, change-feed, periodic
sync), and persisted through a pluggable storage interface.

Actor runtime capabilities (spawning, messaging, timers, groups, cluster
discovery) are provided by the [`dactor`](https://crates.io/crates/dactor)
crate and its adapter crates (`dactor-ractor`, `dactor-kameo`, `dactor-coerce`).

## Workspace Crates

| Crate | Description |
|-------|-------------|
| [`dstate`](dstate/) | Core library — traits, types, replication logic, test support |
| [`dstate-integration`](dstate-integration/) | Integration tests for multi-node scenarios |

## Quick Start

Add `dstate` and a `dactor` adapter to your `Cargo.toml`:

```toml
[dependencies]
dstate = "1"
dactor-ractor = "0.2"   # or dactor-kameo, dactor-coerce
```

### Define a Distributed State

Implement `DistributedState` for simple state types where the entire state is
the public view:

```rust
use dstate::{DistributedState, DeserializeError};
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Counter {
    value: u64,
}

impl DistributedState for Counter {
    fn name() -> &'static str { "counter" }
    const WIRE_VERSION: u32 = 1;
    const STORAGE_VERSION: u32 = 1;

    fn serialize_state(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    fn deserialize_state(bytes: &[u8], _version: u32) -> Result<Self, DeserializeError> {
        bincode::deserialize(bytes).map_err(|e| DeserializeError::Malformed(e.to_string()))
    }
}
```

For state types with private fields, delta synchronization, or view
projections, implement `DeltaDistributedState` instead.

### Use with dactor

Use `dactor` for the actor runtime layer. `dstate` provides the replication
engine; `dactor` provides actor spawning, messaging, timers, and cluster
events. Your shell code bridges the two:

```rust
use dstate::engine::DistributedStateEngine;
use dstate::{NodeId, StateConfig, SyncStrategy};
use dactor::prelude::*;  // ActorRef, Handler, etc.
```

## Architecture

```
┌──────────────────────────────────────────────┐
│              Application Code                │
├──────────────────────────────────────────────┤
│                 Shell Layer                  │
│  (bridges dactor actors with dstate engine)  │
├──────────────┬───────────────────────────────┤
│    dstate    │          dactor               │
│  (state      │  (actor spawning, messaging,  │
│  replication │   timers, groups, cluster)    │
│  engine)     │                               │
├──────────────┤───────────────────────────────┤
│              │  dactor-ractor / dactor-kameo  │
│              │  / dactor-coerce               │
└──────────────┴───────────────────────────────┘
```

### Key Concepts

- **State** — The full internal data on each node (may contain private fields)
- **View** — A public projection of state, shared with peers
- **ViewDelta** — An incremental update to a view (optional, for bandwidth efficiency)
- **Generation** — `(incarnation, age)` pair for crdt-style conflict resolution
- **SyncStrategy** — Configures how and when state changes are replicated:
  - `active_push()` — Push every mutation immediately
  - `feed_lazy_pull()` — Batch changes, pull on query
  - `feed_with_periodic_sync()` — Change feed plus periodic full sync
  - `periodic_only()` — Only periodic full snapshots
- **ClusterEvents** — Subscribe to node join/leave notifications
- **StatePersistence** — Async save/load for crash recovery

## Testing

The core crate includes `test_support` with mock implementations:

- `TestClusterEvents` — Manually-triggered cluster event emitter
- `TestClock` — Deterministic clock with manual `advance()`
- `InMemoryPersistence` — In-memory save/load for testing
- `TestState` / `TestDeltaState` — Example state implementations

The `dstate-integration` crate provides higher-level test infrastructure
(`MockCluster`, `MockTransport`, fault injection interceptors) for
multi-node integration scenarios. For actor-level testing, see
[`dactor-mock`](https://crates.io/crates/dactor-mock) which provides an
in-memory mock cluster with fault injection at the actor framework layer.

### Test Suites

| Suite | Tests | Description |
|-------|-------|-------------|
| Unit tests | 205+ | Core logic, engine, sync, persistence, versioning |
| Happy path | 15 | Multi-node replication scenarios |
| Fault injection | 13 | Partitions, drops, corruption, duplicates |
| Stress | 3 | High throughput, 10-node mesh, large payloads |
| Chaos | 6 | Delays, packet loss, crash/rejoin, split-brain |
| Upgrade | 4 | Rolling V1→V2 upgrade, rollback, mixed versions |
| Harness | 5 | MockCluster API validation |
| dactor mock | 5 | Actor shell with mock cluster |

### Example

See [`dstate/examples/node_resource.rs`](dstate/examples/node_resource.rs) for
a complete `DeltaDistributedState` implementation with threshold-based sync
urgency, delta synchronization, and cross-node queries.

```rust
use dstate::test_support::test_runtime::TestClusterEvents;
use dstate::test_support::test_clock::TestClock;
use dstate::Clock;
```

Run all tests:

```bash
cargo test --workspace
```

## Migrating from v0.1

v1.0 removes the adapter crate pattern (`dstate-ractor`, `dstate-kameo`).
Instead, use `dactor` adapter crates directly:

| v0.1 | v1.0 |
|------|------|
| `dstate` + `dstate-ractor` | `dstate` + `dactor-ractor` |
| `dstate` + `dstate-kameo` | `dstate` + `dactor-kameo` |
| `dstate::ActorRuntime` trait | Removed — use `dactor` directly |
| `dstate::ActorRef<M>` trait | Removed — use `dactor::ActorRef<A>` |
| `dstate::NodeId(u64)` | `dstate::NodeId(String)` (re-exported from dactor) |

## License

MIT
