//! Node Resource Monitor — dstate `DeltaDistributedState` example.
//!
//! Demonstrates a realistic distributed state type for monitoring cluster node
//! resources (CPU, memory, disk). Shows:
//!
//! - **State vs view separation**: `sample_count` and `cpu_accumulator` are
//!   internal bookkeeping (not `pub`); only aggregate metrics appear in the view.
//! - **Delta synchronization**: Only changed resource metrics are sent to peers.
//! - **Dynamic sync urgency**: Large memory spikes → `Immediate`; small CPU
//!   jitter → `Suppress`; moderate changes → `Default`.
//! - **Cross-node queries**: `find_lowest_memory_node()` reads local view map.

use dstate::{DeltaDistributedState, DeserializeError, SyncUrgency};
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Types ──────────────────────────────────────────────────────────────

/// Internal state with private bookkeeping fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResourceState {
    // Not included in the replicated view:
    sample_count: u64,
    cpu_accumulator: f64,
    // Public metrics:
    pub cpu_usage_pct: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_total_bytes: u64,
}

impl Default for NodeResourceState {
    fn default() -> Self {
        Self {
            sample_count: 0,
            cpu_accumulator: 0.0,
            cpu_usage_pct: 0.0,
            memory_used_bytes: 0,
            memory_total_bytes: 8 * 1024 * 1024 * 1024, // 8 GB default
            disk_used_bytes: 0,
            disk_total_bytes: 100 * 1024 * 1024 * 1024, // 100 GB default
        }
    }
}

impl NodeResourceState {
    /// Record a CPU sample and update the running average.
    pub fn record_cpu_sample(&mut self, cpu_pct: f64) {
        self.sample_count += 1;
        self.cpu_accumulator += cpu_pct;
        self.cpu_usage_pct = self.cpu_accumulator / self.sample_count as f64;
    }

    /// Update memory usage.
    pub fn set_memory(&mut self, used: u64, total: u64) {
        self.memory_used_bytes = used;
        self.memory_total_bytes = total;
    }

    /// Update disk usage.
    pub fn set_disk(&mut self, used: u64, total: u64) {
        self.disk_used_bytes = used;
        self.disk_total_bytes = total;
    }

    pub fn memory_usage_pct(&self) -> f64 {
        if self.memory_total_bytes == 0 {
            return 0.0;
        }
        self.memory_used_bytes as f64 / self.memory_total_bytes as f64 * 100.0
    }
}

/// Public view visible to all peers (no private fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeResourceView {
    pub cpu_usage_pct: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_total_bytes: u64,
}

impl Default for NodeResourceView {
    fn default() -> Self {
        Self {
            cpu_usage_pct: 0.0,
            memory_used_bytes: 0,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            disk_used_bytes: 0,
            disk_total_bytes: 100 * 1024 * 1024 * 1024,
        }
    }
}

impl NodeResourceView {
    pub fn memory_usage_pct(&self) -> f64 {
        if self.memory_total_bytes == 0 {
            return 0.0;
        }
        self.memory_used_bytes as f64 / self.memory_total_bytes as f64 * 100.0
    }
}

impl fmt::Display for NodeResourceView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CPU: {:.1}%, Mem: {}/{} ({:.1}%), Disk: {}/{}",
            self.cpu_usage_pct,
            self.memory_used_bytes / (1024 * 1024),
            self.memory_total_bytes / (1024 * 1024),
            self.memory_usage_pct(),
            self.disk_used_bytes / (1024 * 1024),
            self.disk_total_bytes / (1024 * 1024),
        )
    }
}

/// Incremental view update sent to peers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeResourceDelta {
    pub cpu_usage_pct: Option<f64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
}

/// Application-level change description (what the mutation changed).
#[derive(Debug, Clone)]
pub enum NodeResourceChange {
    CpuSample { new_cpu_usage_pct: f64 },
    MemoryUpdate { used: u64, total: u64 },
    DiskUpdate { used: u64, total: u64 },
}

// ── Urgency thresholds ─────────────────────────────────────────────────

const MEMORY_SPIKE_THRESHOLD_PCT: f64 = 10.0;
const CPU_SUPPRESS_THRESHOLD_PCT: f64 = 2.0;

// ── DeltaDistributedState impl ─────────────────────────────────────────

pub struct NodeResource;

impl DeltaDistributedState for NodeResource {
    type State = NodeResourceState;
    type View = NodeResourceView;
    type ViewDelta = NodeResourceDelta;
    type StateDeltaChange = NodeResourceChange;

    fn name() -> &'static str {
        "node_resource"
    }

    const WIRE_VERSION: u32 = 1;
    const STORAGE_VERSION: u32 = 1;

    fn project_view(state: &Self::State) -> Self::View {
        NodeResourceView {
            cpu_usage_pct: state.cpu_usage_pct,
            memory_used_bytes: state.memory_used_bytes,
            memory_total_bytes: state.memory_total_bytes,
            disk_used_bytes: state.disk_used_bytes,
            disk_total_bytes: state.disk_total_bytes,
        }
    }

    fn project_delta(change: &Self::StateDeltaChange) -> Self::ViewDelta {
        match change {
            NodeResourceChange::CpuSample { new_cpu_usage_pct } => NodeResourceDelta {
                cpu_usage_pct: Some(*new_cpu_usage_pct),
                memory_used_bytes: None,
                memory_total_bytes: None,
                disk_used_bytes: None,
                disk_total_bytes: None,
            },
            NodeResourceChange::MemoryUpdate { used, total } => NodeResourceDelta {
                cpu_usage_pct: None,
                memory_used_bytes: Some(*used),
                memory_total_bytes: Some(*total),
                disk_used_bytes: None,
                disk_total_bytes: None,
            },
            NodeResourceChange::DiskUpdate { used, total } => NodeResourceDelta {
                cpu_usage_pct: None,
                memory_used_bytes: None,
                memory_total_bytes: None,
                disk_used_bytes: Some(*used),
                disk_total_bytes: Some(*total),
            },
        }
    }

    fn apply_delta(view: &Self::View, delta: &Self::ViewDelta) -> Self::View {
        NodeResourceView {
            cpu_usage_pct: delta.cpu_usage_pct.unwrap_or(view.cpu_usage_pct),
            memory_used_bytes: delta.memory_used_bytes.unwrap_or(view.memory_used_bytes),
            memory_total_bytes: delta
                .memory_total_bytes
                .unwrap_or(view.memory_total_bytes),
            disk_used_bytes: delta.disk_used_bytes.unwrap_or(view.disk_used_bytes),
            disk_total_bytes: delta.disk_total_bytes.unwrap_or(view.disk_total_bytes),
        }
    }

    fn sync_urgency(
        old_view: &Self::View,
        new_view: &Self::View,
        change: &Self::StateDeltaChange,
    ) -> SyncUrgency {
        match change {
            NodeResourceChange::MemoryUpdate { .. } => {
                let old_pct = old_view.memory_usage_pct();
                let new_pct = new_view.memory_usage_pct();
                let diff = (new_pct - old_pct).abs();
                if diff >= MEMORY_SPIKE_THRESHOLD_PCT {
                    SyncUrgency::Immediate
                } else {
                    SyncUrgency::Default
                }
            }
            NodeResourceChange::CpuSample { .. } => {
                let diff = (new_view.cpu_usage_pct - old_view.cpu_usage_pct).abs();
                if diff < CPU_SUPPRESS_THRESHOLD_PCT {
                    SyncUrgency::Suppress
                } else {
                    SyncUrgency::Default
                }
            }
            NodeResourceChange::DiskUpdate { .. } => SyncUrgency::Default,
        }
    }

    fn serialize_view(view: &Self::View) -> Vec<u8> {
        bincode::serialize(view).expect("view serialization should not fail")
    }

    fn deserialize_view(bytes: &[u8], wire_version: u32) -> Result<Self::View, DeserializeError> {
        if wire_version != Self::WIRE_VERSION {
            return Err(DeserializeError::unknown_version(wire_version));
        }
        bincode::deserialize(bytes)
            .map_err(|e| DeserializeError::Malformed(e.to_string()))
    }

    fn serialize_delta(delta: &Self::ViewDelta) -> Vec<u8> {
        bincode::serialize(delta).expect("delta serialization should not fail")
    }

    fn deserialize_delta(
        bytes: &[u8],
        wire_version: u32,
    ) -> Result<Self::ViewDelta, DeserializeError> {
        if wire_version != Self::WIRE_VERSION {
            return Err(DeserializeError::unknown_version(wire_version));
        }
        bincode::deserialize(bytes)
            .map_err(|e| DeserializeError::Malformed(e.to_string()))
    }

    fn serialize_state(state: &Self::State) -> Vec<u8> {
        bincode::serialize(state).expect("state serialization should not fail")
    }

    fn deserialize_state(
        bytes: &[u8],
        storage_version: u32,
    ) -> Result<Self::State, DeserializeError> {
        if storage_version != Self::STORAGE_VERSION {
            return Err(DeserializeError::unknown_version(storage_version));
        }
        bincode::deserialize(bytes)
            .map_err(|e| DeserializeError::Malformed(e.to_string()))
    }
}

// ── Query helpers ──────────────────────────────────────────────────────

/// Find the node with the lowest memory usage across the cluster view map.
pub fn find_lowest_memory_node(
    views: &std::collections::HashMap<dstate::NodeId, dstate::StateViewObject<NodeResourceView>>,
) -> Option<dstate::NodeId> {
    views
        .iter()
        .min_by(|a, b| {
            a.1.value
                .memory_used_bytes
                .cmp(&b.1.value.memory_used_bytes)
        })
        .map(|(id, _)| id.clone())
}

fn main() {
    println!("NodeResource distributed state type — see integration tests for usage.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_view_hides_private_fields() {
        let mut state = NodeResourceState::default();
        state.record_cpu_sample(50.0);
        state.record_cpu_sample(70.0);
        let view = NodeResource::project_view(&state);

        assert_eq!(view.cpu_usage_pct, 60.0);
        // sample_count and cpu_accumulator are NOT in the view
        assert_eq!(state.sample_count, 2);
        assert_eq!(state.cpu_accumulator, 120.0);
    }

    #[test]
    fn apply_delta_memory_update() {
        let view = NodeResourceView::default();
        let delta = NodeResourceDelta {
            cpu_usage_pct: None,
            memory_used_bytes: Some(4 * 1024 * 1024 * 1024),
            memory_total_bytes: Some(8 * 1024 * 1024 * 1024),
            disk_used_bytes: None,
            disk_total_bytes: None,
        };
        let new_view = NodeResource::apply_delta(&view, &delta);
        assert_eq!(new_view.memory_used_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(new_view.cpu_usage_pct, 0.0); // unchanged
    }

    #[test]
    fn urgency_memory_spike_immediate() {
        let old = NodeResourceView {
            memory_used_bytes: 1 * 1024 * 1024 * 1024,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        let new = NodeResourceView {
            memory_used_bytes: 3 * 1024 * 1024 * 1024, // 12.5% → 37.5% = 25% spike
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        let change = NodeResourceChange::MemoryUpdate {
            used: 3 * 1024 * 1024 * 1024,
            total: 8 * 1024 * 1024 * 1024,
        };
        assert!(matches!(
            NodeResource::sync_urgency(&old, &new, &change),
            SyncUrgency::Immediate
        ));
    }

    #[test]
    fn urgency_small_cpu_suppressed() {
        let old = NodeResourceView {
            cpu_usage_pct: 50.0,
            ..Default::default()
        };
        let new = NodeResourceView {
            cpu_usage_pct: 50.5,
            ..Default::default()
        };
        let change = NodeResourceChange::CpuSample { new_cpu_usage_pct: 50.5 };
        assert!(matches!(
            NodeResource::sync_urgency(&old, &new, &change),
            SyncUrgency::Suppress
        ));
    }

    #[test]
    fn find_lowest_memory() {
        use dstate::{Generation, NodeId, StateViewObject};
        use std::collections::HashMap;
        use std::time::Instant;

        let now = Instant::now();
        let make_view = |node: &str, mem: u64| {
            let id = NodeId(node.into());
            let vo = StateViewObject {
                generation: Generation::new(1, 0),
                wire_version: 1,
                value: NodeResourceView {
                    memory_used_bytes: mem,
                    ..Default::default()
                },
                created_time: 0,
                modified_time: 0,
                synced_at: now,
                pending_remote_generation: None,
                source_node: id.clone(),
            };
            (id, vo)
        };

        let mut views = HashMap::new();
        let (id, vo) = make_view("node-0", 5_000_000_000);
        views.insert(id, vo);
        let (id, vo) = make_view("node-1", 2_000_000_000);
        views.insert(id, vo);
        let (id, vo) = make_view("node-2", 7_000_000_000);
        views.insert(id, vo);

        assert_eq!(
            find_lowest_memory_node(&views),
            Some(NodeId("node-1".into()))
        );
    }
}
