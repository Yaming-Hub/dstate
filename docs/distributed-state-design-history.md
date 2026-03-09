# Distributed State Crate — Design History

This document tracks the evolution of the design spec from the original idea file
(`distributed-state-idea.md`) through each review iteration. Each entry records
the question or concern raised, the decision made, and the rationale.

---

## Iteration 1 — Initial Design Spec

**Ask:** Read the idea file and produce a detailed design document.

**Outcome:** Created `distributed-state-design.md` with 13 sections covering:
state model, sync strategies (periodic push / pull-on-query / delta streaming),
actor architecture, query & mutation APIs, persistence, cluster lifecycle,
registration flow, error handling, wire protocol, and a summary. Diagrams were
in ASCII art.

---

## Iteration 2 — Mermaid Diagrams

**Ask:** Replace ASCII art diagrams with Mermaid.

**Outcome:** All 8 diagrams replaced with Mermaid (`graph TD`,
`sequenceDiagram`, `flowchart`). No semantic changes.

---

## Iteration 3 — Type Erasure Strategy

**Ask:** How does the registry handle different state types? Compare approaches.

**Decision:** Chose **trait-object erasure** (`dyn AnyStateShard`) over
`Box<dyn Any>`.

**Rationale:** The registry must broadcast cluster events (`NodeJoined` /
`NodeLeft`) to all shards without knowing their concrete types. With
`Box<dyn Any>` the registry cannot call any methods — it would need to
downcast to every possible type. Trait-object erasure lets the registry call
`on_node_joined()` / `on_node_left()` through the `AnyStateShard` trait.

**Artifacts added:** Comparison table, `AnyStateShard` trait, blanket impl,
`StateRegistry` struct with `register()`, `lookup()`, broadcast methods.

---

## Iteration 4 — No `expect()` in Library Code

**Ask:** `expect()` panics are unacceptable in library code.

**Decision:** Introduced `RegistryError` enum (`StateNotRegistered`,
`TypeMismatch`). Changed `lookup()` to return `Result<_, RegistryError>`.
`QueryError` wraps `RegistryError` via a `From` impl.

**Exception kept:** `expect()` retained only for the "local node not in
view_map" case — a true invariant: the local node's entry is created at
construction time and never removed.

---

## Iteration 5 — State Identity

**Ask:** Is state identity by type? Is `name()` still needed?

**Decision:** Identity is by Rust type (`TypeId`). Since `name()` is a static
method, the type *is* the identity — there cannot be two instances of the same
type with different names.

`name()` is kept for **diagnostics only** (log messages, error messages,
metrics labels). It is *not* used as the registry key.

---

## Iteration 6 — Dynamic Sync Urgency

**Ask:** Can sync strategy adapt dynamically based on the weight of a delta?

**Decision:** Added `SyncUrgency` enum with four variants:

| Variant | Meaning |
|---|---|
| `Immediate` | Bypass the periodic timer; push now |
| `Delayed(Duration)` | Push after a custom delay |
| `Default` | Use the configured `SyncStrategy` |
| `Suppress` | Do not push this delta at all |

Added `sync_urgency(old, new, delta)` to the `DistributedState` trait with a
default impl returning `Default`. The `SyncEngine` dispatches based on the
returned urgency, overriding the static `SyncStrategy` in `StateConfig`.

**Example:** NodeResource — memory change >20% or CPU change >30% →
`Immediate`; memory change <1% and CPU change <2% → `Suppress`; otherwise
`Default`.

---

## Iteration 7 — Diagnostics Section

**Ask:** What diagnostic information can the system provide when a consumer
finds stale state? Think about distributed-system failure cases.

**Outcome:** Added Section 13 (Diagnostics) with:

- `PeerSyncStatus` struct tracking per-peer last-push, last-ack, consecutive
  failures, wire version.
- `StateSyncMetrics` for aggregate stats (push/pull counts, latencies, error
  counts by category).
- Failure scenario table covering 7 scenarios: network partition, slow
  consumer, version mismatch, node crash, clock skew, delta too large,
  suppression storm.
- Structured logging guidance using the `tracing` crate.
- Health check endpoint specification.

---

## Iteration 8 — Health Check Ownership

**Ask:** Is health check the responsibility of ractor? Justify adding it here.

**Decision:** This crate owns sync-layer health checks. Ractor handles
infrastructure health (node reachable? actor alive?) but is **blind to
sync-layer issues**: stale views, delta delivery failures, version mismatches,
suppression problems.

**Artifacts added:** Comparison table showing what each layer can observe:

| Concern | Ractor | This Crate |
|---|---|---|
| Node reachable? | ✅ | ❌ |
| Actor alive? | ✅ | ❌ |
| Views fresh? | ❌ | ✅ |
| Deltas flowing? | ❌ | ✅ |
| Versions compatible? | ❌ | ✅ |

---

## Iteration 9 — Versioning for Rolling Upgrades

**Ask:** Expand the versioning section — what should a state author do when
it receives a state update in a version it cannot understand?

**Decision:** Added `VersionMismatchPolicy` enum:

| Policy | Behavior |
|---|---|
| `RequestReserialization` | Ask sender to re-serialize in the receiver's version |
| `KeepStale` | Keep the last-known-good view; log a warning |
| `DropAndWait` | Drop the update; wait for next compatible push |

Added a fallback chain flowchart (Mermaid) and guidelines: prefer additive
changes with defaults, reserve breaking changes for major version bumps,
test multi-version scenarios.

---

## Iteration 10 — Three Versions Clarified

**Ask:** There are 3 versions (State struct, View, Delta) — which version
matters? Be clear.

**Decision:** Introduced two version domains:

| Domain | Scope | Crosses the wire? |
|---|---|---|
| `wire_version` | View + Delta | Yes — stamped on every `SyncMessage` |
| `storage_version` | State (full local state) | No — local persistence only |

View and Delta share a version because deltas are derived from view diffs —
they must agree on the same schema. Both are `const` associated values on the
`DistributedState` trait: `WIRE_VERSION` and `STORAGE_VERSION`.

Renamed all `serde_version` references throughout the document.

---

## Iteration 11 — `wire_version` Source

**Ask:** Where does the `wire_version` value come from?

**Clarification:** `WIRE_VERSION` is a compile-time associated constant on the
`DistributedState` trait. Generic actors (`SyncEngine<S>`, `StateShard<S>`)
access it as `S::WIRE_VERSION` — no runtime lookup or registration needed.
The value is baked into the binary at compile time.

---

## Iteration 12 — Backward-Compatible Upgrades

**Ask:** Can the design support a two-phase upgrade pattern where the first
deploy adds understanding of the new version but keeps sending the old version,
and the second deploy switches the outbound version?

**Decision:** Yes — this works because `WIRE_VERSION` controls the **outbound**
format, while `deserialize_view()` / `deserialize_delta()` independently
control **inbound** acceptance. The two are decoupled.

Documented a three-phase upgrade pattern:

| Phase | `WIRE_VERSION` | `deserialize_*` accepts | Risk |
|---|---|---|---|
| 1 — Add understanding | V1 | V1 + V2 | None — outbound unchanged |
| 2 — Switch outbound | V2 | V1 + V2 | None — all nodes understand V2 |
| 3 — Remove legacy | V2 | V2 only | None — no V1 senders remain |

Added code examples for each phase and a Mermaid sequence diagram showing
the rolling upgrade flow.

---

## Iteration 13 — Three-Phase Summary Table

**Ask:** The table summarizing the 3 phases is good — include it in the doc.

**Outcome:** Added the three-phase summary table inline in the versioning
section, placed before the detailed per-phase descriptions.

---

## Iteration 14 — NodeResource Sample Code

**Ask:** Add a sample code section showing a concrete end-to-end example.

**Outcome:** Added Section 14 (Sample Code — NodeResource) with:

- `NodeResource` state, `NodeResourceView`, `NodeResourceDelta` type definitions.
- Full `DistributedState` trait implementation.
- Registration with periodic push.
- Background resource monitor task using `sysinfo`.
- `find_lowest_memory_node()` query function.
- Mermaid cluster diagram.

---

## Iteration 15 — `mutate()` Key Code

**Ask:** Add key code in the `mutate()` method, especially how it interacts
with `wire_version`.

**Outcome:** Replaced `todo!()` placeholder with a full 8-step implementation:

1. Snapshot old view.
2. Apply caller's mutation.
3. Advance age and timestamp.
4. Project new view and compute delta.
5. Determine sync urgency.
6. Update local PublicViewMap entry.
7. Persist with `storage_version` stamp (if enabled).
8. Forward delta to `SyncEngine` with `wire_version` stamp.

Also added `SyncEngine` outbound path: `handle_outbound_delta()`,
`broadcast_delta_now()`, `send_snapshot()`.

---

## Iteration 16 — `query()` Key Logic

**Ask:** Implement the key logic for the `query()` function.

**Outcome:** Implemented the full query flow:

1. Load the current view map snapshot.
2. Detect stale entries based on `max_staleness`.
3. Issue parallel pulls to stale peers.
4. Deserialize responses with wire_version dispatch.
5. Update the view map with fresh entries.
6. Invoke the caller's projection function on the updated map.

---

## Iteration 17 — Error Handling Deferred

**Ask:** In the `query()` function, do not implement error handling code paths —
just add `todo!()`. Focus on the main logic.

**Outcome:** Error branches in `query()` replaced with `todo!()` comments.
Main (happy) path fully implemented.

---

## Iteration 18 — Thread Safety for `view_map`

**Ask:** The `view_map` is modified by proactive pushes, node join/leaves,
and query-time updates, and read by `query()`. How do we ensure thread safety?
Minimize locking overhead — ideally lock-free in most cases.

**Decision:** Use `ArcSwap` for the PublicViewMap.

**Design:**

| Operation | Mechanism | Locking |
|---|---|---|
| Read (query fast path) | `view_map.load()` — atomic pointer load | Lock-free |
| Write (mutate, sync, join/leave) | Clone → modify → `view_map.store(Arc::new(...))` | Actor mailbox (serialized) |

Added `StateShard` struct definition with `Arc<ArcSwap<HashMap<...>>>` field.

**Query two-path design:**

- **Fast path:** Load snapshot atomically, check freshness, all fresh → invoke
  `project()` directly. Zero actor calls, zero locking.
- **Slow path:** Stale entries found → send `RefreshAndQuery` message to actor →
  actor pulls, swaps in updated map, invokes projection.

Clone-and-swap is O(n) where n = number of nodes — negligible for typical
cluster sizes. Large `V` types can be wrapped in `Arc<V>`.

Updated both `query()` and `mutate()` to use the ArcSwap pattern.

---

## Iteration 19 — Persistence Expansion

**Ask:** Expand persistence design. How does `StatePersistence` work with
`StateShard`? What is the startup load process? When do writes happen? Who
controls writes — this crate or the application? What if load or save fails?

**Decisions:**

1. **Write ownership:** This crate controls write timing (save after every
   mutation). The application controls where to write (implements
   `StatePersistence`) and whether to enable persistence at all.

2. **Startup load:** `StateShard::pre_start()` calls `load()`. If the loaded
   `storage_version` differs from `S::STORAGE_VERSION`, the system calls
   `migrate_state()` (a new trait method with a default impl that rejects
   migration). On any failure (load error, migration error), the node starts
   with empty state and logs the error.

3. **Save failure rollback:** If `save()` fails, the mutation is fully rolled
   back — age reverted, old view_map restored via ArcSwap, delta not broadcast.
   `MutationError::PersistenceFailed` returned to caller.

4. **Crash safety:** Save happens before broadcast. If the node crashes after
   save but before broadcast, peers get the update on rejoin. If it crashes
   before save, the mutation is lost (acceptable — `mutate()` never returned
   `Ok`).

**Artifacts added:**

- §8.1: `PersistError` enum, expanded trait docs
- §8.2: Ownership table (what this crate vs application controls)
- §8.3: Startup flowchart (Mermaid), `pre_start()` code, `migrate_state()`
  trait method
- §8.4: Write-path sequence diagram (Mermaid), rollback code
- §8.5: Failure mode table (6 scenarios with behavior and recovery)
- §8.6: Design decision — why synchronous per-mutation save, future
  optimization options (WAL, debounce, async)
- 14 new test cases (PERSIST-07 through PERSIST-20) in the test plan
- Updated dev plan PR 6 scope and line estimate (1200 → 1500)

---

## Iteration 20 — Two-Trait Split and Delta-Aware Mutations

**Ask:** The design always does a full save for the state object — problematic
for large state. Can the mutation closure also return the change/diff, so we
can project it directly to a view delta (avoiding `diff()`)? Can persistence
receive both the full state and the delta? Can we define two flavors — one for
small state (simple) and one for large state (delta-aware)?

**Decisions:**

1. **Two traits:**
   - `DistributedState` — simple, for small state. Has `diff()`. Mutation
     closure: `FnOnce(&mut State)`.
   - `DeltaDistributedState` — delta-aware, for large state. Has `StateDeltaChange`
     type and `project_delta()`. Mutation closure:
     `FnOnce(&mut State) -> StateDeltaChange`. No `diff()` needed.

2. **Two mutation methods:**
   - `shard.mutate(closure)` — system snapshots old view, applies closure,
     computes `diff(old_view, new_view)`.
   - `shard.mutate_with_delta(closure)` — closure returns `StateDeltaChange`,
     system calls `project_delta(state_delta)`. No old view snapshot needed.

3. **Persistence receives optional delta:**
   `save(state, Option<&StateDeltaChange>)`. Simple mutations pass `None`. Delta-
   aware mutations pass `Some(state_delta)`. The backend chooses: full write,
   delta append, or targeted update.

4. **Shared commit path:** Both mutation methods converge on
   `commit_mutation()` for view_map update, persistence, rollback, and
   broadcast.

**Artifacts updated:**

- §2.1: Updated state composition diagram with both delta paths
- §2.2: Split into `DistributedState` (simple) + `DeltaDistributedState`
  (delta-aware) with comparison table and justification
- §7.2: Added `mutate_with_delta()`, `commit_mutation()` shared path, and
  comparison table
- §8.1: Updated `StatePersistence` trait with `StateDeltaChange` type, strategy
  table, and examples for full-write vs WAL backends
- §8.4: Updated sequence diagram for both paths
- Test plan: 4 new `DSM-*` tests, 6 new `DMUT-*` tests
- Dev plan: PR 1 and PR 2 scope expanded, totals updated

---

## Iteration 21 — Remove View/Delta from Simple State

**Ask:** For simple `DistributedState`, do we still need the View concept?
Given the state is small and simple, removing View should simplify the API.

**Decision:** Yes — remove View, Delta, `project_view()`, `diff()`, and
`apply_delta()` from the simple trait. The State IS what gets broadcast.

**Rationale:**

- For small state, View is typically identical to State (no private fields).
  The extra abstraction adds 7 methods with no benefit.
- Full-state broadcast is cheaper than snapshotting + diffing + serializing
  deltas for small objects.
- Receivers just replace their copy — no delta application logic needed.
- The simple trait goes from ~9 methods to 3 (`name`, `serialize_state`,
  `deserialize_state`).

**New two-tier design:**

| | `DistributedState` (Simple) | `DeltaDistributedState` (Delta-Aware) |
|---|---|---|
| Types | State only | State, View, Delta, StateDeltaChange |
| Methods | 3 | ~10 |
| Broadcast | Full state (`FullSnapshot`) | View deltas (`DeltaUpdate`) |
| Private fields | None | Yes (filtered by `project_view`) |
| `sync_urgency` | Not supported | Per-delta control |

**Artifacts updated:**

- §2.1: Simplified composition diagram — simple path shows State broadcast
  directly, delta-aware path shows full View/Delta model
- §2.2: `DistributedState` trait reduced to 3 methods; comparison table updated
- §7.2: Simple `mutate()` — no old_view snapshot, no diff, broadcasts
  `OutboundSnapshot`; added receiver-side `handle_inbound_snapshot()` code;
  removed shared `commit_mutation()` (each path is now self-contained)
- SyncEngine: added simple-state `handle_outbound_snapshot()` alongside
  delta-aware urgency dispatch
- Test plan: SM tests reduced to 3 (serialize/deserialize round-trip);
  DSM tests expanded to 10; MUT tests updated with full-state sync tests
  (MUT-04 through MUT-06)
- Dev plan: PR 1 estimate reduced to 700 lines

---

## Iteration 22 — Type Parameter Renames

**Ask:** Rename `D` → `VD` (ViewDelta), `SD` → `StateDeltaChange`, and
generic `T` → `S` (State) for clarity.

**Decision:** Applied all renames throughout the design spec.

**Rationale:** `D` and `SD` were ambiguous; `VD` (ViewDelta) and
`StateDeltaChange` make the two delta domains unambiguous. `S` is the
standard convention for state generics.

---

## Iteration 23 — Diagram Placement

**Ask:** Move composition diagrams into §2.1 (Simple) and §2.2 (Delta-Aware)
sections, placing each diagram directly above its type table.

**Decision:** Applied. Each section now has its own focused diagram.

---

## Iteration 24 — StateRegistry Is Not an Actor

**Ask:** Why is StateRegistry an actor?

**Decision:** Corrected — it is a plain `struct`, not an actor. Updated
architecture diagram and description.

**Rationale:** StateRegistry is just a `HashMap<TypeId, Box<dyn AnyStateShard>>`
wrapper. It doesn't process messages or need a mailbox. Making it an actor would
add unnecessary overhead.

---

## Iteration 25 — StateShard & SyncEngine Actor Details

**Ask:** Add more detail about what messages StateShard and SyncEngine process
and the key workflow for each message.

**Decision:** Added full message enums and step-by-step workflows.

**Artifacts updated:**

- §6.2: `StateShardMsg` enum (7 variants) with workflows for Mutate,
  MutateWithDelta, InboundSnapshot, InboundDelta, NodeJoined, NodeLeft,
  RefreshAndQuery
- §6.3: `SyncEngineMsg` enum (6 variants) with workflows for
  OutboundSnapshot, OutboundDelta, InboundWireMessage, TimerTick, PullView,
  SnapshotRequest

---

## Iteration 26 — Ractor Processing Group for Broadcasting

**Ask:** Consider using ractor processing group (`ractor::pg`) to broadcast
changes to all nodes instead of manually tracking peers.

**Decision:** Adopted. Each SyncEngine actor joins a processing group named
`distributed_state::<StateName>`. Broadcasting uses `pg::broadcast()` instead
of iterating a manual peer list.

**Rationale:**

- **Eliminates manual peer tracking** — ractor's pg handles membership
  automatically as nodes join/leave the cluster.
- **Simplifies node join/leave** — SyncEngine no longer needs NodeJoined/NodeLeft
  messages; the pg membership updates automatically.
- **Single-call broadcast** — `pg::broadcast()` replaces the `for peer in peers`
  loop, reducing code and error surface.
- **Crash cleanup** — ractor automatically removes crashed actors from the group.
- **Point-to-point still possible** — `PullView` looks up a specific peer via
  `pg::get_members()` for targeted snapshot requests.

**Self-message filtering:** Since `pg::broadcast()` delivers to all group
members including the sender, `InboundWireMessage` now checks
`source_node == self.node_id` and discards own messages.

**Artifacts updated:**

- §6.3: New "Broadcasting via Ractor Processing Groups" subsection with
  comparison table, code sample showing `pg::join()` and `pg::broadcast()`

---

## Iteration 32 — Address User PR Comments

**Ask:** User posted 3 comments on PR #4968909.

**Changes:**

1. **`name()` doc comment (thread 52584896):** Updated from "diagnostics and
   logging" to include change notifications, registry keys, and processing
   group names.
2. **`synced_time` redundancy (thread 52585139):** Removed `synced_time: i64`
   entirely. `synced_at: Instant` is now the sole freshness field. Updated
   all 6 code blocks, staleness check, Mermaid diagram, and diagnostics table.
3. **`Instant` vs `i64` clarification (thread 52585122):** Added doc comments
   explaining why `created_time`/`modified_time` are `i64` (serializable,
   cross-node) while `synced_at` is `Instant` (local-only, monotonic).

---

## Iteration 33 — Timer-Driven Work Section

**Ask:** Add a section after actor architecture describing interval-driven work.

**Outcome:** Added §6.6 Timer-Driven Work with:

- Timer inventory table (4 timers: change feed flush, periodic full sync,
  delayed delta send, pull timeout)
- Lifecycle diagram showing `pre_start()` initialization and runtime scheduling
- Mailbox interaction Mermaid sequence diagram
- Configuration summary table with defaults
- Tuning guidance for `batch_interval` vs `periodic_full_sync`

---

## Iteration 34 — Incarnation Threading

**Ask:** Crash-restart scenario: node restarts, all ages reset to 0, peers
reject updates because age < cached age.

**Decision:** Threaded the `incarnation: u64` field (previously only in §5.1)
through all structs, wire messages, and code samples.

**Artifacts updated:**

- `StateObject`, `StateViewObject`: added `incarnation` field
- `SyncMessage::FullSnapshot`, `SyncMessage::DeltaUpdate`: added `incarnation`
- `ChangeNotification`: added `incarnation`
- `ChangeFeedAggregator.pending`: keyed by `(state_name, source_node)` →
  `(incarnation, age)`
- Inbound snapshot handler: full `(incarnation, age)` comparison logic
- `MarkStale` handler: incarnation-aware comparison
- All mutation code blocks: include incarnation in outbound messages
- SyncEngine outbound handlers: include incarnation
- Startup/pre_start code: incarnation generation rules
- Added §9.3 Crash Restart section with Mermaid sequence diagram and scenario
  table
- Expanded §5.1 with incarnation generation rules table

---

## Iteration 35 — Incarnation Type Fix

**Ask:** `Uuid::now_v7()` returns 128 bits but incarnation is `u64` — type
mismatch.

**Decision:** Changed incarnation source from `Uuid::now_v7().as_u128() as u64`
(lossy truncation) to `current_unix_time_ms() as u64` (naturally u64,
time-ordered).

**Rationale:** Wall-clock milliseconds fit in u64, are naturally time-ordered,
and don't require the `uuid` dependency. The only requirement is that a newer
incarnation is numerically larger than the old one, which wall-clock ms
satisfies.

---

## Iteration 36 — Testability Section

**Ask:** Add a section ensuring core logic is easy to unit test without
spinning up a full actor cluster.

**Outcome:** Added §15 Testability with 6 subsections:

1. **Clock abstraction** — `trait Clock` with `SystemClock` (prod) and
   `TestClock` (controllable, injectable)
2. **ShardCore separation** — Pure logic struct extracted from actor shell;
   all ordering, view-map, staleness logic unit-testable without the runtime
3. **Key pure functions** — Table of 6 functions to unit test
4. **In-memory test doubles** — `InMemoryPersistence`, `FailingPersistence`
5. **TestCluster harness** — In-process multi-node with `settle()`,
   `crash_and_restart()`, `advance_time()`, shared TestClock
6. **Design rules** — 6 rules table (no I/O in core, injectable deps, etc.)

---

## Iteration 37 — Test Plan and Dev Plan Update

**Ask:** Review and update test plan and dev plan based on latest design.

**Test plan changes:**
- Fixed `synced_time` → `synced_at` references
- Added incarnation tests (INC-01–09)
- Added ChangeFeedAggregator tests (CFA-01–09)
- Added composable strategy tests (SYNC-19–22)
- Added crash restart tests (LIFE-07–10)
- Added request coalescing/snapshot/concurrent pulls (SHARD-07–10, QUERY-06–08)
- Added testability infrastructure tests (TEST-01–14)
- Total: 170+ test cases (was 120+)

**Dev plan changes:**
- Updated all 8 PRs with new components and test references
- Total: ~11,500 lines (was ~10,700)

---

## Iteration 38 — Example Code Update

**Ask:** §14 example code is out of date.

**Changes:**

- Changed trait from `DistributedState` to `DeltaDistributedState` (has
  private fields needing View/Delta separation)
- Added `NodeResourceChange` as `StateDeltaChange` type
- Replaced `diff()` with `project_delta()` for state→view delta projection
- Changed `mutate()` → `mutate_with_delta()` — closure now returns the
  change description
- Added `target_version` parameter to `serialize_view`/`serialize_delta`
- Updated registration to return `Result` and use `pull_timeout`
- Updated diagram labels from "periodic push (3s)" to "change feed + full
  sync (30s)"

---

## Iteration 39 — Actor Framework Abstraction

**Ask:** Decouple the design from ractor so it can work with any distributed
actor framework (ractor, kameo, actix + custom messaging, etc.). Use cargo
features to control which provider is compiled in.

**Decision:** Introduced an actor runtime abstraction layer with 5 abstract
traits covering the 5 required capabilities:

| Trait | Capability |
|---|---|
| `ActorRuntime` | Actor lifecycle (spawn), timer scheduling |
| `ActorRef<M>` | Fire-and-forget (`send`) and request-reply (`request`) messaging |
| `ProcessingGroup` | Join/leave/broadcast/get_members for named actor groups |
| `ClusterEvents` | Subscribe to node join/leave events |
| `TimerHandle` | Cancel recurring or one-shot timers |

**Cargo feature flags:** `runtime-ractor` (default), `runtime-kameo`,
`runtime-actix`, `runtime-custom`. Exactly one must be enabled; a
`compile_error!` fires otherwise.

**Module structure:** `src/runtime/{mod.rs, ractor.rs, kameo.rs, actix.rs}`
with conditional re-exports via `DefaultRuntime`.

**Ractor shown as reference provider (§6.0.4):** Mapping table from abstract
traits to ractor's concrete APIs (`cast→send`, `call→request`, `pg::*→group
methods`, etc.).

**Refactored throughout the document:**

- `ActorRef::cast()` → `ActorRef::send()`
- `ActorRef::call()` → `ActorRef::request()`
- `RpcReply<T>` → `ReplyChannel<T>`
- `ractor::pg::*` → `ProcessingGroup` trait methods
- `pre_start()` → `initialize()`
- `ActorError` → `ActorRequestError` / `ActorSendError`
- `StateRegistry` and `ChangeFeedAggregator` now generic over `R: ActorRuntime`
- All diagrams updated (`pg::broadcast()` → `group.broadcast()`)
- Error types use framework-agnostic error enums

**Artifacts added:**

- §6.0 Actor Runtime Abstraction (trait definitions + capability table)
- §6.0.3 Cargo Feature Flags (Cargo.toml, compile_error!, module structure)
- §6.0.4 Ractor Provider (mapping table + reference implementation skeleton)
- §6.0.5 Architecture Overview (updated diagram with generic labels)

**Artifacts updated:** §6.1–6.6, §7, §8.3, §9.3, §10, §13.3, §13.5, §15,
§16 Summary — all ractor-specific references replaced with abstract types

---

## Iteration 40 — Multi-Crate Architecture

**Ask:** Instead of cargo features within a single crate, use separate adapter
crates so that third parties can add new adapters without modifying the core.

**Decision:** Replaced cargo feature-based provider selection with a
multi-crate architecture:

| Crate | Role |
|---|---|
| `dstate` | Core library — traits, data types, pure logic (`ShardCore`), test doubles. **No actor framework dependency.** |
| `dstate-ractor` | Ractor adapter — `impl ActorRuntime for RactorRuntime`, actor shells wrapping `ShardCore`, `ractor_cluster` events |
| `dstate-kameo` | Kameo adapter (same pattern) |
| `dstate-actix` | Actix adapter (same pattern) |

**Rationale:**

- **Extensibility:** Third-party adapters can be created as independent crates
  without forking or modifying the core.
- **Dependency hygiene:** Applications only pull in the single framework they
  use. No optional dependencies or feature-flag complexity.
- **Re-export convenience:** Each adapter re-exports `dstate::*` so
  applications need only one dependency line.

**Core crate module layout:** `traits/` (state, runtime, persistence, clock),
`types/` (envelope, sync_message, config, errors, node), `core/` (shard_core,
sync_logic, change_feed, versioning), `messages/` (shard_msg, sync_msg,
feed_msg), `registry.rs`, `test_support/`.

**Adapter crate module layout:** `runtime.rs` (impl ActorRuntime),
`actors/` (shard_actor, sync_actor, feed_actor wrapping pure logic),
`cluster.rs` (impl ClusterEvents).

**Artifacts updated:**

- §6.0.3: Replaced "Cargo Feature Flags" with "Crate Architecture" showing
  multi-crate dependency graph, crate series table, usage examples, core
  module layout, adapter module layout, and module dependency table
- §6.0.4: Removed `#[cfg(feature)]` annotations; ractor provider now lives
  in the `dstate-ractor` crate
- §6.3 workflows: Updated all broadcast steps to reference `pg::broadcast()`
- §6.3 InboundWireMessage: Added self-message filtering step
- §6.3 PullView: Added note about point-to-point via `pg::get_members()`
- §6.4: Simplified — ClusterMembership no longer notifies SyncEngine; only
  StateShard needs NodeJoined/NodeLeft for view map updates
- Architecture diagram: Updated to show processing group

---

## Iteration 41 — Kameo Provider Detail

**Ask:** Add more detail about the `dstate-kameo` adapter crate.

**Outcome:** Added §6.0.5 Kameo Provider with:

- **Comparison table** — key API and architectural differences between ractor
  and kameo (messaging, actor definition, groups, cluster, remote refs, timers)
- **Module layout** — `dstate-kameo/src/` with `runtime.rs`, `actors/`
  (shard_actor, sync_actor, feed_actor), `group.rs`, `cluster.rs`
- **Trait mapping table** — all 11 abstract trait methods mapped to kameo APIs
  (`tell`/`ask`, `PubSub<M>`, `ActorSwarm`, tokio tasks for timers)
- **Processing group strategy** — kameo has no built-in `pg` module; adapter
  uses one `PubSub<M>` actor per group name from `kameo_actors` crate.
  Cross-node broadcasting via libp2p gossipsub. Includes implementation code.
- **Cluster events** — `KameoClusterEvents` subscribes to `ActorSwarm` peer
  connected/disconnected events, translates to `ClusterEvent::NodeJoined/Left`
- **Timer implementation** — lightweight tokio tasks wrapping `tell()` in a
  loop for `send_interval`, single-shot `tokio::spawn` for `send_after`,
  `JoinHandle::abort()` for cancellation. Messages enter actor mailbox so
  single-threaded guarantee is preserved.
- **Dependencies** — `dstate`, `kameo 0.19`, `kameo_actors 0.19`, `tokio`

**Renumbered:** Architecture Overview moved from §6.0.5 → §6.0.6

---

## Iteration 27 — Change Feed Detailed Design

**Ask:** Expand the change feed design. Frequently sending full change data
causes too much bandwidth. Instead, broadcast lightweight change notifications
every N seconds, and let peers pull the actual data lazily. Since notifications
are state-type-independent, batch them across all state types in one message.

**Decision:** Introduced the `ChangeFeedAggregator` actor (one per node) that
collects `NotifyChange` from all local SyncEngines, deduplicates by
`(state_name, source_node)`, and flushes a single `BatchedChangeFeed` via
its own processing group (`distributed_state::change_feed`).

**Key design elements:**

1. **Cross-state batching:** Notifications carry only `(state_name, source_node,
   new_age)` — no state-specific payload. A single batched message replaces
   N per-state broadcasts, achieving 100x–1000x traffic reduction for
   frequently-mutated states.

2. **`ChangeFeedAggregator` actor (§6.5):** One per node. Messages:
   `NotifyChange`, `FlushTick`, `InboundBatch`. Deduplication via
   `HashMap<(String, NodeId), u64>`. Timer-driven flush.

3. **`pending_remote_age` field:** Added to `StateViewObject<V>`. Set on
   receiving a change notification; checked in query fast path alongside
   `synced_time`; cleared after successful pull.

4. **`MarkStale` message:** Added to `StateShardMsg`. The aggregator routes
   inbound notifications to the appropriate StateShard via `StateRegistry`
   lookup and `AnyStateShard::on_mark_stale()`.

5. **`BatchedChangeFeed` wire message:** Separate from `SyncMessage` — sent
   via the dedicated `distributed_state::change_feed` pg, not the per-state
   SyncEngine groups.

6. **`ChangeFeedConfig`:** Node-level config (not per-state) with
   `batch_interval` (default 1s). `ActiveFeedLazyPull` strategy no longer
   carries its own interval.

**Artifacts updated:**

- §2.3: `StateViewObject<V>` — added `pending_remote_age: Option<u64>`
- §3.1: Fast path flowchart and code — checks `pending_remote_age` alongside
  `synced_time` staleness
- §4.2: Major expansion — 5 subsections: How It Works (sequence diagram),
  Cross-State Batching (struct + traffic comparison table), Aggregator Actor,
  Receiver Side (stale markers + code), Strategy Interaction table
- §6: Architecture diagram — added ChangeFeedAggregator with its own pg
- §6.2: `StateShardMsg` — added `MarkStale` variant; added workflow
- §6.2: `AnyStateShard` trait — added `on_mark_stale()` method + blanket impl
- §6.3: SyncEngine `InboundWireMessage` — removed `ChangeFeed` case (now
  handled by aggregator); `TimerTick` — sends to aggregator not pg
- §6.5: New section — full ChangeFeedAggregator actor design with messages,
  state, workflows, sequence diagram, and design decision table
- §7.1: Query fast path and slow path code — checks `pending_remote_age`;
  clears marker after pull
- §7.2: All `StateViewObject` constructors — added `pending_remote_age: None`
- §10: Registration diagram — shows aggregator spawn on `StateRegistry::new()`
- §10.1: Added `ChangeFeedConfig`, updated `SyncStrategy::ActiveFeedLazyPull`
  (removed per-state `batch_interval`)
- §12: Wire protocol — removed `SyncMessage::ChangeFeed`, added
  `ChangeNotification` + `BatchedChangeFeed` structs with design note

---

## Iteration 28 — Registration Name Uniqueness Validation

**Ask:** State is identified by both name and type. Registration should fail
if a different type tries to register under a name that's already taken.

**Decision:** `register()` now returns `Result<(), RegistryError>` and checks
for name collisions using `TypeId`. Same-type re-registration is idempotent
(no-op). Different-type collision returns `RegistryError::DuplicateName`.

**Rationale:**

- The registry uses `S::name()` as the key for wire protocol routing and
  ChangeFeedAggregator dispatch. Two different types sharing a name would cause
  silent data corruption (deserializing bytes into the wrong type).
- Fail-fast at registration time surfaces the bug immediately rather than
  producing mysterious runtime errors.
- Storing `TypeId` alongside each entry costs one `usize` and enables the check.
- Same-type re-registration is accepted as a no-op for convenience (avoids
  forcing callers to track whether they've already registered).

**Artifacts updated:**

- §6.1: `StateRegistry.shards` type changed from `HashMap<String, Box<dyn AnyStateShard>>`
  to `HashMap<String, (TypeId, Box<dyn AnyStateShard>)>`
- §6.1: `register()` now returns `Result<(), RegistryError>`, checks `TypeId`
  collision, idempotent for same type
- §6.1: `lookup()` and broadcast methods updated to destructure the tuple
- §11.2: Added `RegistryError::DuplicateName { name, existing_type, new_type }`

---

## Iteration 29 — Async Persistence Trait

**Ask:** The `load` and `save` methods on `StatePersistence` should be async.

**Decision:** Added `#[async_trait]` to `StatePersistence` and made both
`save()` and `load()` async functions.

**Rationale:** Real-world persistence backends (databases, cloud blob stores,
networked file systems) are inherently async. Forcing synchronous I/O would
block the actor's tokio runtime thread, degrading throughput. Since the
`StateShard` actor already runs in an async context (ractor actors are
async), `.await`-ing persistence calls is natural and zero-cost.

**Artifacts updated:**

- §8.1: `StatePersistence` trait — added `#[async_trait]`, `async fn save()`,
  `async fn load()`
- §8.1: FileBackend example — `async fn save` using `tokio::fs::write`
- §8.1: WalBackend example — `async fn save`, `async fn load`
- §7.2: Simple mutation — `persistence.save(...).await`
- §7.2: Delta-aware mutation — `persistence.save(...).await`
- §8.3: Startup load — `persistence.load().await`

---

## Iteration 30 — Composable Sync Strategies

**Ask:** `SyncStrategy` is an enum so authors can only pick one strategy.
How can they mix strategies, e.g. `ActiveFeedLazyPull` + `PeriodicPush`
every 15 minutes?

**Decision:** Replaced the `SyncStrategy` enum with a composable struct
containing a `PushMode` (pick one) plus optional layers that combine freely.

**New design:**

- `PushMode` enum (`ActivePush`, `ActiveFeedLazyPull`, `None`):
  how each individual mutation is propagated.
- `periodic_full_sync: Option<Duration>`: optional background timer that
  broadcasts a full snapshot at the configured interval, bounding max staleness.
- `pull_on_query: bool`: whether the query slow-path pulls from stale peers.

Convenience constructors: `active_push()`, `feed_lazy_pull()`,
`feed_with_periodic_sync(interval)`, `periodic_only(interval)`.

**Rationale:**

- Strategies are **orthogonal concerns**: push mode, background sync, and
  query-time pull operate independently.
- An enum forces mutual exclusion; a struct with optional layers lets authors
  compose exactly the behavior they need.
- `periodic_full_sync` acts as a safety net: even if change feed notifications
  or deltas are lost, peers get a full refresh at bounded intervals.

**Artifacts updated:**

- §4.5: Replaced "Choosing a Strategy" table with "Composing Strategies"
  section including composition diagram and example table
- §4.6: Updated to reference `push_mode` instead of `SyncStrategy`
- §6.3 OutboundSnapshot: Dispatches based on `push_mode`
- §6.3 OutboundDelta: Default urgency dispatches based on `push_mode`
- §6.3 TimerTick: Simplified to handle `periodic_full_sync` only
- §10.1: `SyncStrategy` is now a struct with `PushMode` + layers;
  `StateConfig` removed `sync_interval` (superseded by `periodic_full_sync`);
  convenience constructors added; `ChangeFeedConfig` kept separate
- §14.3: Example registration updated to `feed_with_periodic_sync(30s)`

---

## Iteration 31 — Multi-Model Design Review

**Ask:** Review the design using Claude Sonnet 4.6, GPT-5.2, and Gemini 3 Pro.
Address valid comments, resolve others with rationale.

**21 comments reviewed; 16 accepted and fixed, 5 declined:**

**Accepted fixes:**

| # | Source | Issue | Fix |
|---|---|---|---|
| 1 | Claude Sonnet + GPT-5.2 | `sync_urgency` receives same view for old/new | Capture old_view from view map before mutation |
| 2 | Claude Sonnet | `StateShardMsg` references delta types on simple trait | Added note: two separate msg enums in implementation |
| 3 | Claude Sonnet | `StateConfig::persistence` has `???` type | Made `StateConfig<S>` generic over `S: DistributedState` |
| 4 | Claude Sonnet | Sequential peer pulls block actor | Concurrent pulls via `join_all` |
| 5 | Claude Sonnet | `Suppress` can silently drop all updates | Added warning + registration-time check |
| 6 | Claude Sonnet + GPT-5.2 | Rollback exposes uncommitted state | Persist before publishing; mutate cloned state |
| 7 | Claude Sonnet | `PullView` timeout unconfigurable | Added `pull_timeout: Duration` to `StateConfig` |
| 8 | GPT-5.2 | Owner restart age=0 rejected by peers | Added `incarnation: u64` with `(incarnation, age)` ordering |
| 9 | GPT-5.2 | Wall-clock freshness unreliable | Added `synced_at: Instant`; use monotonic clock for checks |
| 10 | GPT-5.2 | `RequestSnapshot` lacks target age | Added `min_required_age: Option<u64>` |
| 11 | GPT-5.2 | Query API mixes generics with `Box<dyn Any>` | Added `snapshot()` API; added `Send + 'static` bounds |
| 12 | Gemini 3 Pro | `RequestReserialization` unimplementable | Added `target_version: u32` to serialize methods |
| 13 | Gemini 3 Pro | Thundering herd on concurrent queries | Added request coalescing for in-flight pulls |
| 14 | Gemini 3 Pro | Missing `Send` bound on closures | Added `+ Send` to `FnOnce` in message enum and mutation signatures |
| 15 | Gemini 3 Pro | `AnyStateShard` uses `async_trait` unnecessarily | Removed `#[async_trait]`; methods are now synchronous |

**Declined (with rationale):**

| # | Source | Issue | Reason |
|---|---|---|---|
| 1 | GPT-5.2 + Gemini | O(N) ArcSwap clone scalability | Acknowledged; acceptable for v1 cluster sizes. `Arc<V>` optimization documented. |
| 2 | GPT-5.2 | Error handling has `todo!()` | Intentional per iteration 17 — user deferred error handling |
| 3 | Gemini 3 Pro | `String` state_name on wire | Debuggability trade-off; batching amortizes overhead |
