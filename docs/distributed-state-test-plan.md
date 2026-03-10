# Distributed State — Test Plan

This test plan covers the distributed state **multi-crate workspace** as
specified in [distributed-state-design.md](./distributed-state-design.md).
Tests are organized by design section and grouped into unit, integration,
and stress/chaos categories.

**Multi-crate testing strategy:**

| Crate | What's tested | Runtime used |
|---|---|---|
| `dstate` (core) | All pure logic (`ShardCore`, `sync_logic`, `change_feed`, `versioning`), types, traits, test doubles | `TestRuntime` (mock `ActorRuntime` in `test_support/`) |
| `dstate-ractor` | Adapter conformance, actor wrappers, real ractor cluster integration | `RactorRuntime` |
| `dstate-kameo` | Adapter conformance, PubSub groups, ActorSwarm cluster, tokio timer tasks | `KameoRuntime` |

Core crate tests (§1–§15) use `TestRuntime` and require no framework
dependency. Adapter-specific tests are in §16.

---

## 1. State Model (Design §2)

### 1.1 DistributedState Trait (Simple) — Unit Tests

| ID | Test | Description |
|---|---|---|
| SM-01 | Serialize/deserialize State round-trip | `deserialize_state(serialize_state(s), WIRE_VERSION)` must equal `s`. |
| SM-02 | `deserialize_state` rejects unknown wire_version | Call `deserialize_state` with an unsupported `wire_version`, expect `DeserializeError::unknown_version`. |
| SM-03 | State is fully public — all fields visible to peers | Serialize a State, deserialize on a peer, verify all fields are accessible. No hidden/private data. |

### 1.2 DeltaDistributedState Trait — Unit Tests

| ID | Test | Description |
|---|---|---|
| DSM-01 | `project_view` extracts public fields only | Mutate private fields on State, call `project_view`, verify View contains only public fields and private fields are absent. |
| DSM-02 | `project_delta` converts StateDeltaChange to Delta | Create a `StateDeltaChange`, call `project_delta`, verify the returned Delta matches the expected view-level change. |
| DSM-03 | `project_delta` + `apply_delta` round-trip | Apply a StateDeltaChange to a State, project the delta, apply it to the old View. Verify the result matches `project_view(new_state)`. |
| DSM-04 | `project_delta` filters private changes | Create a `StateDeltaChange` that includes both public and private field changes. Verify `project_delta` produces a Delta containing only public field changes. |
| DSM-05 | Multiple `StateDeltaChange` compose correctly | Apply two sequential StateDeltaChange changes, project each to Delta, apply both to a View. Verify the result matches the final `project_view`. |
| DSM-06 | `apply_delta` advances a View | Apply a Delta to an old View, verify the result matches the expected new View. |
| DSM-07 | Serialize/deserialize View round-trip | `deserialize_view(serialize_view(v), WIRE_VERSION)` must equal `v`. |
| DSM-08 | Serialize/deserialize Delta round-trip | `deserialize_delta(serialize_delta(d), WIRE_VERSION)` must equal `d`. |
| DSM-09 | `deserialize_view` rejects unknown wire_version | Call `deserialize_view` with an unsupported `wire_version`, expect `DeserializeError::unknown_version`. |
| DSM-10 | `deserialize_delta` rejects unknown wire_version | Same as DSM-09 for deltas. |

### 1.3 Envelope Types — Unit Tests

| ID | Test | Description |
|---|---|---|
| ENV-01 | `StateObject` age starts at 0 | A newly created `StateObject` has `age == 0`, meaning no data. |
| ENV-02 | `StateObject` timestamps set correctly | Verify `created_time` is set at creation and `modified_time` updates on mutation. |
| ENV-03 | `StateViewObject` tracks `synced_at` | After sync, `synced_at` reflects the current monotonic instant (used for freshness checks). |
| ENV-04 | `storage_version` is set from `STORAGE_VERSION` | A persisted `StateObject` carries the trait's `STORAGE_VERSION` constant. |
| ENV-05 | `wire_version` is set from `WIRE_VERSION` | A synced `StateViewObject` carries the sender's `WIRE_VERSION` constant. |
| ENV-06 | `incarnation` included in `StateObject` | A new `StateObject` carries a valid `incarnation` value. |
| ENV-07 | `incarnation` propagated to `StateViewObject` | After sync, `StateViewObject.incarnation` matches the source `StateObject.incarnation`. |

---

## 2. Node-Local Data Layout (Design §3)

### 2.1 ViewMap — Unit Tests

`ViewMap<V>` is the internal implementation (`core/view_map.rs`) that uses
a two-level `ArcSwap` for O(1) per-node updates without cloning the entire
map on each change.

| ID | Test | Description |
|---|---|---|
| PVM-01 | Own node entry exists after registration | After registering a state type, the `ViewMap<V>` contains an entry for the local `NodeId`. |
| PVM-02 | Own node entry derives from local shard | Mutate the local shard, verify the local `ViewMap<V>` entry reflects the projected View at the current `Generation`. |
| PVM-03 | Map is empty of peers on single-node cluster | On a single node, only the local entry exists. |

---

## 3. Synchronization Strategies (Design §4)

### 3.1 Active Push — Integration Tests

| ID | Test | Description |
|---|---|---|
| SYNC-01 | Delta pushed immediately on mutation | Mutate state on Node A, verify Node B receives the delta within a short bound (< 100ms network latency). |
| SYNC-02 | Full View pushed on first sync | On initial connection, Node A sends a full snapshot (not a delta). |
| SYNC-03 | Multiple rapid mutations produce multiple deltas | Mutate 10 times rapidly on Node A, verify Node B receives all 10 deltas in order. |

### 3.2 Active Feed + Lazy Pull — Integration Tests

| ID | Test | Description |
|---|---|---|
| SYNC-04 | Change feed notification is sent on mutation | Mutate on Node A, verify the `ChangeFeedAggregator` receives a `NotifyChange` message. |
| SYNC-05 | Change feeds are batched by interval | Mutate 5 times within the batch interval, verify peers receive a single `BatchedChangeFeed` containing one notification (deduped). |
| SYNC-06 | Pull triggered on query with stale view | Node B receives a change feed, then queries with freshness. Verify Node B pulls the latest view from Node A before returning. |
| SYNC-07 | No pull if view is fresh | Node B has a recent view. Query with a generous freshness window. Verify no pull request is sent. |

### 3.3 Change Feed Aggregator — Unit Tests

| ID | Test | Description |
|---|---|---|
| CFA-01 | NotifyChange deduplicates by `(state_name, source_node)` | Send 3 NotifyChange for the same state with ages 1, 2, 3. Verify pending map has only one entry at age 3. |
| CFA-02 | FlushTick sends `BatchedChangeFeed` | Accumulate 2 notifications, send FlushTick. Verify a `BatchedChangeFeed` is broadcast via pg. |
| CFA-03 | Empty pending map → no flush | No notifications since last flush. Send FlushTick. Verify no broadcast. |
| CFA-04 | InboundBatch routes to correct StateShard via registry | Receive a `BatchedChangeFeed` with 2 different state names. Verify each StateShard receives the correct `MarkStale`. |
| CFA-05 | Self-message filtered | Receive `BatchedChangeFeed` where `source_node == self.node_id`. Verify no routing — batch ignored. |
| CFA-06 | Cross-state batching | Mutate 2 different state types within one batch interval. Verify a single `BatchedChangeFeed` carries notifications for both states. |
| CFA-07 | Higher incarnation wins in dedup | Send NotifyChange `(incarnation=100, age=50)` then `(incarnation=200, age=1)`. Verify pending entry is `(200, 1)`. |
| CFA-08 | MarkStale sets `pending_remote_age` | StateShard receives `MarkStale(source, incarnation, age)`. Verify the peer's `pending_remote_age` is set. |
| CFA-09 | MarkStale with stale incarnation is no-op | Peer cached at `(incarnation=200, age=5)`. Receive MarkStale `(incarnation=100, age=99)`. Verify no-op. |

### 3.4 Periodic Full Sync — Integration Tests

| ID | Test | Description |
|---|---|---|
| SYNC-08 | Full snapshot pushed at configured interval | Configure `periodic_full_sync = 3s`. Mutate on Node A. Verify Node B receives a full snapshot within 3s (not immediately). |
| SYNC-09 | Multiple mutations collapsed into one periodic push | Mutate 5 times within one interval. Verify Node B receives a single `FullSnapshot` covering the latest state. |
| SYNC-10 | No push when state unchanged | No mutations during an interval. Verify no sync message is sent. |

### 3.5 Pull with Freshness — Integration Tests

| ID | Test | Description |
|---|---|---|
| SYNC-11 | Fresh view returns immediately | View was synced 2s ago, query with `max_staleness = 5s`. Verify result returned without network call. |
| SYNC-12 | Stale view triggers pull | View was synced 10s ago (via TestClock), query with `max_staleness = 5s`. Verify a pull request is sent and the result reflects the latest state. |
| SYNC-13 | Pull failure returns `StalePeer` error | View is stale and the owner is unreachable. Verify `QueryError::StalePeer` is returned. |

### 3.6 Composable Strategy — Integration Tests

| ID | Test | Description |
|---|---|---|
| SYNC-19 | ActivePush + periodic_full_sync | Configure `push_mode = ActivePush` and `periodic_full_sync = Some(30s)`. Verify mutations push immediately AND a full snapshot fires after 30s. |
| SYNC-20 | ActiveFeedLazyPull + periodic_full_sync | Configure feed + 15-min periodic. Verify notifications fire on mutation AND a full snapshot fires at the periodic interval. |
| SYNC-21 | `pull_on_query` without any push mode | Configure `push_mode = None`, `periodic_full_sync = None`, `pull_on_query = true`. Verify mutations don't push, but queries pull fresh data. |
| SYNC-22 | Suppress safety warning at registration | Register with `sync_urgency` always returning `Suppress`, `push_mode = None`, `periodic_full_sync = None`, `pull_on_query = false`. Verify a warning log is emitted. |

### 3.7 Dynamic Sync Urgency — Integration Tests

| ID | Test | Description |
|---|---|---|
| SYNC-14 | `SyncUrgency::Immediate` bypasses timer | `sync_urgency` returns `Immediate`. Verify the delta is pushed without waiting for the periodic interval. |
| SYNC-15 | `SyncUrgency::Delayed(d)` overrides interval | `sync_urgency` returns `Delayed(1s)` with periodic push at 10s. Verify delta arrives within ~1s. |
| SYNC-16 | `SyncUrgency::Suppress` skips broadcast | `sync_urgency` returns `Suppress`. Verify no sync message is sent for this mutation. |
| SYNC-17 | Suppressed deltas accumulate | Suppress 3 mutations, then trigger a non-suppressed mutation. Verify the final delta carries the cumulative change of all 4 mutations. |
| SYNC-18 | `SyncUrgency::Default` uses configured strategy | `sync_urgency` returns `Default`. Verify the behavior matches `StateConfig.sync_strategy`. |

---

## 4. Versioning and Ordering (Design §5)

### 4.1 Age and Incarnation Ordering — Unit Tests

| ID | Test | Description |
|---|---|---|
| VER-01 | Delta at `local_age + 1` applied normally | Local view at age 5, receive delta with generation (inc=1, age=6). Verify delta is applied and local age advances to 6. |
| VER-02 | Stale delta discarded | Local view at age 5, receive delta with generation (inc=1, age=4). Verify delta is discarded and local age remains 5. |
| VER-03 | Duplicate delta discarded | Local view at age 5, receive delta with generation (inc=1, age=5). Verify delta is discarded. |
| VER-04 | Gap triggers full snapshot request | Local view at age 5, receive delta with generation (inc=1, age=8). Verify a `RequestSnapshot` is sent and the delta is not applied. |
| VER-05 | Full snapshot at higher age replaces local view | Local view at age 5, receive full snapshot at age 10. Verify local view is replaced and age is now 10. |
| INC-01 | Higher incarnation accepted unconditionally | Peer cached at `Generation::new(100, 50)`. Receive snapshot at `Generation::new(200, 0)`. Verify entry replaced — new incarnation wins even though age is lower. |
| INC-02 | Lower incarnation rejected | Peer cached at `Generation::new(200, 5)`. Receive snapshot at `Generation::new(100, 99)`. Verify discarded — old lifetime. |
| INC-03 | Same incarnation follows normal age rules | Peer at `Generation::new(100, 5)`. Receive snapshot at `Generation::new(100, 3)`. Verify discarded. |
| INC-04 | Incarnation generated on startup without persistence | Start `StateShard` with `persistence: None`. Verify `Generation` incarnation is set to `current_unix_time_ms()` (via TestClock). |
| INC-05 | Incarnation preserved on startup with persistence | Persist state with `Generation::new(42, 0)`. Restart actor. Verify loaded state has matching `Generation`. |
| INC-06 | Incarnation changes on migration failure | Persist at `STORAGE_VERSION=1`, upgrade to V2, `migrate_state` returns `Err`. Verify actor starts with a new `Generation` incarnation (not the persisted one). |
| INC-07 | Generation propagated in FullSnapshot wire message | Mutate and broadcast. Verify the `SyncMessage::FullSnapshot` carries the correct `Generation`. |
| INC-08 | Generation propagated in DeltaUpdate wire message | Same as INC-07 for `SyncMessage::DeltaUpdate`. |
| INC-09 | Generation propagated in ChangeNotification | Mutate, verify the `ChangeNotification` sent to the ChangeFeedAggregator carries the correct `Generation`. |

### 4.2 Wire Version Compatibility — Integration Tests

| ID | Test | Description |
|---|---|---|
| VER-06 | Same wire_version: sync works | Both nodes at `WIRE_VERSION = 1`. Verify normal sync. |
| VER-07 | Receiver supports older wire_version | Node A sends at `WIRE_VERSION = 1`, Node B has deserialization for V1 and V2. Verify Node B applies the view. |
| VER-08 | Receiver does not support wire_version — KeepStale | Node A sends `WIRE_VERSION = 3`, Node B only supports 1 and 2, policy is `KeepStale`. Verify Node B keeps its last good view and logs a warning. |
| VER-09 | Receiver does not support wire_version — DropAndWait | Same as VER-08 but policy is `DropAndWait`. Verify Node B removes the peer's view from PublicViewMap. |
| VER-10 | Receiver does not support wire_version — RequestReserialization | Same as VER-08 but policy is `RequestReserialization`. Verify Node B sends a re-serialization request with its max supported version. |
| VER-11 | Re-serialization request served by owner | Node B requests re-serialization at version 2. Verify Node A re-serializes and sends a snapshot at version 2. |

### 4.3 Two-Phase Upgrade — Integration Tests

| ID | Test | Description |
|---|---|---|
| VER-12 | Phase 1: old nodes read old, new nodes read both | Mixed cluster: some nodes at phase 1 (read V1+V2, send V1), some at old code (read V1). Verify all nodes can sync. |
| VER-13 | Phase 2: all nodes read V2 after switch | All nodes upgraded to phase 1, then phase 2 switches to `WIRE_VERSION = 2`. Verify no sync failures. |
| VER-14 | Phase 2 rollback: revert to phase 1 code | After phase 2 deployment, roll back some nodes to phase 1 code. Verify those nodes still sync (they read V1+V2). |
| VER-15 | Phase 3: V1 deserialization removed | All nodes at phase 3 (V2 only). Verify sync works. Send a V1 message to a phase 3 node and verify it is rejected with `DeserializeError`. |

### 4.4 Storage Version — Unit Tests

| ID | Test | Description |
|---|---|---|
| VER-16 | Persist and load at same storage_version | Save state at `STORAGE_VERSION = 1`, load it back, verify data is intact. |
| VER-17 | Load from older storage_version | Save state at `STORAGE_VERSION = 1`, upgrade code to `STORAGE_VERSION = 2`, load the old data. Verify deserialization fills defaults for new fields. |
| VER-18 | Load from unknown storage_version fails gracefully | Attempt to load data saved at a future `STORAGE_VERSION`. Verify a clear error (not panic). |

---

## 5. Actor Architecture (Design §6)

### 5.0 Actor Runtime Abstraction (Design §6.0) — Unit Tests

These tests verify the abstract `ActorRuntime` trait contracts. They run in
the core crate using `TestRuntime` (a mock implementation of `ActorRuntime`
provided in `dstate::test_support`). Adapter conformance tests for ractor
and kameo are in §16.

| ID | Test | Description |
|---|---|---|
| RT-01 | `TestRuntime::spawn` returns a valid `ActorRef` | Spawn an actor via `TestRuntime`. Verify the returned `ActorRef` can receive messages via `send()`. |
| RT-02 | `ActorRef::send` delivers message | Spawn an actor, send a message via `send()`. Verify the actor's handler is invoked with the correct message. |
| RT-03 | `ActorRef::request` returns reply | Spawn an actor, call `request()` with a message. Verify the reply is received within the timeout. |
| RT-04 | `ActorRef::request` times out | Spawn an actor that never replies. Call `request()` with a short timeout. Verify `ActorRequestError::Timeout`. |
| RT-05 | `ProcessingGroup::join` and `get_members` | Join 3 actors to a group. Call `get_members()`. Verify 3 members returned. |
| RT-06 | `ProcessingGroup::leave` removes member | Join 2 actors, leave 1. Call `get_members()`. Verify 1 member returned. |
| RT-07 | `ProcessingGroup::broadcast` reaches all members | Join 3 actors to a group. Broadcast a message. Verify all 3 actors receive it. |
| RT-08 | `ProcessingGroup::broadcast` skips non-members | Join 2 of 3 actors. Broadcast. Verify only the 2 members receive the message. |
| RT-09 | `ClusterEvents::subscribe` receives `NodeJoined` | Subscribe to cluster events. Simulate a node join. Verify `ClusterEvent::NodeJoined(node_id)` is received. |
| RT-10 | `ClusterEvents::subscribe` receives `NodeLeft` | Subscribe to cluster events. Simulate a node leave. Verify `ClusterEvent::NodeLeft(node_id)` is received. |
| RT-11 | `send_interval` fires periodically | Schedule a recurring message at 100ms. Advance `TestClock` by 350ms. Verify the actor received ~3 messages. |
| RT-12 | `send_after` fires once after delay | Schedule a one-shot message after 200ms. Advance `TestClock` by 300ms. Verify the actor received exactly 1 message. |
| RT-13 | `TimerHandle::cancel` stops delivery | Schedule a recurring message. Cancel the timer. Advance time. Verify no further messages are delivered. |
| RT-14 | `ActorRef` is `Clone + Send + Sync` | Verify `ActorRef` can be cloned, sent across threads, and shared between threads (compile-time check). |

### 5.1 StateRegistry — Unit Tests

| ID | Test | Description |
|---|---|---|
| REG-01 | Register and lookup succeeds | Register a state type on `StateRegistry<TestRuntime>`, look it up, verify the returned `ActorRef` is valid. |
| REG-02 | Lookup unregistered state returns `StateNotRegistered` | Call `lookup::<UnregisteredState>()`, verify `RegistryError::StateNotRegistered`. |
| REG-03 | Lookup with wrong type returns `TypeMismatch` | Register state A under name "x", look up with a different type B that also has name "x". Verify `RegistryError::TypeMismatch`. |
| REG-04 | Same-type re-registration is idempotent | Register the same state type twice. Verify no error — second call is a no-op. |
| REG-04a | Different-type same-name returns `DuplicateName` | Register state A under name "x", register a different state B also with name "x". Verify `RegistryError::DuplicateName`. |
| REG-05 | `broadcast_node_joined` reaches all shards | Register 3 state types, broadcast `NodeJoined`. Verify all 3 `StateShard` actors receive the event. |
| REG-06 | `broadcast_node_left` reaches all shards | Same as REG-05 for `NodeLeft`. |
| REG-07 | Registry is generic over `ActorRuntime` | Instantiate `StateRegistry<TestRuntime>`. Verify register, lookup, and broadcast work identically to any concrete runtime. |

### 5.2 StateShard / ShardCore — Unit Tests

Tests in this section exercise `ShardCore<S, V>` directly (pure state machine,
no framework dependency) and the actor shell message routing via
`TestRuntime`.

| ID | Test | Description |
|---|---|---|
| SHARD-01 | Mutation increments age | Mutate once, verify age goes from 0 to 1. Mutate again, verify age is 2. |
| SHARD-02 | Mutation updates `modified_time` | Mutate, verify `modified_time` is updated via the injected Clock. |
| SHARD-03 | Mutation generates delta and forwards to SyncEngine | Mutate, verify the `SyncEngine` receives a delta message with correct `generation`. |
| SHARD-04 | Concurrent mutation requests are serialized | Send 100 mutation messages concurrently. Verify ages are sequential 1..100 with no gaps or duplicates. |
| SHARD-05 | `on_node_joined` adds entry to PublicViewMap | Send `NodeJoined(node_x)`, verify `PublicViewMap` contains an entry for `node_x`. |
| SHARD-06 | `on_node_left` removes entry from PublicViewMap | Send `NodeLeft(node_x)`, verify `PublicViewMap` no longer contains `node_x`. |
| SHARD-07 | `MarkStale` sets `pending_remote_generation` on peer entry | Send `MarkStale { source, generation }`. Verify the peer's `pending_remote_generation` is set. |
| SHARD-08 | `MarkStale` with stale generation is no-op | Peer at `Generation::new(200, 5)`. Send MarkStale with `Generation::new(100, 99)`. Verify entry unchanged. |
| SHARD-09 | Request coalescing: concurrent queries share pull | Send 3 `RefreshAndQuery` messages while one pull is in-flight. Verify only 1 pull request is sent (not 3). |
| SHARD-10 | `snapshot()` returns view map without freshness check | Call `snapshot()`. Verify the full view map is returned even if entries are stale. |

### 5.3 SyncEngine Actor — Unit Tests

| ID | Test | Description |
|---|---|---|
| SE-01 | Outbound delta stamped with correct wire_version | Send a delta, verify the `SyncMessage::DeltaUpdate.wire_version` equals `S::WIRE_VERSION`. |
| SE-02 | Outbound snapshot stamped with correct wire_version | Send a snapshot, verify `SyncMessage::FullSnapshot.wire_version` equals `S::WIRE_VERSION`. |
| SE-03 | `sync_urgency` called on every mutation | Mock `sync_urgency`, mutate 3 times, verify it was called 3 times with correct old/new views. |
| SE-04 | Periodic timer fires at configured interval | Configure 2s interval, verify sync messages are sent every ~2s (±tolerance). |
| SE-05 | Timer does not fire when no mutations pending | No mutations, wait 2 intervals. Verify no sync messages sent. |

---

## 6. Query API (Design §7)

### 6.1 Query — Integration Tests

| ID | Test | Description |
|---|---|---|
| QUERY-01 | Projection callback receives full view map | Register state, add 3 peers. Query with a projection that counts entries. Verify count is 4 (3 peers + self). |
| QUERY-02 | Stale entries refreshed before callback | One peer's view is stale (synced 10s ago via TestClock). Query with `max_staleness = 5s`. Verify a pull happens before the callback executes. |
| QUERY-03 | Fresh entries not re-fetched | All views are < 2s old. Query with `max_staleness = 5s`. Verify no pull requests. |
| QUERY-04 | Callback result is returned to caller | Projection returns a computed value. Verify the `query()` return matches. |
| QUERY-05 | Query with unreachable peer returns `StalePeer` | One peer is unreachable, freshness is tight. Verify `QueryError::StalePeer` with the correct `node_id`. |
| QUERY-06 | `pending_remote_age` triggers slow path | Peer has fresh `synced_at` but `pending_remote_age > age`. Verify query falls to slow path and pulls fresh data. |
| QUERY-07 | Concurrent pulls via `join_all` | 3 stale peers. Verify all 3 are pulled concurrently (not sequentially) by checking total latency ≈ max(single pull) not sum. |
| QUERY-08 | `snapshot()` returns stale data without pulling | Call `snapshot()` when peers are stale. Verify data returned immediately, no pull triggered. |

### 6.2 Simple Mutation — Unit Tests

| ID | Test | Description |
|---|---|---|
| MUT-01 | Mutation closure receives mutable state | Inside `mutate()`, modify a field. Verify the state is updated. |
| MUT-02 | Mutation updates local view map with full state | After `mutate()`, verify the local node's entry in the view map contains a clone of the full state (State = View). |
| MUT-03 | Mutation failure does not advance age | If the mutation closure panics (caught), verify age is unchanged. |
| MUT-04 | Mutation broadcasts `FullSnapshot` not `DeltaUpdate` | After `mutate()` on simple state, verify the `SyncEngine` receives an `OutboundSnapshot` message (not `OutboundDelta`). |
| MUT-05 | Receiver replaces state on `FullSnapshot` | Peer receives `FullSnapshot` with higher `(incarnation, age)`. Verify the peer's view map entry is replaced with the deserialized state. |
| MUT-06 | Receiver discards stale `FullSnapshot` | Peer receives `FullSnapshot` with same incarnation and age ≤ local. Verify the entry is not updated. |

### 6.3 Delta-Aware Mutation — Unit Tests

| ID | Test | Description |
|---|---|---|
| DMUT-01 | `mutate_with_delta` captures `StateDeltaChange` | Inside `mutate_with_delta()`, modify state and return a `StateDeltaChange`. Verify state is updated and the returned delta is captured. |
| DMUT-02 | `project_delta` is called (no `diff`) | Use `mutate_with_delta()`. Verify `project_delta()` is called with the returned `StateDeltaChange`. |
| DMUT-03 | Delta-aware mutation updates view map via `project_view` | After `mutate_with_delta()`, verify the local node's view map entry matches `project_view(new_state)`. |
| DMUT-04 | No old_view snapshot on delta-aware path | Instrument `project_view()`. Verify it is called once (for the new view) after `mutate_with_delta()`, not twice. |
| DMUT-05 | `save()` receives `Some(state_delta)` | Use `mutate_with_delta()` with persistence enabled. Verify `save()` is called with `state_delta: Some(...)`. |
| DMUT-06 | Simple `mutate()` passes `None` to save | Use `mutate()` with persistence enabled. Verify `save()` is called with `state_delta: None`. |

---

## 7. Persistence (Design §8)

### 7.1 Persistence Trait — Unit Tests

| ID | Test | Description |
|---|---|---|
| PERSIST-01 | `save()` called after each mutation | Enable persistence. Mutate 3 times. Verify `save()` was called 3 times with correct `StateObject` (age, storage_version). |
| PERSIST-02 | `load()` called on actor startup | Restart the `StateShard` actor. Verify `load()` is called and state is restored. |
| PERSIST-03 | State survives actor restart | Persist state at age 5. Restart actor. Verify loaded state has age 5 and correct values. |
| PERSIST-04 | No persistence when disabled | Register with `persistence: None`. Mutate. Verify no `save()` calls. |
| PERSIST-05 | `save()` failure returns `MutationError::PersistenceFailed` | Mock `save()` to return error. Verify `mutate()` returns `MutationError::PersistenceFailed`. |
| PERSIST-06 | `load()` returns None on fresh node | No prior state file. Verify `load()` returns `None` and state starts at age 0. |

### 7.2 Persist-Before-Publish — Unit Tests

| ID | Test | Description |
|---|---|---|
| PERSIST-07 | Save failure prevents age advance | Mutate to age 3 (succeeds). Next `save()` fails. Verify age is still 3, not 4. State unchanged because mutation was applied to a clone. |
| PERSIST-08 | Save failure leaves view_map unchanged | Mutate (save fails). Verify the PublicViewMap still contains the pre-mutation view for the local node (clone-and-swap never happened). |
| PERSIST-09 | Save failure does not broadcast | Mutate (save fails). Verify `SyncEngine` did **not** receive an `OutboundSnapshot` or `OutboundDelta` message. |
| PERSIST-10 | Successful save then publish ordering | Mutate (save succeeds). Verify `save()` is called **before** the view map is updated and **before** `SyncEngine` receives the outbound message. |

### 7.3 Startup Load — Unit Tests

| ID | Test | Description |
|---|---|---|
| PERSIST-11 | `load()` error falls back to empty state | Mock `load()` to return `Err(StorageUnavailable)`. Verify actor starts with age 0 and default state. |
| PERSIST-12 | `load()` error is logged | Mock `load()` to fail. Verify a `tracing::error!` event is emitted with state name and error details. |
| PERSIST-13 | Loaded state at same storage_version used as-is | Save at `STORAGE_VERSION = 1`, load at `STORAGE_VERSION = 1`. Verify state, age, and timestamps are restored exactly. |
| PERSIST-14 | Loaded state at different storage_version triggers migration | Save at `STORAGE_VERSION = 1`, upgrade code to `STORAGE_VERSION = 2`. Verify `migrate_state()` is called with the old value and version. |
| PERSIST-15 | Migration failure falls back to empty state | `migrate_state()` returns `Err`. Verify actor starts with age 0 and default state. Log is emitted. |
| PERSIST-16 | Successful migration restores age and timestamps | `migrate_state()` succeeds. Verify the restored state keeps the original age, `created_time`, `modified_time`, and has the new `STORAGE_VERSION`. |

### 7.4 Startup Rejoin — Integration Tests

| ID | Test | Description |
|---|---|---|
| PERSIST-17 | Restart node re-announces to cluster | Node A persists at age 5, crashes, restarts. Verify peer nodes receive a snapshot with age 5 from Node A. |
| PERSIST-18 | Restart node rebuilds peer views | Node A restarts. Verify Node A's PublicViewMap is populated with current views from all live peers via snapshot exchange. |
| PERSIST-19 | Crash before save loses in-flight mutation | Kill Node A mid-mutation (before `save()`). Restart. Verify loaded state is at the previous age (mutation lost). |
| PERSIST-20 | Crash after save but before broadcast | Kill Node A after `save()` but before delta broadcast. Restart. Verify Node A has the saved state and peers receive the update via rejoin snapshot exchange. |

---

## 8. Cluster Lifecycle Events (Design §9)

### 8.1 Node Join — Integration Tests

| ID | Test | Description |
|---|---|---|
| LIFE-01 | New node receives snapshots from all existing nodes | Start 3-node cluster. Add Node D. Verify Node D's PublicViewMap contains views for A, B, C. |
| LIFE-02 | Existing nodes receive snapshot from new node | Add Node D. Verify Nodes A, B, C each have Node D's view in their PublicViewMap. |
| LIFE-03 | Join with multiple state types | Register 2 state types. Add new node. Verify both state types are synced for the new node. |

### 8.2 Node Leave — Integration Tests

| ID | Test | Description |
|---|---|---|
| LIFE-04 | Departed node removed from PublicViewMap | Node C leaves. Verify Nodes A and B no longer have Node C in their PublicViewMap. |
| LIFE-05 | In-flight sync from departed node discarded | Node C sends a delta and then leaves immediately. Verify the delta is discarded (not applied) after `NodeLeft`. |
| LIFE-06 | Leave with multiple state types | Register 2 state types. Node C leaves. Verify removed from both PublicViewMaps. |

### 8.3 Crash Restart — Integration Tests

| ID | Test | Description |
|---|---|---|
| LIFE-07 | Crash restart without persistence: new incarnation accepted | Node A crashes (no persistence). Restart. Verify peers accept Node A's `age=0` because `incarnation` is higher. |
| LIFE-08 | Crash restart with persistence: same incarnation, state restored | Node A persists at age 5, crashes, restarts. Verify peers see same incarnation and age 5. |
| LIFE-09 | Fast restart before `NodeLeft` detected | Node A crashes and restarts immediately. Verify peers accept the new incarnation even if `NodeLeft` hasn't fired yet. |
| LIFE-10 | Late `NodeLeft` after rejoin doesn't break state | Node A crashes, restarts, re-syncs. Then late `NodeLeft` arrives. Verify entry is removed but immediately repopulated by the live node's next push. |

### 8.4 Network Partition — Integration Tests

| ID | Test | Description |
|---|---|---|
| LIFE-11 | Partitioned peer views become stale | Partition Node C from A and B. Advance TestClock. Query on Node A with tight freshness. Verify `StalePeer` for Node C. |
| LIFE-12 | Views recover after partition heals | Heal the partition. Verify Node A's view of Node C is refreshed on next sync cycle. |
| LIFE-13 | Query with loose freshness succeeds during partition | Same partition as LIFE-11. Query with `max_staleness = 60s`. Verify the query succeeds with the last known view. |

---

## 9. Registration Flow (Design §10)

### 9.1 Registration — Integration Tests

| ID | Test | Description |
|---|---|---|
| FLOW-01 | Register spawns StateShard and SyncEngine | Register a state type via `StateRegistry<R>`. Verify both actors are alive and responding to messages via the runtime's `ActorRef::request()`. |
| FLOW-02 | `start()` initiates cluster sync | Register, then call `start()`. Verify sync messages are exchanged with peers. |
| FLOW-03 | Register with composable SyncStrategy | Register with `active_push()`, `feed_lazy_pull()`, `feed_with_periodic_sync(15min)`, `periodic_only(5min)`. Verify each creates a working state shard. |
| FLOW-04 | Register with VersionMismatchPolicy variants | Register with each `VersionMismatchPolicy`. Verify the policy is stored and used on version mismatch. |

---

## 10. Error Handling (Design §11)

### 10.1 Error Propagation — Unit Tests

| ID | Test | Description |
|---|---|---|
| ERR-01 | `RegistryError::StateNotRegistered` propagates through `QueryError` | Call `lookup` for unregistered state via query path. Verify `QueryError::Registry(StateNotRegistered)`. |
| ERR-02 | `RegistryError::TypeMismatch` propagates through `QueryError` | Trigger type mismatch via query path. Verify `QueryError::Registry(TypeMismatch)`. |
| ERR-03 | `MutationError::PersistenceFailed` on save failure | Persistence backend returns error. Verify `MutationError::PersistenceFailed`. |
| ERR-04 | Actor crash returns `ActorSendError` / `ActorRequestError` | Kill the `StateShard` actor. Verify subsequent query returns `ActorRequestError` and mutate returns `ActorSendError`. |

### 10.2 Retry Pattern — Integration Tests

| ID | Test | Description |
|---|---|---|
| ERR-05 | Stale-state retry succeeds on second attempt | First query returns a stale view leading to a downstream failure. Retry with tighter freshness. Verify second attempt succeeds. |
| ERR-06 | Backoff between retries | Verify retry loop waits progressively longer between attempts. |

---

## 11. Wire Protocol (Design §12)

### 11.1 Message Serialization — Unit Tests

| ID | Test | Description |
|---|---|---|
| WIRE-01 | `FullSnapshot` round-trip | Serialize and deserialize a `FullSnapshot` message. Verify all fields preserved including `incarnation`. |
| WIRE-02 | `DeltaUpdate` round-trip | Serialize and deserialize a `DeltaUpdate` message. Verify all fields preserved including `incarnation`. |
| WIRE-03 | `BatchedChangeFeed` round-trip | Serialize and deserialize a `BatchedChangeFeed` message. Verify all `ChangeNotification` entries preserved including `incarnation`. |
| WIRE-04 | `RequestSnapshot` round-trip | Serialize and deserialize a `RequestSnapshot` message. Verify all fields preserved. |

### 11.2 Message Handling — Integration Tests

| ID | Test | Description |
|---|---|---|
| WIRE-05 | `FullSnapshot` updates PublicViewMap | Receive a `FullSnapshot` from a peer. Verify the peer's entry in PublicViewMap is updated. |
| WIRE-06 | `DeltaUpdate` advances peer view | Receive a `DeltaUpdate` at the expected age. Verify the peer's view is advanced. |
| WIRE-07 | `BatchedChangeFeed` routes through aggregator and marks peer stale | Receive a `BatchedChangeFeed` with a higher `(incarnation, age)`. Verify the peer is marked as needing a pull via `MarkStale`. |
| WIRE-08 | `RequestSnapshot` triggers snapshot response | Receive a `RequestSnapshot`. Verify a `FullSnapshot` is sent back. |
| WIRE-09 | Unknown `state_name` in message is ignored | Receive a sync message for an unregistered state. Verify it is logged and discarded (no panic). |

---

## 12. Diagnostics and Observability (Design §13)

### 12.1 PeerSyncStatus — Unit Tests

| ID | Test | Description |
|---|---|---|
| DIAG-01 | `sync_status()` returns entry for each peer | 3-node cluster. Verify `sync_status()` returns 3 entries (including self). |
| DIAG-02 | `consecutive_failures` increments on failure | Fail 3 sync attempts to a peer. Verify `consecutive_failures == 3`. |
| DIAG-03 | `consecutive_failures` resets on success | After failures, succeed once. Verify `consecutive_failures == 0`. |
| DIAG-04 | `last_sync_latency_ms` tracks round-trip | Complete a sync. Verify `last_sync_latency_ms` is populated and reasonable. |
| DIAG-05 | `last_error` captures failure message | Fail a sync with a known error. Verify `last_error` contains the error message. |
| DIAG-06 | `gap_count` increments on age gap | Trigger an age gap. Verify `gap_count` increments. |

### 12.2 StateSyncMetrics — Unit Tests

| ID | Test | Description |
|---|---|---|
| DIAG-07 | `total_mutations` counts mutations | Mutate 5 times. Verify `total_mutations == 5`. |
| DIAG-08 | `deltas_sent` counts outbound deltas | Push 3 deltas. Verify `deltas_sent == 3`. |
| DIAG-09 | `deltas_suppressed` counts suppressed deltas | Suppress 2 mutations. Verify `deltas_suppressed == 2`. |
| DIAG-10 | `deltas_immediate` counts immediate pushes | Trigger 2 immediate urgency mutations. Verify `deltas_immediate == 2`. |
| DIAG-11 | `age_gaps_detected` counts gaps | Trigger 1 age gap. Verify `age_gaps_detected == 1`. |
| DIAG-12 | `stale_deltas_discarded` counts discards | Send 2 stale deltas. Verify `stale_deltas_discarded == 2`. |
| DIAG-13 | `sync_failures` counts all failure types | Fail 3 syncs (network, serde, timeout). Verify `sync_failures == 3`. |

### 12.3 stale_peers() — Unit Tests

| ID | Test | Description |
|---|---|---|
| DIAG-14 | Returns only stale peers | 3 peers: 2 fresh, 1 stale. Call `stale_peers(5s)`. Verify only the stale peer is returned. |
| DIAG-15 | Returns empty when all fresh | All peers synced < 2s ago. Call `stale_peers(5s)`. Verify empty result. |

### 12.4 Health Check — Integration Tests

| ID | Test | Description |
|---|---|---|
| DIAG-16 | Healthy: all peers synced | All peers within freshness window. Verify `HealthStatus::Healthy`. |
| DIAG-17 | Degraded: some peers stale | 1 of 3 peers stale. Verify `HealthStatus::Degraded`. |
| DIAG-18 | Unhealthy: majority stale | 2 of 3 peers stale. Verify `HealthStatus::Unhealthy`. |
| DIAG-19 | Multiple state types reported | Register 2 state types, one healthy, one degraded. Verify `SystemHealth.states` contains both with correct statuses. |

### 12.5 Structured Logging — Unit Tests

| ID | Test | Description |
|---|---|---|
| DIAG-20 | Sync success emits INFO log | Complete a sync. Verify a `tracing` INFO event with `state_name`, `peer`, `age`, `latency_ms`. |
| DIAG-21 | Sync failure emits WARN log | Fail a sync. Verify a WARN event with `state_name`, `peer`, `error`, `consecutive_failures`. |
| DIAG-22 | Delta suppression emits DEBUG log | Suppress a delta. Verify a DEBUG event with `urgency=Suppress`, `suppressed_count`. |

---

## 13. Stress and Chaos Tests

### 13.1 High Throughput

| ID | Test | Description |
|---|---|---|
| STRESS-01 | 1000 mutations/sec sustained | Single node mutating at 1000/s. Verify no message loss, ages are sequential, SyncEngine keeps up. |
| STRESS-02 | 10-node cluster full mesh sync | 10 nodes, each mutating. Verify all nodes converge within expected freshness bounds. |
| STRESS-03 | Large view payload | State with a 1MB public view. Verify sync completes without timeout or memory issues. |

### 13.2 Fault Injection

| ID | Test | Description |
|---|---|---|
| CHAOS-01 | Random network delays (10ms–2s) | Inject random delays on sync messages. Verify system converges and freshness queries eventually succeed. |
| CHAOS-02 | Packet loss (10% drop rate) | Drop 10% of sync messages randomly. Verify age gaps are detected, snapshots requested, and views recover. |
| CHAOS-03 | Node crash and rejoin | Kill a node, wait, restart it. Verify it loads persisted state and re-syncs with the cluster. |
| CHAOS-04 | Rapid join/leave cycles | A node joins and leaves 10 times in 30 seconds. Verify PublicViewMaps on other nodes are consistent (no ghost entries). |
| CHAOS-05 | Split-brain partition | Partition a 5-node cluster into 2+3. Verify each partition operates independently. Heal partition and verify convergence. |
| CHAOS-06 | Clock skew between nodes | Inject 5s clock drift on one node. Verify freshness checks use monotonic clocks or handle skew gracefully. |

### 13.3 Version Upgrade Simulation

| ID | Test | Description |
|---|---|---|
| UPGRADE-01 | Rolling upgrade phase 1 | 5-node cluster. Upgrade nodes one at a time to phase 1 code (read V1+V2, send V1). Verify no sync failures at any point during the rollout. |
| UPGRADE-02 | Rolling upgrade phase 2 | All nodes at phase 1. Upgrade one at a time to phase 2 (send V2). Verify no sync failures. |
| UPGRADE-03 | Partial rollback during phase 2 | 3 of 5 nodes upgraded to phase 2. Roll back 1 node to phase 1. Verify it still syncs with V2 senders. |
| UPGRADE-04 | Mixed version steady state | Hold a mixed V1/V2 cluster for 5 minutes under load. Verify no metric anomalies (gaps, failures, dropped views). |

---

## 14. NodeResource Example (Design §14)

### 14.1 End-to-End — Integration Tests

| ID | Test | Description |
|---|---|---|
| EXAMPLE-01 | Resource monitor updates local shard | Start the background monitor. Wait 5s. Verify `cpu_usage_pct`, `memory_used_bytes`, `disk_used_bytes` are populated (non-zero). |
| EXAMPLE-02 | Private fields not visible on peers | Start 2-node cluster with NodeResource. Verify peer view does not contain `sample_count` or `cpu_accumulator`. |
| EXAMPLE-03 | `find_lowest_memory_node` returns correct node | 3-node cluster with known memory values. Verify the function returns the node with the lowest `memory_used_bytes`. |
| EXAMPLE-04 | Dynamic urgency: large change triggers immediate push | Simulate a 25% memory spike on Node A. Verify peers receive the update immediately (not after periodic interval). |
| EXAMPLE-05 | Dynamic urgency: small change suppressed | Simulate a 0.5% CPU change. Verify no sync message is sent for that mutation. |
| EXAMPLE-06 | Suppressed changes included in next push | Suppress 3 small changes, then trigger a large change. Verify the pushed delta includes all 4 mutations' cumulative effect. |

---

## 15. Testability Infrastructure (Design §15)

### 15.1 Clock Abstraction — Unit Tests

| ID | Test | Description |
|---|---|---|
| TEST-01 | TestClock starts frozen | Create a `TestClock`. Verify `now()` and `unix_ms()` return the initial values. |
| TEST-02 | TestClock advances on demand | Call `advance(5s)`. Verify `now().elapsed()` reflects 5s passed. `unix_ms()` advanced by 5000. |
| TEST-03 | StateShard uses injected Clock | Create StateShard with TestClock. Mutate. Verify `modified_time` matches TestClock's `unix_ms()`, not real wall clock. |
| TEST-04 | Freshness check uses Clock, not `Instant::now()` | Freeze TestClock. Sync a view. Advance TestClock by 10s. Verify `synced_at.elapsed()` reports ~10s. |

### 15.2 ShardCore — Unit Tests

| ID | Test | Description |
|---|---|---|
| TEST-05 | `should_accept` implements full ordering table | Test all 5 rows of the §5.1 `(incarnation, age)` ordering table via direct method calls on `ShardCore` (no actor). |
| TEST-06 | `stale_peers` returns correct peers | Create `ShardCore` with 3 peers at different `synced_at` ages. Call `stale_peers(5s)`. Verify only the stale one is returned. |
| TEST-07 | `apply_mutation` advances age and returns new state | Call `apply_mutation` directly on `ShardCore`. Verify age increments and new state is returned. |

### 15.3 Test Doubles — Unit Tests

| ID | Test | Description |
|---|---|---|
| TEST-08 | `InMemoryPersistence` save/load round-trip | Save a `StateObject`, load it back. Verify exact match. |
| TEST-09 | `FailingPersistence` always returns error | Call `save()` and `load()`. Verify both return `PersistError::StorageUnavailable`. |
| TEST-10 | `InMemoryPersistence` load returns None initially | Call `load()` before any `save()`. Verify `Ok(None)`. |

### 15.4 TestCluster — Integration Tests

`TestCluster` uses `TestRuntime` (the mock `ActorRuntime` from
`dstate::test_support`) with in-process transport. No real cluster or
framework dependency needed.

| ID | Test | Description |
|---|---|---|
| TEST-11 | `TestCluster::new(N)` creates N nodes | Create cluster with 3 nodes using `TestRuntime`. Verify 3 distinct node IDs, each with a running StateShard. |
| TEST-12 | `settle()` waits until all messages delivered | Mutate on node 0, call `settle()`. Verify all peers have the updated view. |
| TEST-13 | `crash_and_restart()` simulates crash restart | Create cluster, mutate on node 0, crash_and_restart(0). Verify node comes back with new incarnation and peers accept it. |
| TEST-14 | `advance_time()` controls shared clock | Create cluster, advance time by 30s. Verify freshness checks see 30s elapsed (no real delay). |

---

## 16. Multi-Crate Architecture Tests (Design §6.0.3)

### 16.0 TestRuntime — Unit Tests

`TestRuntime` is the mock `ActorRuntime` that ships with the core crate
(`dstate::test_support`). These tests verify the mock itself is correct so
that all core-crate tests built on it are trustworthy.

| ID | Test | Description |
|---|---|---|
| MOCK-01 | `TestRuntime` spawns in-process actors | Spawn 3 actors. Verify each receives its own messages independently. |
| MOCK-02 | `TestRuntime` processing group isolates groups | Join actors to different groups. Broadcast to group A. Verify group B members receive nothing. |
| MOCK-03 | `TestRuntime` cluster events are controllable | Inject `NodeJoined` and `NodeLeft` via `TestRuntime::simulate_node_join()` / `simulate_node_leave()`. Verify subscriber callbacks fire. |
| MOCK-04 | `TestRuntime` timers respect `TestClock` | Schedule `send_interval(100ms)`. Advance `TestClock` by 250ms. Verify actor received ~2 messages (timer linked to TestClock, not wall time). |
| MOCK-05 | `TestRuntime` in-process transport delivers between nodes | Create 2 `TestNode` instances sharing a `TestRuntime`. Send from node 0's actor to node 1's actor. Verify delivery. |

### 16.1 Adapter Conformance Tests — Integration Tests

Each adapter crate must pass these conformance tests to verify it correctly
implements the abstract `ActorRuntime` traits. The test bodies are identical
across adapters — only the `ActorRuntime` type parameter changes.

| ID | Test | Description |
|---|---|---|
| ADAPT-01 | `spawn` + `send` + `request` round-trip | Spawn an actor via the adapter's runtime. Send a message, request a reply. Verify both succeed. |
| ADAPT-02 | Processing group join/leave/broadcast | Join 3 actors to a group, broadcast a message, verify all 3 receive it. Leave 1, broadcast again, verify only 2 receive it. |
| ADAPT-03 | Cluster events fire on join/leave | Start a 2-node cluster using the adapter's real cluster mechanism. Verify `NodeJoined` events are received. Stop one node, verify `NodeLeft`. |
| ADAPT-04 | `send_interval` fires periodically | Schedule a recurring message. Wait 3 intervals. Verify the actor received ≥3 messages. |
| ADAPT-05 | `send_after` fires once | Schedule a one-shot message after 200ms. Wait 500ms. Verify exactly 1 message received. |
| ADAPT-06 | `TimerHandle::cancel` stops delivery | Schedule a recurring message, cancel it after 2 fires. Wait. Verify no further messages. |
| ADAPT-07 | Full state sync via adapter | Register a state type, mutate on node 0, verify node 1 receives the update via the adapter's real transport. |
| ADAPT-08 | Crash restart via adapter | Crash a node, restart it. Verify peers accept the new incarnation and state re-syncs. |

### 16.2 Ractor Adapter (`dstate-ractor`) — Integration Tests

| ID | Test | Description |
|---|---|---|
| RACTOR-01 | `RactorRuntime` implements `ActorRuntime` | Verify `RactorRuntime` compiles as `R: ActorRuntime` and passes ADAPT-01 through ADAPT-08. |
| RACTOR-02 | `send()` maps to `ractor::ActorRef::cast()` | Send a message. Verify the underlying ractor `cast()` is invoked (no reply expected). |
| RACTOR-03 | `request()` maps to `ractor::ActorRef::call()` | Request a reply. Verify the underlying ractor `call()` with `RpcReplyPort` is invoked. |
| RACTOR-04 | Processing group uses `ractor::pg` | Join and broadcast via `ProcessingGroup`. Verify `ractor::pg::join()` and `ractor::pg::broadcast()` are called. |
| RACTOR-05 | Cluster events via `ractor_cluster` | Start a 2-node ractor cluster. Verify `ClusterEvents` receives `NodeJoined` from `ractor_cluster`. |
| RACTOR-06 | Timer maps to `ractor::Actor::send_interval` | Schedule a timer. Verify the ractor timer mechanism is used and messages arrive. |
| RACTOR-07 | Re-exports core crate types | Verify `dstate_ractor::StateConfig`, `dstate_ractor::DistributedState`, etc. are accessible (re-exported from `dstate`). |

### 16.3 Kameo Adapter (`dstate-kameo`) — Integration Tests

| ID | Test | Description |
|---|---|---|
| KAMEO-01 | `KameoRuntime` implements `ActorRuntime` | Verify `KameoRuntime` compiles as `R: ActorRuntime` and passes ADAPT-01 through ADAPT-08. |
| KAMEO-02 | `send()` maps to `kameo::ActorRef::tell()` | Send a message. Verify the underlying kameo `tell()` is invoked. |
| KAMEO-03 | `request()` maps to `kameo::ActorRef::ask()` | Request a reply with timeout. Verify kameo `ask()` is invoked and reply is returned. |
| KAMEO-04 | Processing group via `PubSub<M>` actor | Join 3 actors to a group backed by `PubSub<M>`. Broadcast. Verify all 3 receive the message via `PubSub::Publish`. |
| KAMEO-05 | `PubSub` group cross-node via gossipsub | Start 2-node kameo swarm. Join actors on different nodes to the same group. Broadcast from node 0. Verify node 1's actor receives it. |
| KAMEO-06 | Cluster events via `ActorSwarm` | Bootstrap a 2-node `ActorSwarm`. Verify `ClusterEvents` receives peer discovery events. Stop one node, verify leave event. |
| KAMEO-07 | Timer via tokio task + `tell()` | Schedule `send_interval` via `KameoRuntime`. Verify a tokio task is spawned that calls `tell()` on the target actor at each interval. |
| KAMEO-08 | Timer cancel via `JoinHandle::abort()` | Schedule a timer, cancel it. Verify the tokio task is aborted and no further messages arrive. |
| KAMEO-09 | `#[derive(Actor)]` wrapper compiles | Verify `StateShardActor<S>` uses kameo's `#[derive(Actor)]` and implements `Message<StateShardMsg<S>>`. |
| KAMEO-10 | Remote actor messaging via `RemoteActorRef` | Spawn an actor on node 0, look it up from node 1 via `ActorSwarm`. Send a message via `RemoteActorRef`. Verify delivery. |
| KAMEO-11 | Re-exports core crate types | Verify `dstate_kameo::StateConfig`, `dstate_kameo::DistributedState`, etc. are accessible (re-exported from `dstate`). |
