use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;

use crate::core::shard_core::ShardCore;
use crate::traits::clock::Clock;
use crate::types::node::NodeId;

// ═══════════════════════════════════════════════════════════════
// Diagnostic types
// ═══════════════════════════════════════════════════════════════

/// Per-peer synchronization status snapshot.
///
/// Captures the state of a single peer's view as seen by the local node,
/// including sync health indicators (failures, gaps, latency).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct PeerSyncStatus {
    pub node_id: NodeId,
    pub local_age: u64,
    pub remote_age: Option<u64>,
    pub gap_count: u64,
    pub consecutive_failures: u32,
    pub last_sync_time: Option<i64>,
    pub last_attempt_time: Option<i64>,
    pub last_error: Option<String>,
    pub last_sync_latency_ms: Option<u64>,
}

/// Aggregate sync metrics for a single state type.
///
/// All counters are monotonically increasing since actor startup.
/// The actor shell reads these for dashboards and alerting.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct StateSyncMetrics {
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

/// Health status for a single state type across the cluster.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct StateHealth {
    pub state_name: String,
    pub healthy_peers: u32,
    pub stale_peers: u32,
    pub failing_peers: u32,
    pub status: HealthStatus,
}

/// Overall health classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthStatus {
    /// All peers synced within freshness window.
    Healthy,
    /// Some peers stale or failing, but majority healthy.
    Degraded,
    /// Majority of peers stale or unreachable.
    Unhealthy,
}

// ═══════════════════════════════════════════════════════════════
// Per-peer diagnostic tracking
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default)]
struct PeerDiagnostics {
    consecutive_failures: u32,
    gap_count: u64,
    last_sync_time: Option<i64>,
    last_attempt_time: Option<i64>,
    last_error: Option<String>,
    last_sync_latency_ms: Option<u64>,
}

// ═══════════════════════════════════════════════════════════════
// DiagnosticsLogic
// ═══════════════════════════════════════════════════════════════

/// Pure logic for tracking sync diagnostics, metrics, and health.
///
/// # Threading model
///
/// This struct is intended for single-actor use. It is owned exclusively by
/// a `StateShard` actor and accessed sequentially within that actor's
/// message loop — no internal locking is needed.
///
/// # Data Flow
///
/// ```text
/// Actor shell event handlers call record_*() methods:
///
///   accept_inbound_snapshot() succeeded
///     └─ diagnostics.record_snapshot_received(peer, latency, clock)
///
///   accept_inbound_delta() → Applied
///     └─ diagnostics.record_delta_received(peer, latency, clock)
///
///   accept_inbound_delta() → Discarded
///     └─ diagnostics.record_stale_delta_discarded()
///
///   accept_inbound_delta() → GapDetected
///     └─ diagnostics.record_gap_detected(peer)
///
///   outbound delta sent
///     └─ diagnostics.record_delta_sent()
///
///   outbound snapshot sent
///     └─ diagnostics.record_snapshot_sent()
///
///   sync failure (network/serde/timeout)
///     └─ diagnostics.record_sync_failure(peer, error, clock)
///
///   mutation applied
///     └─ diagnostics.record_mutation()
///
///   SyncUrgency::Suppress
///     └─ diagnostics.record_delta_suppressed()
///
///   SyncUrgency::Immediate
///     └─ diagnostics.record_delta_immediate()
///
/// Query methods combine DiagnosticsLogic state + ShardCore view map:
///
///   diagnostics.sync_status(shard) → Vec<PeerSyncStatus>
///   diagnostics.metrics()          → &StateSyncMetrics
///   diagnostics.health_status(shard, max_staleness) → StateHealth
/// ```
#[allow(dead_code)]
pub(crate) struct DiagnosticsLogic {
    state_name: String,
    metrics: StateSyncMetrics,
    peers: HashMap<NodeId, PeerDiagnostics>,
}

#[allow(dead_code)]
impl DiagnosticsLogic {
    pub fn new(state_name: String) -> Self {
        Self {
            state_name,
            metrics: StateSyncMetrics::default(),
            peers: HashMap::new(),
        }
    }

    // ── Recording methods ───────────────────────────────────────

    /// Record a successful inbound sync (delta or snapshot applied) from a peer.
    ///
    /// Resets consecutive failures, updates latency and timestamps.
    /// Called by `record_delta_received` and `record_snapshot_received`.
    pub fn record_sync_success(&mut self, peer: NodeId, latency_ms: u64, clock: &dyn Clock) {
        let now = clock.unix_ms();
        let diag = self.peers.entry(peer).or_default();
        diag.consecutive_failures = 0;
        diag.last_sync_time = Some(now);
        diag.last_attempt_time = Some(now);
        diag.last_error = None;
        diag.last_sync_latency_ms = Some(latency_ms);

        tracing::debug!(
            state_name = %self.state_name,
            peer = ?peer,
            latency_ms = latency_ms,
            "sync succeeded"
        );
    }

    /// Record a sync failure for a peer.
    pub fn record_sync_failure(&mut self, peer: NodeId, error: &str, clock: &dyn Clock) {
        let now = clock.unix_ms();
        let diag = self.peers.entry(peer).or_default();
        diag.consecutive_failures += 1;
        diag.last_attempt_time = Some(now);
        diag.last_error = Some(error.to_string());
        self.metrics.sync_failures += 1;

        tracing::warn!(
            state_name = %self.state_name,
            peer = ?peer,
            error = error,
            consecutive_failures = diag.consecutive_failures,
            "sync failed"
        );
    }

    /// Record an age gap detected on inbound delta.
    pub fn record_gap_detected(&mut self, peer: NodeId) {
        let diag = self.peers.entry(peer).or_default();
        diag.gap_count += 1;
        self.metrics.age_gaps_detected += 1;
    }

    /// Record a local mutation applied.
    pub fn record_mutation(&mut self) {
        self.metrics.total_mutations += 1;
    }

    /// Record an outbound delta sent.
    pub fn record_delta_sent(&mut self) {
        self.metrics.deltas_sent += 1;
    }

    /// Record an inbound delta received and applied.
    pub fn record_delta_received(&mut self, peer: NodeId, latency_ms: u64, clock: &dyn Clock) {
        self.metrics.deltas_received += 1;
        self.record_sync_success(peer, latency_ms, clock);
    }

    /// Record an outbound snapshot sent.
    pub fn record_snapshot_sent(&mut self) {
        self.metrics.snapshots_sent += 1;
    }

    /// Record an inbound snapshot received and applied.
    pub fn record_snapshot_received(
        &mut self,
        peer: NodeId,
        latency_ms: u64,
        clock: &dyn Clock,
    ) {
        self.metrics.snapshots_received += 1;
        self.record_sync_success(peer, latency_ms, clock);
    }

    /// Record a delta suppressed (SyncUrgency::Suppress).
    pub fn record_delta_suppressed(&mut self) {
        self.metrics.deltas_suppressed += 1;

        tracing::debug!(
            state_name = %self.state_name,
            suppressed_count = self.metrics.deltas_suppressed,
            "delta suppressed"
        );
    }

    /// Record an immediate delta push (SyncUrgency::Immediate).
    pub fn record_delta_immediate(&mut self) {
        self.metrics.deltas_immediate += 1;
    }

    /// Record a stale inbound delta discarded.
    pub fn record_stale_delta_discarded(&mut self) {
        self.metrics.stale_deltas_discarded += 1;
    }

    // ── Query methods ───────────────────────────────────────────

    /// Build sync status for all peers in the view map.
    ///
    /// Combines view map data (age, synced_at, pending_remote_generation)
    /// with per-peer diagnostics (failures, latency, errors).
    pub fn sync_status<S, V>(
        &self,
        shard: &ShardCore<S, V>,
    ) -> Vec<PeerSyncStatus>
    where
        S: Clone + Send + Sync + 'static,
        V: Clone + Send + Sync + Debug + 'static,
    {
        let snapshot = shard.snapshot();
        snapshot
            .iter()
            .map(|(node_id, view)| {
                let diag = self.peers.get(node_id);
                let remote_age = view.pending_remote_generation.map(|g| g.age);
                PeerSyncStatus {
                    node_id: *node_id,
                    local_age: view.generation.age,
                    remote_age,
                    gap_count: diag.map_or(0, |d| d.gap_count),
                    consecutive_failures: diag.map_or(0, |d| d.consecutive_failures),
                    last_sync_time: diag.and_then(|d| d.last_sync_time),
                    last_attempt_time: diag.and_then(|d| d.last_attempt_time),
                    last_error: diag.and_then(|d| d.last_error.clone()),
                    last_sync_latency_ms: diag.and_then(|d| d.last_sync_latency_ms),
                }
            })
            .collect()
    }

    /// Return the accumulated metrics.
    pub fn metrics(&self) -> &StateSyncMetrics {
        &self.metrics
    }

    /// Compute health status for this state type.
    ///
    /// A peer is considered:
    /// - **healthy**: synced within `max_staleness` and no pending_remote_generation
    /// - **stale**: synced_at older than `max_staleness` or has pending_remote_generation
    /// - **failing**: has `consecutive_failures > 0`
    ///
    /// Note: a peer can be both stale and failing, so `stale_peers + failing_peers`
    /// may exceed the total peer count.
    ///
    /// Health classification uses **strict majority** (>50%):
    /// - `Healthy`: all non-local peers are healthy
    /// - `Degraded`: some peers stale/failing, but strict majority healthy
    /// - `Unhealthy`: half or more peers are stale/failing
    pub fn health_status<S, V>(
        &self,
        shard: &ShardCore<S, V>,
        max_staleness: Duration,
    ) -> StateHealth
    where
        S: Clone + Send + Sync + 'static,
        V: Clone + Send + Sync + Debug + 'static,
    {
        let local_id = shard.node_id();
        let snapshot = shard.snapshot();
        let clock = shard.clock();
        let now = clock.now();

        let mut healthy: u32 = 0;
        let mut stale: u32 = 0;
        let mut failing: u32 = 0;
        let mut total_peers: u32 = 0;

        for (node_id, view) in &snapshot {
            if *node_id == local_id {
                continue; // skip self
            }
            total_peers += 1;

            let is_stale = view.pending_remote_generation.is_some()
                || now
                    .checked_duration_since(view.synced_at)
                    .map_or(false, |d| d > max_staleness);

            let diag = self.peers.get(node_id);
            let is_failing = diag.map_or(false, |d| d.consecutive_failures > 0);

            if is_stale || is_failing {
                if is_stale {
                    stale += 1;
                }
                if is_failing {
                    failing += 1;
                }
            } else {
                healthy += 1;
            }
        }

        let status = if stale == 0 && failing == 0 {
            HealthStatus::Healthy
        } else if total_peers > 0 && healthy > total_peers / 2 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Unhealthy
        };

        StateHealth {
            state_name: self.state_name.clone(),
            healthy_peers: healthy,
            stale_peers: stale,
            failing_peers: failing,
            status,
        }
    }

    /// Remove tracking data for a departed peer.
    ///
    /// Should be called when `LifecycleLogic` processes a node leave,
    /// to prevent unbounded growth of the `peers` map on node churn.
    pub fn remove_peer(&mut self, peer: &NodeId) {
        self.peers.remove(peer);
    }

    /// The state name this diagnostics instance tracks.
    pub fn state_name(&self) -> &str {
        &self.state_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::core::shard_core::{new_state_object, InboundSnapshot, ShardCore};
    use crate::test_support::test_clock::TestClock;
    use crate::test_support::test_state::TestState;
    use crate::types::node::Generation;
    use tracing_test::traced_test;

    fn test_clock() -> Arc<dyn Clock> {
        Arc::new(TestClock::with_base_unix_ms(1_000_000))
    }

    fn make_shard(clock: Arc<dyn Clock>) -> ShardCore<TestState, TestState> {
        let state = new_state_object(
            TestState { counter: 0, label: "init".into() },
            clock.unix_ms() as u64,
            clock.as_ref(),
        );
        let view = state.value.clone();
        ShardCore::new(NodeId(1), state, view, clock)
    }

    fn make_3_node_shard(clock: Arc<dyn Clock>) -> ShardCore<TestState, TestState> {
        let shard = make_shard(clock.clone());
        shard.on_node_joined(NodeId(2), TestState { counter: 0, label: "".into() });
        shard.on_node_joined(NodeId(3), TestState { counter: 0, label: "".into() });

        // Accept snapshots to populate real views
        let now_ms = clock.unix_ms();
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(100, 5),
            wire_version: 1,
            view: TestState { counter: 5, label: "node2".into() },
            created_time: now_ms,
            modified_time: now_ms,
        });
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(3),
            generation: Generation::new(200, 3),
            wire_version: 1,
            view: TestState { counter: 3, label: "node3".into() },
            created_time: now_ms,
            modified_time: now_ms,
        });
        shard
    }

    // ── DIAG-01: sync_status() returns entry for each peer ──────

    #[test]
    fn diag_01_sync_status_returns_all_peers() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());
        let diag = DiagnosticsLogic::new("test_state".into());

        let status = diag.sync_status(&shard);
        assert_eq!(status.len(), 3); // self + 2 peers

        let ids: Vec<NodeId> = status.iter().map(|s| s.node_id).collect();
        assert!(ids.contains(&NodeId(1)));
        assert!(ids.contains(&NodeId(2)));
        assert!(ids.contains(&NodeId(3)));
    }

    // ── DIAG-02: consecutive_failures increments on failure ──────

    #[test]
    fn diag_02_consecutive_failures_increments() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());
        let mut diag = DiagnosticsLogic::new("test_state".into());

        diag.record_sync_failure(NodeId(2), "timeout", clock.as_ref());
        diag.record_sync_failure(NodeId(2), "timeout", clock.as_ref());
        diag.record_sync_failure(NodeId(2), "connection refused", clock.as_ref());

        let status = diag.sync_status(&shard);
        let peer2 = status.iter().find(|s| s.node_id == NodeId(2)).unwrap();
        assert_eq!(peer2.consecutive_failures, 3);
    }

    // ── DIAG-03: consecutive_failures resets on success ──────────

    #[test]
    fn diag_03_failures_reset_on_success() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());
        let mut diag = DiagnosticsLogic::new("test_state".into());

        diag.record_sync_failure(NodeId(2), "err1", clock.as_ref());
        diag.record_sync_failure(NodeId(2), "err2", clock.as_ref());
        assert_eq!(diag.peers.get(&NodeId(2)).unwrap().consecutive_failures, 2);

        diag.record_sync_success(NodeId(2), 5, clock.as_ref());

        let status = diag.sync_status(&shard);
        let peer2 = status.iter().find(|s| s.node_id == NodeId(2)).unwrap();
        assert_eq!(peer2.consecutive_failures, 0);
        assert!(peer2.last_error.is_none());
    }

    // ── DIAG-04: last_sync_latency_ms tracks round-trip ─────────

    #[test]
    fn diag_04_latency_tracked() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());
        let mut diag = DiagnosticsLogic::new("test_state".into());

        diag.record_sync_success(NodeId(2), 42, clock.as_ref());

        let status = diag.sync_status(&shard);
        let peer2 = status.iter().find(|s| s.node_id == NodeId(2)).unwrap();
        assert_eq!(peer2.last_sync_latency_ms, Some(42));
    }

    // ── DIAG-05: last_error captures failure message ────────────

    #[test]
    fn diag_05_last_error_captured() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());
        let mut diag = DiagnosticsLogic::new("test_state".into());

        diag.record_sync_failure(NodeId(3), "connection timed out", clock.as_ref());

        let status = diag.sync_status(&shard);
        let peer3 = status.iter().find(|s| s.node_id == NodeId(3)).unwrap();
        assert_eq!(peer3.last_error.as_deref(), Some("connection timed out"));
    }

    // ── DIAG-06: gap_count increments on age gap ────────────────

    #[test]
    fn diag_06_gap_count_increments() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());
        let mut diag = DiagnosticsLogic::new("test_state".into());

        diag.record_gap_detected(NodeId(2));
        diag.record_gap_detected(NodeId(2));

        let status = diag.sync_status(&shard);
        let peer2 = status.iter().find(|s| s.node_id == NodeId(2)).unwrap();
        assert_eq!(peer2.gap_count, 2);
    }

    // ── DIAG-07: total_mutations counts mutations ───────────────

    #[test]
    fn diag_07_total_mutations() {
        let mut diag = DiagnosticsLogic::new("test_state".into());
        for _ in 0..5 {
            diag.record_mutation();
        }
        assert_eq!(diag.metrics().total_mutations, 5);
    }

    // ── DIAG-08: deltas_sent counts outbound deltas ─────────────

    #[test]
    fn diag_08_deltas_sent() {
        let mut diag = DiagnosticsLogic::new("test_state".into());
        diag.record_delta_sent();
        diag.record_delta_sent();
        diag.record_delta_sent();
        assert_eq!(diag.metrics().deltas_sent, 3);
    }

    // ── DIAG-09: deltas_suppressed counts suppressed ────────────

    #[test]
    fn diag_09_deltas_suppressed() {
        let mut diag = DiagnosticsLogic::new("test_state".into());
        diag.record_delta_suppressed();
        diag.record_delta_suppressed();
        assert_eq!(diag.metrics().deltas_suppressed, 2);
    }

    // ── DIAG-10: deltas_immediate counts immediate pushes ───────

    #[test]
    fn diag_10_deltas_immediate() {
        let mut diag = DiagnosticsLogic::new("test_state".into());
        diag.record_delta_immediate();
        diag.record_delta_immediate();
        assert_eq!(diag.metrics().deltas_immediate, 2);
    }

    // ── DIAG-11: age_gaps_detected counts gaps ──────────────────

    #[test]
    fn diag_11_age_gaps_detected() {
        let mut diag = DiagnosticsLogic::new("test_state".into());
        diag.record_gap_detected(NodeId(2));
        assert_eq!(diag.metrics().age_gaps_detected, 1);
    }

    // ── DIAG-12: stale_deltas_discarded counts discards ─────────

    #[test]
    fn diag_12_stale_deltas_discarded() {
        let mut diag = DiagnosticsLogic::new("test_state".into());
        diag.record_stale_delta_discarded();
        diag.record_stale_delta_discarded();
        assert_eq!(diag.metrics().stale_deltas_discarded, 2);
    }

    // ── DIAG-13: sync_failures counts all failure types ─────────

    #[test]
    fn diag_13_sync_failures() {
        let clock = test_clock();
        let mut diag = DiagnosticsLogic::new("test_state".into());

        diag.record_sync_failure(NodeId(2), "network", clock.as_ref());
        diag.record_sync_failure(NodeId(3), "serde", clock.as_ref());
        diag.record_sync_failure(NodeId(2), "timeout", clock.as_ref());

        assert_eq!(diag.metrics().sync_failures, 3);
    }

    // ── DIAG-14: stale_peers returns only stale ─────────────────

    #[test]
    fn diag_14_stale_peers_filtered() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_3_node_shard(clock.clone());

        // Advance time so all peers become stale
        tc.advance(Duration::from_secs(30));

        // Re-sync node 2 only
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(2),
            generation: Generation::new(100, 6),
            wire_version: 1,
            view: TestState { counter: 6, label: "node2".into() },
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        });

        // Node 3 is stale (30s old), node 2 is fresh
        let stale = shard.stale_peers(Duration::from_secs(10));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], NodeId(3));
    }

    // ── DIAG-15: returns empty when all fresh ───────────────────

    #[test]
    fn diag_15_all_fresh() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());

        let stale = shard.stale_peers(Duration::from_secs(10));
        // Node 2 and 3 have pending_remote_generation from on_node_joined
        // placeholder, but we accepted snapshots so they should be cleared...
        // Actually, on_node_joined sets pending, but accept_inbound_snapshot
        // clears it if incoming >= pending. Let's check:
        // pending was Some(Generation::new(1, 0)), incoming for node2 is (100, 5) > (1, 0) → cleared
        assert!(stale.is_empty(), "expected no stale peers, got {:?}", stale);
    }

    // ── DIAG-16: Healthy when all peers synced ──────────────────

    #[test]
    fn diag_16_healthy() {
        let clock = test_clock();
        let shard = make_3_node_shard(clock.clone());
        let diag = DiagnosticsLogic::new("test_state".into());

        let health = diag.health_status(&shard, Duration::from_secs(10));
        assert_eq!(health.status, HealthStatus::Healthy);
        assert_eq!(health.healthy_peers, 2);
        assert_eq!(health.stale_peers, 0);
    }

    // ── DIAG-17: Degraded when some peers stale ─────────────────

    #[test]
    fn diag_17_degraded() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());

        // 4-node cluster: self + 3 peers
        let shard = make_shard(clock.clone());
        shard.on_node_joined(NodeId(2), TestState { counter: 0, label: "".into() });
        shard.on_node_joined(NodeId(3), TestState { counter: 0, label: "".into() });
        shard.on_node_joined(NodeId(4), TestState { counter: 0, label: "".into() });

        let now_ms = clock.unix_ms();
        for id in [2, 3, 4] {
            shard.accept_inbound_snapshot(InboundSnapshot {
                source: NodeId(id),
                generation: Generation::new(id as u64 * 100, 1),
                wire_version: 1,
                view: TestState { counter: 1, label: format!("n{}", id) },
                created_time: now_ms,
                modified_time: now_ms,
            });
        }

        // Advance time to make all stale
        tc.advance(Duration::from_secs(30));

        // Refresh 2 of 3 peers
        for id in [2, 3] {
            shard.accept_inbound_snapshot(InboundSnapshot {
                source: NodeId(id),
                generation: Generation::new(id as u64 * 100, 2),
                wire_version: 1,
                view: TestState { counter: 2, label: format!("n{}", id) },
                created_time: clock.unix_ms(),
                modified_time: clock.unix_ms(),
            });
        }

        let diag = DiagnosticsLogic::new("test_state".into());
        let health = diag.health_status(&shard, Duration::from_secs(10));
        assert_eq!(health.status, HealthStatus::Degraded);
        assert_eq!(health.healthy_peers, 2);
        assert_eq!(health.stale_peers, 1);
    }

    // ── DIAG-18: Unhealthy when majority stale ──────────────────

    #[test]
    fn diag_18_unhealthy() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());
        let shard = make_3_node_shard(clock.clone());

        // Advance time — both peers become stale
        tc.advance(Duration::from_secs(30));

        let diag = DiagnosticsLogic::new("test_state".into());
        let health = diag.health_status(&shard, Duration::from_secs(10));
        assert_eq!(health.status, HealthStatus::Unhealthy);
        assert_eq!(health.stale_peers, 2);
    }

    // ── DIAG-19: Multiple state types reported ──────────────────

    #[test]
    fn diag_19_multiple_state_types() {
        let clock = test_clock();

        // State type 1: healthy
        let shard1 = make_3_node_shard(clock.clone());
        let diag1 = DiagnosticsLogic::new("state_a".into());
        let health1 = diag1.health_status(&shard1, Duration::from_secs(10));

        // State type 2: unhealthy (stale peers)
        let tc2 = TestClock::with_base_unix_ms(1_000_000);
        let clock2: Arc<dyn Clock> = Arc::new(tc2.clone());
        let shard2 = make_3_node_shard(clock2.clone());
        tc2.advance(Duration::from_secs(30));
        let diag2 = DiagnosticsLogic::new("state_b".into());
        let health2 = diag2.health_status(&shard2, Duration::from_secs(10));

        // Aggregate (simulating StateRegistry::health_check)
        let states = vec![health1, health2];
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].status, HealthStatus::Healthy);
        assert_eq!(states[0].state_name, "state_a");
        assert_eq!(states[1].status, HealthStatus::Unhealthy);
        assert_eq!(states[1].state_name, "state_b");
    }

    // ── DIAG-20: Sync success emits DEBUG log ───────────────────

    #[traced_test]
    #[test]
    fn diag_20_sync_success_debug_log() {
        let clock = test_clock();
        let mut diag = DiagnosticsLogic::new("node_resource".into());

        diag.record_sync_success(NodeId(2), 3, clock.as_ref());

        assert!(logs_contain("sync succeeded"));
        assert!(logs_contain("node_resource"));
        assert!(logs_contain("latency_ms"));
    }

    // ── DIAG-21: Sync failure emits WARN log ────────────────────

    #[traced_test]
    #[test]
    fn diag_21_sync_failure_warn_log() {
        let clock = test_clock();
        let mut diag = DiagnosticsLogic::new("node_resource".into());

        diag.record_sync_failure(NodeId(3), "connection timed out", clock.as_ref());

        assert!(logs_contain("sync failed"));
        assert!(logs_contain("node_resource"));
        assert!(logs_contain("connection timed out"));
        assert!(logs_contain("consecutive_failures"));
    }

    // ── DIAG-22: Delta suppression emits DEBUG log ──────────────

    #[traced_test]
    #[test]
    fn diag_22_delta_suppressed_debug_log() {
        let mut diag = DiagnosticsLogic::new("node_resource".into());

        diag.record_delta_suppressed();

        assert!(logs_contain("delta suppressed"));
        assert!(logs_contain("suppressed_count"));
    }

    // ── DIAG-23: Single-node cluster is Healthy ─────────────────

    #[test]
    fn diag_23_single_node_healthy() {
        let clock = test_clock();
        let shard = make_shard(clock.clone()); // only self, no peers
        let diag = DiagnosticsLogic::new("test_state".into());

        let health = diag.health_status(&shard, Duration::from_secs(10));
        assert_eq!(health.status, HealthStatus::Healthy);
        assert_eq!(health.healthy_peers, 0);
        assert_eq!(health.stale_peers, 0);
        assert_eq!(health.failing_peers, 0);
    }

    // ── DIAG-24: Disjoint stale and failing peers counted correctly ──

    #[test]
    fn diag_24_disjoint_stale_and_failing() {
        let tc = TestClock::with_base_unix_ms(1_000_000);
        let clock: Arc<dyn Clock> = Arc::new(tc.clone());

        // 5-node cluster: self + 4 peers
        let shard = make_shard(clock.clone());
        for id in [2, 3, 4, 5] {
            shard.on_node_joined(NodeId(id), TestState { counter: 0, label: "".into() });
        }
        let now_ms = clock.unix_ms();
        for id in [2, 3, 4, 5] {
            shard.accept_inbound_snapshot(InboundSnapshot {
                source: NodeId(id),
                generation: Generation::new(id as u64 * 100, 1),
                wire_version: 1,
                view: TestState { counter: 1, label: format!("n{}", id) },
                created_time: now_ms,
                modified_time: now_ms,
            });
        }

        // Advance time to make all stale
        tc.advance(Duration::from_secs(30));

        // Refresh nodes 2 and 3 (fresh), leave node 4 stale
        for id in [2, 3] {
            shard.accept_inbound_snapshot(InboundSnapshot {
                source: NodeId(id),
                generation: Generation::new(id as u64 * 100, 2),
                wire_version: 1,
                view: TestState { counter: 2, label: format!("n{}", id) },
                created_time: clock.unix_ms(),
                modified_time: clock.unix_ms(),
            });
        }

        // Node 5 is fresh but failing (record failure)
        let mut diag = DiagnosticsLogic::new("test_state".into());
        diag.record_sync_failure(NodeId(5), "connection refused", clock.as_ref());

        // Refresh node 5 view so it's NOT stale, but HAS consecutive_failures
        shard.accept_inbound_snapshot(InboundSnapshot {
            source: NodeId(5),
            generation: Generation::new(500, 2),
            wire_version: 1,
            view: TestState { counter: 2, label: "n5".into() },
            created_time: clock.unix_ms(),
            modified_time: clock.unix_ms(),
        });

        // Now: node 2 healthy, node 3 healthy, node 4 stale-only, node 5 failing-only
        // Total = 4, healthy = 2, 2 > 4/2 = 2 → FALSE → Unhealthy (strict majority)
        let health = diag.health_status(&shard, Duration::from_secs(10));
        assert_eq!(health.healthy_peers, 2);
        assert_eq!(health.stale_peers, 1);   // node 4
        assert_eq!(health.failing_peers, 1); // node 5
        assert_eq!(health.status, HealthStatus::Unhealthy);
    }

    // ── DIAG-25: remove_peer cleans up departed node ────────────

    #[test]
    fn diag_25_remove_peer() {
        let clock = test_clock();
        let mut diag = DiagnosticsLogic::new("test_state".into());

        diag.record_sync_failure(NodeId(2), "err", clock.as_ref());
        assert_eq!(diag.peers.len(), 1);

        diag.remove_peer(&NodeId(2));
        assert_eq!(diag.peers.len(), 0);
    }

    // ── DIAG-26: delta/snapshot received resets failures ─────────

    #[test]
    fn diag_26_inbound_resets_failures() {
        let clock = test_clock();
        let mut diag = DiagnosticsLogic::new("test_state".into());

        // Build up failures
        diag.record_sync_failure(NodeId(2), "err1", clock.as_ref());
        diag.record_sync_failure(NodeId(2), "err2", clock.as_ref());
        assert_eq!(diag.peers.get(&NodeId(2)).unwrap().consecutive_failures, 2);

        // Delta received clears failures
        diag.record_delta_received(NodeId(2), 5, clock.as_ref());
        assert_eq!(diag.peers.get(&NodeId(2)).unwrap().consecutive_failures, 0);
        assert_eq!(diag.metrics().deltas_received, 1);

        // Build up again
        diag.record_sync_failure(NodeId(2), "err3", clock.as_ref());
        assert_eq!(diag.peers.get(&NodeId(2)).unwrap().consecutive_failures, 1);

        // Snapshot received clears failures
        diag.record_snapshot_received(NodeId(2), 3, clock.as_ref());
        assert_eq!(diag.peers.get(&NodeId(2)).unwrap().consecutive_failures, 0);
        assert_eq!(diag.metrics().snapshots_received, 1);
    }
}
