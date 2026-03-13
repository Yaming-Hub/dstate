# dstate

Distributed state replication for Rust actor frameworks.

`dstate` provides a framework-agnostic core for managing replicated state
across nodes in a cluster. State changes are projected into public views,
synchronized via configurable strategies (active push, change-feed, periodic
sync), and persisted through a pluggable storage interface.

## Workspace Crates

| Crate | Description |
|-------|-------------|
| [`dstate`](dstate/) | Core library — traits, types, replication logic, test support |
| [`dstate-ractor`](dstate-ractor/) | Adapter for the [ractor](https://crates.io/crates/ractor) actor framework |
| [`dstate-kameo`](dstate-kameo/) | Adapter for the [kameo](https://crates.io/crates/kameo) actor framework |

## Quick Start

Add the core crate and an adapter to your `Cargo.toml`:

```toml
[dependencies]
dstate = "0.1"
dstate-ractor = "0.1"   # or dstate-kameo = "0.1"
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
projections, implement `DeltaDistributedState` instead. See the example
programs in each adapter crate's `examples/` directory.

### Use an Actor Runtime

```rust
use dstate::{ActorRuntime, ActorRef};
use dstate_ractor::RactorRuntime;  // or dstate_kameo::KameoRuntime

#[tokio::main]
async fn main() {
    let runtime = RactorRuntime::new();

    // Spawn an actor that handles u64 messages
    let actor = runtime.spawn("counter", |msg: u64| {
        println!("Received: {msg}");
    });

    // Fire-and-forget send
    actor.send(42).unwrap();
}
```

## Architecture

```
┌─────────────────────────────────────────────┐
│              Application Code               │
├─────────────────────────────────────────────┤
│     dstate (core traits + replication)      │
│  ┌─────────┐  ┌──────────┐  ┌───────────┐  │
│  │  State   │  │  Sync    │  │ Persistence│  │
│  │  Traits  │  │ Strategy │  │  Traits    │  │
│  └─────────┘  └──────────┘  └───────────┘  │
├──────────────────┬──────────────────────────┤
│  dstate-ractor   │     dstate-kameo         │
│  (ractor adapter)│     (kameo adapter)      │
└──────────────────┴──────────────────────────┘
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
- **ActorRuntime** — Framework-agnostic actor spawning, timers, and groups
- **ClusterEvents** — Subscribe to node join/leave notifications
- **StatePersistence** — Async save/load for crash recovery

### Choosing an Adapter

| Feature | `dstate-ractor` | `dstate-kameo` |
|---------|----------------|----------------|
| Framework | [ractor](https://crates.io/crates/ractor) | [kameo](https://crates.io/crates/kameo) |
| Mailbox | Unbounded | Bounded (default) |
| Spawn | Async (bridge thread) | Sync (cheaper) |
| Send | `cast()` fire-and-forget | `tell().try_send()` fire-and-forget |
| Timers | tokio tasks | tokio tasks |

Both adapters expose identical `ActorRuntime` semantics. Choose based on your
preferred actor framework.

## Testing

The core crate includes `test_support` with mock implementations:

- `TestRuntime` — In-memory actor runtime with channel-based mailboxes
- `TestClock` — Deterministic clock with manual `advance()`
- `InMemoryPersistence` — In-memory save/load for testing
- `TestState` / `TestDeltaState` — Example state implementations

```rust
use dstate::test_support::{TestRuntime, InMemoryPersistence};
use dstate::Clock;
use dstate::test_support::test_clock::TestClock;
```

Run all tests:

```bash
cargo test --workspace
```

## License

MIT
