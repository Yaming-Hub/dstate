//! Public API for driving the dstate replication protocol.
//!
//! [`DistributedStateEngine`] wraps all internal core modules (ShardCore,
//! SyncLogic, ChangeFeedLogic, VersioningLogic, LifecycleLogic,
//! DiagnosticsLogic) into a single, testable unit. Each method returns a
//! `Vec<EngineAction>` describing outbound effects — the engine itself
//! performs no I/O.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::change_feed_logic::ChangeFeedLogic;
use crate::core::diagnostics::DiagnosticsLogic;
use crate::core::lifecycle_logic::LifecycleLogic;
use crate::core::shard_core::{self, AcceptResult, DeltaAcceptResult, InboundSnapshot, ShardCore};
use crate::core::sync_logic::{SyncAction, SyncLogic};
use crate::core::versioning_logic::{InboundVersionResult, VersionMismatchAction, VersioningLogic};
use crate::traits::clock::Clock;
use crate::types::config::StateConfig;
use crate::types::envelope::{StateObject, StateViewObject};
use crate::types::errors::DeserializeError;
use crate::types::node::{Generation, NodeId};
use crate::types::sync_message::{BatchedChangeFeed, SyncMessage};

// ── Public types ────────────────────────────────────────────────

/// Wire-level envelope for all messages exchanged between nodes.
///
/// Wraps both synchronization messages and change feed batches, enabling
/// a single transport layer to handle all inter-node communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireMessage {
    /// A synchronization message (snapshot, delta, or snapshot request).
    Sync(SyncMessage),
    /// A batched change feed notification.
    Feed(BatchedChangeFeed),
}

/// Outbound effect produced by engine operations.
///
/// The hosting framework (actor runtime or test harness) is responsible
/// for executing these actions — the engine itself performs no I/O.
#[derive(Debug, Clone)]
pub enum EngineAction {
    /// Broadcast a sync message to all known peers.
    BroadcastSync(SyncMessage),
    /// Send a sync message to a specific peer.
    SendSync {
        target: NodeId,
        message: SyncMessage,
    },
    /// Schedule a delayed broadcast after the given duration.
    ScheduleDelayed {
        delay: Duration,
        message: SyncMessage,
    },
}

/// Result of a mutation operation.
#[derive(Debug)]
pub struct MutateResult<V: Clone> {
    /// The generation after mutation.
    pub generation: Generation,
    /// The projected view after mutation.
    pub view: V,
    /// Outbound actions to execute.
    pub actions: Vec<EngineAction>,
}

/// Result of a delta-aware mutation operation.
#[derive(Debug)]
pub struct DeltaMutateResult<V: Clone, VD: Clone> {
    /// The generation after mutation.
    pub generation: Generation,
    /// The projected view after mutation.
    pub view: V,
    /// The projected view delta.
    pub view_delta: VD,
    /// Outbound actions to execute.
    pub actions: Vec<EngineAction>,
}

/// Result of a query operation.
#[derive(Debug)]
pub enum EngineQueryResult<R> {
    /// All peer views are fresh; result is up-to-date.
    Fresh(R),
    /// Some peer views are stale; the engine has issued
    /// `RequestSnapshot` actions for stale peers.
    Stale {
        result: R,
        stale_peers: Vec<NodeId>,
    },
}

/// Health status of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Sync metrics snapshot.
#[derive(Debug, Clone, Default)]
pub struct SyncMetrics {
    pub total_mutations: u64,
    pub deltas_sent: u64,
    pub deltas_received: u64,
    pub snapshots_sent: u64,
    pub snapshots_received: u64,
    pub deltas_suppressed: u64,
    pub deltas_immediate: u64,
    pub sync_failures: u64,
    pub age_gaps_detected: u64,
    pub stale_deltas_discarded: u64,
}

/// Health assessment of the engine.
#[derive(Debug, Clone)]
pub struct EngineHealth {
    pub state_name: String,
    pub healthy_peers: u32,
    pub stale_peers: u32,
    pub failing_peers: u32,
    pub status: HealthStatus,
}

// ── Engine ──────────────────────────────────────────────────────

/// Function type for serializing a view to bytes.
type SerializeViewFn<V> = Box<dyn Fn(&V) -> Vec<u8> + Send + Sync>;

/// Function type for deserializing a view from bytes.
type DeserializeViewFn<V> =
    Box<dyn Fn(&[u8], u32) -> Result<V, DeserializeError> + Send + Sync>;

/// Function type for applying an inbound delta: `(bytes, wire_version, current_view) → new_view`.
type InboundDeltaApplierFn<V> =
    Box<dyn Fn(&[u8], u32, &V) -> Result<V, DeserializeError> + Send + Sync>;

/// Public API for driving the dstate replication protocol.
///
/// Wraps all internal core modules into a single, testable unit.
/// Each method returns a `Vec<EngineAction>` describing outbound effects —
/// the engine itself performs no I/O.
///
/// # Type Parameters
///
/// - `S`: The full internal state type (owned by this node).
/// - `V`: The public view type (projected from state, shared with peers).
pub struct DistributedStateEngine<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    state_name: String,
    wire_version: u32,
    shard: ShardCore<S, V>,
    sync_logic: SyncLogic,
    change_feed: ChangeFeedLogic,
    versioning: VersioningLogic,
    lifecycle: LifecycleLogic,
    diagnostics: DiagnosticsLogic,
    serialize_view: SerializeViewFn<V>,
    deserialize_view: DeserializeViewFn<V>,
    /// Optional delta applier for inbound `DeltaUpdate` messages.
    /// When `None`, inbound deltas trigger a full snapshot request.
    inbound_delta_applier: Option<InboundDeltaApplierFn<V>>,
}

impl<S, V> DistributedStateEngine<S, V>
where
    S: Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + Debug + 'static,
{
    // ── Construction ────────────────────────────────────────────

    /// Create a new engine for a single distributed state.
    ///
    /// # Parameters
    ///
    /// - `state_name`: Globally unique name for this state type.
    /// - `node_id`: This node's identity in the cluster.
    /// - `initial_value`: The initial state value.
    /// - `project_initial`: Projects the initial state into its initial view.
    /// - `config`: Sync strategy and policy configuration.
    /// - `wire_version`: Current wire protocol version.
    /// - `clock`: Time source (use `TestClock` for deterministic tests).
    /// - `serialize_view`: Serializes a view to bytes for wire transmission.
    /// - `deserialize_view`: Deserializes a view from bytes.
    /// - `inbound_delta_applier`: Optional function that applies an inbound
    ///   delta (bytes, wire_version, current_view) → new_view. When `None`,
    ///   inbound `DeltaUpdate` messages trigger a full snapshot request.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state_name: impl Into<String>,
        node_id: NodeId,
        initial_value: S,
        project_initial: impl FnOnce(&S) -> V,
        config: StateConfig,
        wire_version: u32,
        clock: Arc<dyn Clock>,
        serialize_view: impl Fn(&V) -> Vec<u8> + Send + Sync + 'static,
        deserialize_view: impl Fn(&[u8], u32) -> Result<V, DeserializeError> + Send + Sync + 'static,
        inbound_delta_applier: Option<InboundDeltaApplierFn<V>>,
    ) -> Self {
        let state_name = state_name.into();
        let supports_delta = inbound_delta_applier.is_some();

        let state = shard_core::new_state_object(initial_value, clock.unix_ms() as u64, &*clock);
        let initial_view = project_initial(&state.value);

        let shard = ShardCore::new(node_id, state, initial_view, clock.clone());

        let sync_logic = SyncLogic::new(
            state_name.clone(),
            config.sync_strategy.clone(),
            supports_delta,
        );
        let change_feed = ChangeFeedLogic::new(node_id);
        let versioning = VersioningLogic::new(
            state_name.clone(),
            config.version_mismatch_policy,
            wire_version,
            1, // storage_version (not used in engine path)
        );
        let lifecycle = LifecycleLogic::new();
        let diagnostics = DiagnosticsLogic::new(state_name.clone());

        Self {
            state_name,
            wire_version,
            shard,
            sync_logic,
            change_feed,
            versioning,
            lifecycle,
            diagnostics,
            serialize_view: Box::new(serialize_view),
            deserialize_view: Box::new(deserialize_view),
            inbound_delta_applier,
        }
    }

    // ── Mutation ────────────────────────────────────────────────

    /// Apply a mutation to the state and produce outbound sync actions.
    ///
    /// This is the simple (non-delta) path: the entire projected view is
    /// broadcast as a `FullSnapshot`. For delta-aware mutations, use
    /// [`mutate_with_delta`](Self::mutate_with_delta).
    pub fn mutate(
        &mut self,
        mutate_fn: impl FnOnce(&mut S),
        project_fn: impl FnOnce(&S) -> V,
        urgency: crate::SyncUrgency,
    ) -> MutateResult<V> {
        let outcome = self.shard.apply_mutation(mutate_fn, project_fn);
        self.diagnostics.record_mutation();

        let sync_action = self.sync_logic.resolve_action(urgency);
        let actions = self.sync_action_to_engine_actions_snapshot(&sync_action, &outcome.view);

        MutateResult {
            generation: outcome.generation,
            view: outcome.view,
            actions,
        }
    }

    /// Apply a delta-aware mutation and produce outbound sync actions.
    ///
    /// The `serialize_delta` closure is called only when the sync logic
    /// decides to broadcast a delta (not a full snapshot).
    pub fn mutate_with_delta<VD, DC>(
        &mut self,
        mutate_fn: impl FnOnce(&mut S) -> DC,
        project_view_fn: impl FnOnce(&S) -> V,
        project_delta_fn: impl FnOnce(&DC) -> VD,
        urgency: crate::SyncUrgency,
        serialize_delta: impl FnOnce(&VD) -> Vec<u8>,
    ) -> DeltaMutateResult<V, VD>
    where
        VD: Clone + Debug,
    {
        let outcome =
            self.shard
                .apply_mutation_with_delta(mutate_fn, project_view_fn, project_delta_fn);
        self.diagnostics.record_mutation();

        let sync_action = self.sync_logic.resolve_action(urgency);
        let actions = self.sync_action_to_engine_actions_delta(
            &sync_action,
            &outcome.view,
            &outcome.view_delta,
            serialize_delta,
        );

        DeltaMutateResult {
            generation: outcome.generation,
            view: outcome.view,
            view_delta: outcome.view_delta,
            actions,
        }
    }

    // ── Inbound handling ────────────────────────────────────────

    /// Process an inbound synchronization message.
    ///
    /// Returns actions the host must execute (e.g., sending a snapshot
    /// in response to a `RequestSnapshot`).
    pub fn handle_inbound_sync(&mut self, msg: SyncMessage) -> Vec<EngineAction> {
        match msg {
            SyncMessage::FullSnapshot {
                state_name,
                source_node,
                generation,
                wire_version,
                data,
            } => self.handle_full_snapshot(state_name, source_node, generation, wire_version, data),

            SyncMessage::DeltaUpdate {
                state_name,
                source_node,
                generation,
                wire_version,
                data,
            } => self.handle_delta_update(state_name, source_node, generation, wire_version, data),

            SyncMessage::RequestSnapshot {
                state_name,
                requester,
            } => self.handle_request_snapshot(state_name, requester),
        }
    }

    /// Process an inbound change feed batch.
    ///
    /// Marks peer views as stale based on feed notifications. Stale views
    /// will trigger snapshot requests on the next query (if `pull_on_query`
    /// is enabled).
    pub fn handle_inbound_change_feed(&mut self, feed: BatchedChangeFeed) {
        let entries =
            ChangeFeedLogic::route_inbound_batch(&feed, self.shard.node_id());
        for (sn, source, gen) in entries {
            if sn != self.state_name {
                continue;
            }
            self.shard.mark_stale(source, gen.incarnation, gen.age);
        }
    }

    // ── Change feed ─────────────────────────────────────────────

    /// Flush pending change feed notifications into a batch.
    ///
    /// Call this at the configured `batch_interval`. Returns `None` if no
    /// notifications are pending.
    pub fn flush_change_feed(&mut self) -> Option<BatchedChangeFeed> {
        self.change_feed.flush()
    }

    /// Number of change feed notifications pending flush.
    pub fn pending_change_feed_count(&self) -> usize {
        self.change_feed.pending_count()
    }

    // ── Cluster lifecycle ───────────────────────────────────────

    /// Handle a node joining the cluster.
    ///
    /// Returns a `SendSync` action with our current snapshot for the new peer.
    pub fn on_node_joined(&mut self, node_id: NodeId, default_view: V) -> Vec<EngineAction> {
        let join_actions = self.lifecycle.on_node_joined(&self.shard, node_id, default_view);

        let mut actions = Vec::new();
        for ja in join_actions {
            match ja {
                crate::core::lifecycle_logic::JoinAction::SendSnapshotToPeer { target } => {
                    let data = (self.serialize_view)(&self.own_view_value());
                    self.diagnostics.record_snapshot_sent();
                    actions.push(EngineAction::SendSync {
                        target,
                        message: SyncMessage::FullSnapshot {
                            state_name: self.state_name.clone(),
                            source_node: self.shard.node_id(),
                            generation: self.shard.state().generation,
                            wire_version: self.wire_version,
                            data,
                        },
                    });
                }
            }
        }
        actions
    }

    /// Handle a node leaving the cluster.
    ///
    /// Removes the node's view and adds it to the departed set (filtering
    /// late in-flight messages).
    pub fn on_node_left(&mut self, node_id: NodeId) {
        self.lifecycle.on_node_left(&self.shard, node_id);
        self.diagnostics.remove_peer(&node_id);
    }

    /// Check whether inbound messages from a node should be accepted.
    pub fn should_accept_from(&self, node_id: &NodeId) -> bool {
        self.lifecycle.should_accept_from(node_id)
    }

    // ── Query ───────────────────────────────────────────────────

    /// Query the current view of all peers.
    ///
    /// If `pull_on_query` is enabled and stale peers are detected, the
    /// engine issues `RequestSnapshot` actions for those peers.
    pub fn query<R>(
        &self,
        max_staleness: Duration,
        project: impl FnOnce(&HashMap<NodeId, StateViewObject<V>>) -> R,
    ) -> (EngineQueryResult<R>, Vec<EngineAction>) {
        let result = self.shard.query_local(max_staleness, project);
        match result {
            crate::core::shard_core::QueryResult::Fresh(r) => {
                (EngineQueryResult::Fresh(r), vec![])
            }
            crate::core::shard_core::QueryResult::StalePeersDetected {
                stale_peers,
                result,
            } => {
                let actions = if self.sync_logic.strategy().pull_on_query {
                    stale_peers
                        .iter()
                        .map(|&peer| EngineAction::SendSync {
                            target: peer,
                            message: SyncMessage::RequestSnapshot {
                                state_name: self.state_name.clone(),
                                requester: self.shard.node_id(),
                            },
                        })
                        .collect()
                } else {
                    vec![]
                };
                (
                    EngineQueryResult::Stale {
                        result,
                        stale_peers,
                    },
                    actions,
                )
            }
        }
    }

    // ── Periodic sync ───────────────────────────────────────────

    /// Produce a full snapshot broadcast for periodic sync.
    ///
    /// Call this at the interval returned by [`periodic_interval`](Self::periodic_interval).
    /// Returns an empty `Vec` if periodic sync is not configured.
    pub fn periodic_sync(&mut self) -> Vec<EngineAction> {
        if !self.sync_logic.should_periodic_sync() {
            return vec![];
        }

        let data = (self.serialize_view)(&self.own_view_value());
        self.diagnostics.record_snapshot_sent();

        vec![EngineAction::BroadcastSync(SyncMessage::FullSnapshot {
            state_name: self.state_name.clone(),
            source_node: self.shard.node_id(),
            generation: self.shard.state().generation,
            wire_version: self.wire_version,
            data,
        })]
    }

    /// The configured periodic full-sync interval, if any.
    pub fn periodic_interval(&self) -> Option<Duration> {
        self.sync_logic.periodic_interval()
    }

    // ── Diagnostics & accessors ─────────────────────────────────

    pub fn node_id(&self) -> NodeId {
        self.shard.node_id()
    }

    pub fn state_name(&self) -> &str {
        &self.state_name
    }

    pub fn state(&self) -> &StateObject<S> {
        self.shard.state()
    }

    pub fn view_count(&self) -> usize {
        self.shard.view_count()
    }

    pub fn get_view(&self, node_id: &NodeId) -> Option<Arc<StateViewObject<V>>> {
        self.shard.get_view(node_id)
    }

    /// Get a snapshot of all peer views.
    pub fn snapshot(&self) -> HashMap<NodeId, StateViewObject<V>> {
        self.shard.snapshot()
    }

    /// Current sync metrics.
    pub fn metrics(&self) -> SyncMetrics {
        let m = self.diagnostics.metrics();
        SyncMetrics {
            total_mutations: m.total_mutations,
            deltas_sent: m.deltas_sent,
            deltas_received: m.deltas_received,
            snapshots_sent: m.snapshots_sent,
            snapshots_received: m.snapshots_received,
            deltas_suppressed: m.deltas_suppressed,
            deltas_immediate: m.deltas_immediate,
            sync_failures: m.sync_failures,
            age_gaps_detected: m.age_gaps_detected,
            stale_deltas_discarded: m.stale_deltas_discarded,
        }
    }

    /// Health assessment of this state engine.
    pub fn health(&self, max_staleness: Duration) -> EngineHealth {
        let h = self.diagnostics.health_status(&self.shard, max_staleness);
        EngineHealth {
            state_name: h.state_name,
            healthy_peers: h.healthy_peers,
            stale_peers: h.stale_peers,
            failing_peers: h.failing_peers,
            status: match h.status {
                crate::core::diagnostics::HealthStatus::Healthy => HealthStatus::Healthy,
                crate::core::diagnostics::HealthStatus::Degraded => HealthStatus::Degraded,
                crate::core::diagnostics::HealthStatus::Unhealthy => HealthStatus::Unhealthy,
            },
        }
    }

    /// The configured sync strategy.
    pub fn sync_strategy(&self) -> &crate::types::config::SyncStrategy {
        self.sync_logic.strategy()
    }

    // ── Internal helpers ────────────────────────────────────────

    fn own_view_value(&self) -> V {
        self.shard
            .get_view(&self.shard.node_id())
            .expect("own view should always exist")
            .value
            .clone()
    }

    fn make_snapshot_request(&self, target: NodeId) -> EngineAction {
        EngineAction::SendSync {
            target,
            message: SyncMessage::RequestSnapshot {
                state_name: self.state_name.clone(),
                requester: self.shard.node_id(),
            },
        }
    }

    /// Convert a SyncAction into EngineActions using a full snapshot.
    fn sync_action_to_engine_actions_snapshot(
        &mut self,
        action: &SyncAction,
        view: &V,
    ) -> Vec<EngineAction> {
        match action {
            SyncAction::BroadcastSnapshot | SyncAction::BroadcastDelta => {
                // For the non-delta mutate path, always send a snapshot
                // even if SyncLogic says BroadcastDelta.
                let data = (self.serialize_view)(view);
                self.diagnostics.record_snapshot_sent();
                vec![EngineAction::BroadcastSync(SyncMessage::FullSnapshot {
                    state_name: self.state_name.clone(),
                    source_node: self.shard.node_id(),
                    generation: self.shard.state().generation,
                    wire_version: self.wire_version,
                    data,
                })]
            }
            SyncAction::NotifyChangeFeed => {
                self.change_feed.notify_change(
                    self.state_name.clone(),
                    self.shard.node_id(),
                    self.shard.state().generation,
                );
                vec![]
            }
            SyncAction::ScheduleDelayed(delay) => {
                let data = (self.serialize_view)(view);
                vec![EngineAction::ScheduleDelayed {
                    delay: *delay,
                    message: SyncMessage::FullSnapshot {
                        state_name: self.state_name.clone(),
                        source_node: self.shard.node_id(),
                        generation: self.shard.state().generation,
                        wire_version: self.wire_version,
                        data,
                    },
                }]
            }
            SyncAction::Suppress => {
                self.diagnostics.record_delta_suppressed();
                vec![]
            }
        }
    }

    /// Convert a SyncAction into EngineActions, preferring delta when appropriate.
    fn sync_action_to_engine_actions_delta<VD: Clone + Debug>(
        &mut self,
        action: &SyncAction,
        view: &V,
        view_delta: &VD,
        serialize_delta: impl FnOnce(&VD) -> Vec<u8>,
    ) -> Vec<EngineAction> {
        match action {
            SyncAction::BroadcastDelta => {
                let data = serialize_delta(view_delta);
                self.diagnostics.record_delta_sent();
                self.diagnostics.record_delta_immediate();
                vec![EngineAction::BroadcastSync(SyncMessage::DeltaUpdate {
                    state_name: self.state_name.clone(),
                    source_node: self.shard.node_id(),
                    generation: self.shard.state().generation,
                    wire_version: self.wire_version,
                    data,
                })]
            }
            SyncAction::BroadcastSnapshot => {
                let data = (self.serialize_view)(view);
                self.diagnostics.record_snapshot_sent();
                vec![EngineAction::BroadcastSync(SyncMessage::FullSnapshot {
                    state_name: self.state_name.clone(),
                    source_node: self.shard.node_id(),
                    generation: self.shard.state().generation,
                    wire_version: self.wire_version,
                    data,
                })]
            }
            SyncAction::NotifyChangeFeed => {
                self.change_feed.notify_change(
                    self.state_name.clone(),
                    self.shard.node_id(),
                    self.shard.state().generation,
                );
                vec![]
            }
            SyncAction::ScheduleDelayed(delay) => {
                let data = serialize_delta(view_delta);
                vec![EngineAction::ScheduleDelayed {
                    delay: *delay,
                    message: SyncMessage::DeltaUpdate {
                        state_name: self.state_name.clone(),
                        source_node: self.shard.node_id(),
                        generation: self.shard.state().generation,
                        wire_version: self.wire_version,
                        data,
                    },
                }]
            }
            SyncAction::Suppress => {
                self.diagnostics.record_delta_suppressed();
                vec![]
            }
        }
    }

    fn handle_full_snapshot(
        &mut self,
        state_name: String,
        source_node: NodeId,
        generation: Generation,
        wire_version: u32,
        data: Vec<u8>,
    ) -> Vec<EngineAction> {
        if state_name != self.state_name {
            return vec![];
        }
        if !self.lifecycle.should_accept_from(&source_node) {
            return vec![];
        }

        let view = match self.versioning.on_inbound(&data, wire_version, |d, v| {
            (self.deserialize_view)(d, v)
        }) {
            InboundVersionResult::Ok(view) => view,
            InboundVersionResult::VersionMismatch(action) => {
                match &action {
                    VersionMismatchAction::DropView { reason, .. } => {
                        self.diagnostics.record_sync_failure(
                            source_node,
                            reason,
                            self.shard.clock(),
                        );
                    }
                    VersionMismatchAction::KeepStale { reason, .. } => {
                        self.diagnostics.record_sync_failure(
                            source_node,
                            reason,
                            self.shard.clock(),
                        );
                    }
                }
                return vec![];
            }
            InboundVersionResult::MalformedData(msg) => {
                self.diagnostics.record_sync_failure(
                    source_node,
                    &format!("malformed: {msg}"),
                    self.shard.clock(),
                );
                return vec![];
            }
        };

        let now_ms = self.shard.clock().unix_ms();
        let snap = InboundSnapshot {
            source: source_node,
            generation,
            wire_version,
            view,
            created_time: now_ms,
            modified_time: now_ms,
        };

        match self.shard.accept_inbound_snapshot(snap) {
            AcceptResult::Accepted => {
                self.diagnostics
                    .record_snapshot_received(source_node, 0, self.shard.clock());
            }
            AcceptResult::Discarded => {
                self.diagnostics.record_stale_delta_discarded();
            }
        }

        vec![]
    }

    fn handle_delta_update(
        &mut self,
        state_name: String,
        source_node: NodeId,
        generation: Generation,
        wire_version: u32,
        data: Vec<u8>,
    ) -> Vec<EngineAction> {
        if state_name != self.state_name {
            return vec![];
        }
        if !self.lifecycle.should_accept_from(&source_node) {
            return vec![];
        }

        let delta_applier = match &self.inbound_delta_applier {
            Some(applier) => applier,
            None => return vec![self.make_snapshot_request(source_node)],
        };

        // Get the current view for the source peer.
        let current_view = match self.shard.get_view(&source_node) {
            Some(v) => v,
            None => return vec![self.make_snapshot_request(source_node)],
        };

        // Apply the delta to produce the new view.
        let new_view = match delta_applier(&data, wire_version, &current_view.value) {
            Ok(v) => v,
            Err(_) => {
                self.diagnostics.record_sync_failure(
                    source_node,
                    "delta deserialization failed",
                    self.shard.clock(),
                );
                return vec![self.make_snapshot_request(source_node)];
            }
        };

        let result = self
            .shard
            .accept_inbound_delta(source_node, generation, wire_version, |_existing| {
                new_view
            });

        match result {
            DeltaAcceptResult::Applied => {
                self.diagnostics
                    .record_delta_received(source_node, 0, self.shard.clock());
                vec![]
            }
            DeltaAcceptResult::GapDetected { .. } => {
                self.diagnostics.record_gap_detected(source_node);
                vec![self.make_snapshot_request(source_node)]
            }
            DeltaAcceptResult::Discarded => {
                self.diagnostics.record_stale_delta_discarded();
                vec![]
            }
            DeltaAcceptResult::UnknownPeer => {
                vec![self.make_snapshot_request(source_node)]
            }
        }
    }

    fn handle_request_snapshot(
        &mut self,
        state_name: String,
        requester: NodeId,
    ) -> Vec<EngineAction> {
        if state_name != self.state_name {
            return vec![];
        }

        let data = (self.serialize_view)(&self.own_view_value());
        self.diagnostics.record_snapshot_sent();

        vec![EngineAction::SendSync {
            target: requester,
            message: SyncMessage::FullSnapshot {
                state_name: self.state_name.clone(),
                source_node: self.shard.node_id(),
                generation: self.shard.state().generation,
                wire_version: self.wire_version,
                data,
            },
        }]
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_state::{
        TestDeltaChange, TestDeltaState, TestDeltaStateInner, TestDeltaView, TestDeltaViewDelta,
        TestState,
    };
    use crate::traits::clock::TestClock;
    use crate::traits::state::{DeltaDistributedState, SyncUrgency};
    use crate::types::config::{StateConfig, SyncStrategy};

    fn test_clock() -> Arc<TestClock> {
        Arc::new(TestClock::with_base_unix_ms(1_000_000))
    }

    fn make_engine(
        node_id: NodeId,
        clock: Arc<TestClock>,
    ) -> DistributedStateEngine<TestState, TestState> {
        DistributedStateEngine::new(
            "test_state",
            node_id,
            TestState::default(),
            |s| s.clone(),
            StateConfig::default(), // ActivePush
            1,
            clock,
            |v| bincode::serialize(v).unwrap(),
            |b, v| {
                if v != 1 {
                    return Err(DeserializeError::unknown_version(v));
                }
                bincode::deserialize(b)
                    .map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        )
    }

    fn make_delta_engine(
        node_id: NodeId,
        clock: Arc<TestClock>,
    ) -> DistributedStateEngine<TestDeltaStateInner, TestDeltaView> {
        DistributedStateEngine::new(
            "test_delta_state",
            node_id,
            TestDeltaStateInner::default(),
            TestDeltaState::project_view,
            StateConfig::default(),
            1,
            clock,
            TestDeltaState::serialize_view,
            TestDeltaState::deserialize_view,
            Some(Box::new(|bytes, version, current_view| {
                let delta = TestDeltaState::deserialize_delta(bytes, version)?;
                Ok(TestDeltaState::apply_delta(current_view, &delta))
            })),
        )
    }

    fn make_feed_engine(
        node_id: NodeId,
        clock: Arc<TestClock>,
    ) -> DistributedStateEngine<TestState, TestState> {
        DistributedStateEngine::new(
            "test_state",
            node_id,
            TestState::default(),
            |s| s.clone(),
            StateConfig {
                sync_strategy: SyncStrategy::feed_lazy_pull(),
                ..StateConfig::default()
            },
            1,
            clock,
            |v| bincode::serialize(v).unwrap(),
            |b, v| {
                if v != 1 {
                    return Err(DeserializeError::unknown_version(v));
                }
                bincode::deserialize(b)
                    .map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        )
    }

    // ── Constructor tests ───────────────────────────────────────

    #[test]
    fn engine_initial_state() {
        let clock = test_clock();
        let engine = make_engine(NodeId(1), clock);

        assert_eq!(engine.node_id(), NodeId(1));
        assert_eq!(engine.state_name(), "test_state");
        assert_eq!(engine.view_count(), 1); // own view
        assert_eq!(engine.state().value.counter, 0);
    }

    // ── Mutation tests ──────────────────────────────────────────

    #[test]
    fn mutate_active_push_broadcasts_snapshot() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);

        let result = engine.mutate(
            |s| {
                s.counter += 1;
                s.label = "updated".into();
            },
            |s| s.clone(),
            SyncUrgency::Default,
        );

        assert_eq!(result.generation.age, 1);
        assert_eq!(result.view.counter, 1);
        assert_eq!(result.view.label, "updated");
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(
            &result.actions[0],
            EngineAction::BroadcastSync(SyncMessage::FullSnapshot { .. })
        ));

        // Verify metrics
        let m = engine.metrics();
        assert_eq!(m.total_mutations, 1);
        assert_eq!(m.snapshots_sent, 1);
    }

    #[test]
    fn mutate_suppress_no_actions() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);

        let result = engine.mutate(|s| s.counter += 1, |s| s.clone(), SyncUrgency::Suppress);

        assert_eq!(result.actions.len(), 0);
        assert_eq!(engine.metrics().deltas_suppressed, 1);
    }

    #[test]
    fn mutate_delayed_produces_scheduled_action() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        let delay = Duration::from_secs(5);

        let result = engine.mutate(
            |s| s.counter += 1,
            |s| s.clone(),
            SyncUrgency::Delayed(delay),
        );

        assert_eq!(result.actions.len(), 1);
        assert!(matches!(
            &result.actions[0],
            EngineAction::ScheduleDelayed { delay: d, .. } if *d == Duration::from_secs(5)
        ));
    }

    #[test]
    fn mutate_feed_mode_notifies_change_feed() {
        let clock = test_clock();
        let mut engine = make_feed_engine(NodeId(1), clock);

        let result = engine.mutate(|s| s.counter += 1, |s| s.clone(), SyncUrgency::Default);

        // Feed mode produces no immediate actions (notifications are batched)
        assert!(result.actions.is_empty());
        assert_eq!(engine.pending_change_feed_count(), 1);

        // Flush produces a BatchedChangeFeed
        let feed = engine.flush_change_feed().unwrap();
        assert_eq!(feed.source_node, NodeId(1));
        assert_eq!(feed.notifications.len(), 1);
        assert_eq!(feed.notifications[0].state_name, "test_state");
    }

    #[test]
    fn mutate_immediate_overrides_feed_mode() {
        let clock = test_clock();
        let mut engine = make_feed_engine(NodeId(1), clock);

        let result = engine.mutate(|s| s.counter += 1, |s| s.clone(), SyncUrgency::Immediate);

        // Immediate urgency broadcasts even in feed mode
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(
            &result.actions[0],
            EngineAction::BroadcastSync(SyncMessage::FullSnapshot { .. })
        ));
    }

    // ── Delta mutation tests ────────────────────────────────────

    #[test]
    fn delta_mutate_broadcasts_delta() {
        let clock = test_clock();
        let mut engine = make_delta_engine(NodeId(1), clock);

        let result = engine.mutate_with_delta(
            |s| {
                s.counter += 5;
                TestDeltaChange {
                    counter_delta: 5,
                    new_label: None,
                    accumulator_delta: 0.0,
                }
            },
            TestDeltaState::project_view,
            TestDeltaState::project_delta,
            SyncUrgency::Default,
            TestDeltaState::serialize_delta,
        );

        assert_eq!(result.view.counter, 5);
        assert_eq!(result.view_delta.counter_delta, 5);
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(
            &result.actions[0],
            EngineAction::BroadcastSync(SyncMessage::DeltaUpdate { .. })
        ));

        let m = engine.metrics();
        assert_eq!(m.deltas_sent, 1);
        assert_eq!(m.deltas_immediate, 1);
    }

    // ── Inbound snapshot tests ──────────────────────────────────

    #[test]
    fn inbound_snapshot_accepted() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);

        // Add node 2 as a peer
        engine.on_node_joined(NodeId(2), TestState::default());

        // Simulate inbound snapshot from node 2
        let peer_state = TestState {
            counter: 42,
            label: "peer".into(),
        };
        let data = bincode::serialize(&peer_state).unwrap();

        let actions = engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1_000_000, 3),
            wire_version: 1,
            data,
        });

        assert!(actions.is_empty());

        // Verify the view was updated
        let view = engine.get_view(&NodeId(2)).unwrap();
        assert_eq!(view.value.counter, 42);
        assert_eq!(view.generation.age, 3);

        assert_eq!(engine.metrics().snapshots_received, 1);
    }

    #[test]
    fn inbound_snapshot_wrong_state_name_ignored() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        engine.on_node_joined(NodeId(2), TestState::default());

        let actions = engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "other_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1, 1),
            wire_version: 1,
            data: vec![],
        });

        assert!(actions.is_empty());
        assert_eq!(engine.metrics().snapshots_received, 0);
    }

    #[test]
    fn inbound_snapshot_departed_node_rejected() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);

        // Add and then remove node 2
        engine.on_node_joined(NodeId(2), TestState::default());
        engine.on_node_left(NodeId(2));

        let peer_state = TestState {
            counter: 99,
            label: "ghost".into(),
        };
        let data = bincode::serialize(&peer_state).unwrap();

        let actions = engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1, 1),
            wire_version: 1,
            data,
        });

        assert!(actions.is_empty());
        assert!(engine.get_view(&NodeId(2)).is_none());
    }

    #[test]
    fn inbound_snapshot_version_mismatch() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        engine.on_node_joined(NodeId(2), TestState::default());

        let actions = engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1, 1),
            wire_version: 99, // unsupported
            data: vec![1, 2, 3],
        });

        assert!(actions.is_empty());
        assert_eq!(engine.metrics().sync_failures, 1);
    }

    #[test]
    fn inbound_snapshot_malformed_data() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        engine.on_node_joined(NodeId(2), TestState::default());

        let actions = engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1, 1),
            wire_version: 1,
            data: vec![0xFF, 0xFF], // garbage
        });

        assert!(actions.is_empty());
        assert_eq!(engine.metrics().sync_failures, 1);
    }

    // ── Inbound delta tests ─────────────────────────────────────

    #[test]
    fn inbound_delta_applied() {
        let clock = test_clock();
        let mut engine = make_delta_engine(NodeId(1), clock);

        // Add peer and accept initial snapshot
        engine.on_node_joined(NodeId(2), TestDeltaView { counter: 0, label: String::new() });
        let initial_view = TestDeltaView {
            counter: 10,
            label: "peer".into(),
        };
        let data = TestDeltaState::serialize_view(&initial_view);
        engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_delta_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1_000_000, 1),
            wire_version: 1,
            data,
        });

        // Now send a delta
        let delta = TestDeltaViewDelta {
            counter_delta: 5,
            new_label: None,
        };
        let delta_data = TestDeltaState::serialize_delta(&delta);
        let actions = engine.handle_inbound_sync(SyncMessage::DeltaUpdate {
            state_name: "test_delta_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1_000_000, 2),
            wire_version: 1,
            data: delta_data,
        });

        assert!(actions.is_empty());
        let view = engine.get_view(&NodeId(2)).unwrap();
        assert_eq!(view.value.counter, 15); // 10 + 5
        assert_eq!(engine.metrics().deltas_received, 1);
    }

    #[test]
    fn inbound_delta_without_applier_requests_snapshot() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock); // no delta applier
        engine.on_node_joined(NodeId(2), TestState::default());

        let actions = engine.handle_inbound_sync(SyncMessage::DeltaUpdate {
            state_name: "test_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1, 1),
            wire_version: 1,
            data: vec![1, 2, 3],
        });

        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            EngineAction::SendSync {
                target: NodeId(2),
                message: SyncMessage::RequestSnapshot { .. }
            }
        ));
    }

    #[test]
    fn inbound_delta_gap_requests_snapshot() {
        let clock = test_clock();
        let mut engine = make_delta_engine(NodeId(1), clock);
        engine.on_node_joined(NodeId(2), TestDeltaView { counter: 0, label: String::new() });

        // Accept snapshot at age=1
        let initial_view = TestDeltaView { counter: 10, label: "peer".into() };
        engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_delta_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1_000_000, 1),
            wire_version: 1,
            data: TestDeltaState::serialize_view(&initial_view),
        });

        // Send delta at age=5 (gap: expected age=2)
        let delta = TestDeltaViewDelta { counter_delta: 1, new_label: None };
        let actions = engine.handle_inbound_sync(SyncMessage::DeltaUpdate {
            state_name: "test_delta_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1_000_000, 5), // gap
            wire_version: 1,
            data: TestDeltaState::serialize_delta(&delta),
        });

        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            EngineAction::SendSync {
                message: SyncMessage::RequestSnapshot { .. },
                ..
            }
        ));
        assert_eq!(engine.metrics().age_gaps_detected, 1);
    }

    // ── Request snapshot tests ───────────────────────────────────

    #[test]
    fn request_snapshot_sends_own_view() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        engine.mutate(|s| s.counter = 42, |s| s.clone(), SyncUrgency::Default);

        let actions = engine.handle_inbound_sync(SyncMessage::RequestSnapshot {
            state_name: "test_state".into(),
            requester: NodeId(2),
        });

        assert_eq!(actions.len(), 1);
        if let EngineAction::SendSync {
            target,
            message: SyncMessage::FullSnapshot { data, .. },
        } = &actions[0]
        {
            assert_eq!(*target, NodeId(2));
            let view: TestState = bincode::deserialize(data).unwrap();
            assert_eq!(view.counter, 42);
        } else {
            panic!("expected SendSync with FullSnapshot");
        }
    }

    // ── Change feed tests ───────────────────────────────────────

    #[test]
    fn change_feed_marks_stale() {
        let clock = test_clock();
        let mut engine = make_feed_engine(NodeId(1), clock.clone());
        engine.on_node_joined(NodeId(2), TestState::default());

        // Accept initial snapshot from node 2 so it has a real generation
        let data = bincode::serialize(&TestState { counter: 1, label: "x".into() }).unwrap();
        engine.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_state".into(),
            source_node: NodeId(2),
            generation: Generation::new(1_000_000, 1),
            wire_version: 1,
            data,
        });

        // Receive a change feed indicating node 2 has a newer generation
        let feed = BatchedChangeFeed {
            source_node: NodeId(2),
            notifications: vec![crate::types::sync_message::ChangeNotification {
                state_name: "test_state".into(),
                source_node: NodeId(2),
                generation: Generation::new(1_000_000, 5),
            }],
        };
        engine.handle_inbound_change_feed(feed);

        // Now query should detect staleness
        clock.advance(Duration::from_secs(10));
        let (result, actions) = engine.query(Duration::from_millis(1), |views| {
            views.get(&NodeId(2)).map(|v| v.value.counter)
        });

        assert!(matches!(result, EngineQueryResult::Stale { .. }));
        // Feed+lazy pull should issue RequestSnapshot
        assert!(!actions.is_empty());
    }

    // ── Lifecycle tests ─────────────────────────────────────────

    #[test]
    fn node_joined_sends_snapshot() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        engine.mutate(|s| s.counter = 10, |s| s.clone(), SyncUrgency::Default);

        let actions = engine.on_node_joined(NodeId(2), TestState::default());

        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            EngineAction::SendSync {
                target: NodeId(2),
                message: SyncMessage::FullSnapshot { .. }
            }
        ));
        assert_eq!(engine.view_count(), 2);
    }

    #[test]
    fn node_left_removes_view() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        engine.on_node_joined(NodeId(2), TestState::default());
        assert_eq!(engine.view_count(), 2);

        engine.on_node_left(NodeId(2));

        assert_eq!(engine.view_count(), 1);
        assert!(engine.get_view(&NodeId(2)).is_none());
        assert!(!engine.should_accept_from(&NodeId(2)));
    }

    // ── Query tests ─────────────────────────────────────────────

    #[test]
    fn query_fresh_returns_all_views() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);
        engine.mutate(|s| s.counter = 100, |s| s.clone(), SyncUrgency::Default);

        let (result, actions) = engine.query(Duration::from_secs(60), |views| {
            views.get(&NodeId(1)).unwrap().value.counter
        });

        assert!(matches!(result, EngineQueryResult::Fresh(100)));
        assert!(actions.is_empty());
    }

    // ── Periodic sync tests ─────────────────────────────────────

    #[test]
    fn periodic_sync_broadcasts_snapshot() {
        let clock = test_clock();
        let mut engine = DistributedStateEngine::new(
            "test_state",
            NodeId(1),
            TestState::default(),
            |s| s.clone(),
            StateConfig {
                sync_strategy: SyncStrategy::periodic_only(Duration::from_secs(30)),
                ..StateConfig::default()
            },
            1,
            clock,
            |v| bincode::serialize(v).unwrap(),
            |b, _| {
                bincode::deserialize(b)
                    .map_err(|e| DeserializeError::Malformed(e.to_string()))
            },
            None,
        );

        engine.mutate(|s| s.counter = 5, |s| s.clone(), SyncUrgency::Suppress);
        let actions = engine.periodic_sync();

        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            EngineAction::BroadcastSync(SyncMessage::FullSnapshot { .. })
        ));
    }

    #[test]
    fn periodic_sync_returns_empty_when_not_configured() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock); // ActivePush, no periodic
        let actions = engine.periodic_sync();
        assert!(actions.is_empty());
    }

    // ── Health & metrics tests ──────────────────────────────────

    #[test]
    fn health_status_with_no_peers() {
        let clock = test_clock();
        let engine = make_engine(NodeId(1), clock);
        let h = engine.health(Duration::from_secs(60));
        assert_eq!(h.status, HealthStatus::Healthy);
        assert_eq!(h.stale_peers, 0);
    }

    #[test]
    fn metrics_track_mutations() {
        let clock = test_clock();
        let mut engine = make_engine(NodeId(1), clock);

        for _ in 0..5 {
            engine.mutate(|s| s.counter += 1, |s| s.clone(), SyncUrgency::Default);
        }

        let m = engine.metrics();
        assert_eq!(m.total_mutations, 5);
        assert_eq!(m.snapshots_sent, 5); // ActivePush sends snapshot each time
    }

    // ── Round-trip test (mutate → serialize → deserialize → accept) ──

    #[test]
    fn full_round_trip_two_engines() {
        let clock = test_clock();
        let mut engine1 = make_engine(NodeId(1), clock.clone());
        let mut engine2 = make_engine(NodeId(2), clock);

        // Join each other
        engine1.on_node_joined(NodeId(2), TestState::default());
        engine2.on_node_joined(NodeId(1), TestState::default());

        // Engine 1 mutates
        let result = engine1.mutate(
            |s| {
                s.counter = 42;
                s.label = "hello".into();
            },
            |s| s.clone(),
            SyncUrgency::Default,
        );

        // Deliver the broadcast to engine 2
        assert_eq!(result.actions.len(), 1);
        if let EngineAction::BroadcastSync(msg) = &result.actions[0] {
            let actions = engine2.handle_inbound_sync(msg.clone());
            assert!(actions.is_empty());
        }

        // Verify engine 2 has the updated view
        let view = engine2.get_view(&NodeId(1)).unwrap();
        assert_eq!(view.value.counter, 42);
        assert_eq!(view.value.label, "hello");
    }

    #[test]
    fn delta_round_trip_two_engines() {
        let clock = test_clock();
        let mut engine1 = make_delta_engine(NodeId(1), clock.clone());
        let mut engine2 = make_delta_engine(NodeId(2), clock);

        let default_view = TestDeltaView {
            counter: 0,
            label: String::new(),
        };
        engine1.on_node_joined(NodeId(2), default_view.clone());
        engine2.on_node_joined(NodeId(1), default_view);

        // Engine 1 mutates with delta
        let result = engine1.mutate_with_delta(
            |s| {
                s.counter += 10;
                TestDeltaChange {
                    counter_delta: 10,
                    new_label: Some("updated".into()),
                    accumulator_delta: 0.0,
                }
            },
            TestDeltaState::project_view,
            TestDeltaState::project_delta,
            SyncUrgency::Default,
            TestDeltaState::serialize_delta,
        );

        // Engine 2 needs a base snapshot first (initial view has gen=0)
        // Send a full snapshot instead of the delta for initial sync
        let snapshot_data = TestDeltaState::serialize_view(&result.view);
        engine2.handle_inbound_sync(SyncMessage::FullSnapshot {
            state_name: "test_delta_state".into(),
            source_node: NodeId(1),
            generation: result.generation,
            wire_version: 1,
            data: snapshot_data,
        });

        // Now engine 1 mutates again, producing a delta
        let result2 = engine1.mutate_with_delta(
            |s| {
                s.counter += 3;
                TestDeltaChange {
                    counter_delta: 3,
                    new_label: None,
                    accumulator_delta: 0.0,
                }
            },
            TestDeltaState::project_view,
            TestDeltaState::project_delta,
            SyncUrgency::Default,
            TestDeltaState::serialize_delta,
        );

        // Deliver delta to engine 2
        if let EngineAction::BroadcastSync(msg) = &result2.actions[0] {
            let actions = engine2.handle_inbound_sync(msg.clone());
            assert!(actions.is_empty());
        }

        // Verify engine 2 applied the delta
        let view = engine2.get_view(&NodeId(1)).unwrap();
        assert_eq!(view.value.counter, 13); // 10 + 3
    }
}
