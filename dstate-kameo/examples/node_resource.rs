//! Node Resource Monitor — kameo adapter example.
//!
//! Same scenario as `node_resource_ractor` but using `KameoRuntime`.
//! Demonstrates that switching actor frameworks requires only changing
//! the runtime type — all dstate traits and logic remain identical.
//!
//! Run with: `cargo run -p dstate-kameo --example node_resource`

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dstate::{
    ActorRef, ActorRuntime, DeltaDistributedState, DeserializeError, StateConfig, SyncStrategy,
    SyncUrgency, TimerHandle,
};
use dstate_kameo::KameoRuntime;

// ---------------------------------------------------------------------------
// State definitions (identical to the ractor example)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct NodeResourceState {
    cpu_percent: f64,
    memory_used_mb: u64,
    memory_total_mb: u64,
    sample_count: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct NodeResourceView {
    cpu_percent: f64,
    memory_used_mb: u64,
    memory_total_mb: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct NodeResourceDelta {
    cpu_percent: Option<f64>,
    memory_used_mb: Option<u64>,
}

#[derive(Clone, Debug)]
enum ResourceChange {
    CpuUpdated { old: f64, new: f64 },
    MemoryUpdated { old: u64, new: u64 },
    FullRefresh,
}

// ---------------------------------------------------------------------------
// DeltaDistributedState implementation (identical to the ractor example)
// ---------------------------------------------------------------------------

struct NodeResource;

impl DeltaDistributedState for NodeResource {
    type State = NodeResourceState;
    type View = NodeResourceView;
    type ViewDelta = NodeResourceDelta;
    type StateDeltaChange = ResourceChange;

    fn name() -> &'static str {
        "node_resource"
    }

    const WIRE_VERSION: u32 = 1;
    const STORAGE_VERSION: u32 = 1;

    fn project_view(state: &NodeResourceState) -> NodeResourceView {
        NodeResourceView {
            cpu_percent: state.cpu_percent,
            memory_used_mb: state.memory_used_mb,
            memory_total_mb: state.memory_total_mb,
        }
    }

    fn project_delta(change: &ResourceChange) -> NodeResourceDelta {
        match change {
            ResourceChange::CpuUpdated { new, .. } => NodeResourceDelta {
                cpu_percent: Some(*new),
                memory_used_mb: None,
            },
            ResourceChange::MemoryUpdated { new, .. } => NodeResourceDelta {
                cpu_percent: None,
                memory_used_mb: Some(*new),
            },
            ResourceChange::FullRefresh => NodeResourceDelta {
                cpu_percent: None,
                memory_used_mb: None,
            },
        }
    }

    fn apply_delta(view: &NodeResourceView, delta: &NodeResourceDelta) -> NodeResourceView {
        NodeResourceView {
            cpu_percent: delta.cpu_percent.unwrap_or(view.cpu_percent),
            memory_used_mb: delta.memory_used_mb.unwrap_or(view.memory_used_mb),
            memory_total_mb: view.memory_total_mb,
        }
    }

    fn sync_urgency(
        old_view: &NodeResourceView,
        new_view: &NodeResourceView,
        _change: &ResourceChange,
    ) -> SyncUrgency {
        let old_pct = old_view.memory_used_mb as f64 / old_view.memory_total_mb.max(1) as f64;
        let new_pct = new_view.memory_used_mb as f64 / new_view.memory_total_mb.max(1) as f64;

        if (new_pct - old_pct).abs() > 0.10 {
            SyncUrgency::Immediate
        } else {
            SyncUrgency::Default
        }
    }

    fn serialize_view(view: &NodeResourceView) -> Vec<u8> {
        bincode::serialize(view).unwrap()
    }

    fn deserialize_view(
        bytes: &[u8],
        _wire_version: u32,
    ) -> Result<NodeResourceView, DeserializeError> {
        bincode::deserialize(bytes).map_err(|e| DeserializeError::Malformed(e.to_string()))
    }

    fn serialize_delta(delta: &NodeResourceDelta) -> Vec<u8> {
        bincode::serialize(delta).unwrap()
    }

    fn deserialize_delta(
        bytes: &[u8],
        _wire_version: u32,
    ) -> Result<NodeResourceDelta, DeserializeError> {
        bincode::deserialize(bytes).map_err(|e| DeserializeError::Malformed(e.to_string()))
    }

    fn serialize_state(state: &NodeResourceState) -> Vec<u8> {
        let view = Self::project_view(state);
        bincode::serialize(&view).unwrap()
    }

    fn deserialize_state(
        bytes: &[u8],
        _storage_version: u32,
    ) -> Result<NodeResourceState, DeserializeError> {
        let view: NodeResourceView =
            bincode::deserialize(bytes).map_err(|e| DeserializeError::Malformed(e.to_string()))?;
        Ok(NodeResourceState {
            cpu_percent: view.cpu_percent,
            memory_used_mb: view.memory_used_mb,
            memory_total_mb: view.memory_total_mb,
            sample_count: 0,
        })
    }
}

// ---------------------------------------------------------------------------
// Actor messages
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum MonitorMsg {
    Sample { cpu: f64, memory_mb: u64 },
    PrintStatus,
}

// ---------------------------------------------------------------------------
// Main — same logic as ractor, but using KameoRuntime
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let runtime = KameoRuntime::new();

    let _config = StateConfig {
        sync_strategy: SyncStrategy::feed_with_periodic_sync(Duration::from_secs(30)),
        ..StateConfig::default()
    };

    println!("=== dstate Node Resource Monitor (kameo) ===\n");
    println!(
        "State: {} (wire v{}, storage v{})",
        NodeResource::name(),
        NodeResource::WIRE_VERSION,
        NodeResource::STORAGE_VERSION,
    );

    let sample_count = Arc::new(AtomicU64::new(0));
    let count = sample_count.clone();

    let monitor = runtime.spawn("resource_monitor", move |msg: MonitorMsg| {
        match msg {
            MonitorMsg::Sample { cpu, memory_mb } => {
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let state = NodeResourceState {
                    cpu_percent: cpu,
                    memory_used_mb: memory_mb,
                    memory_total_mb: 16384,
                    sample_count: n,
                };
                let view = NodeResource::project_view(&state);
                let urgency = NodeResource::sync_urgency(
                    &NodeResourceView {
                        cpu_percent: 0.0,
                        memory_used_mb: 0,
                        memory_total_mb: 16384,
                    },
                    &view,
                    &ResourceChange::FullRefresh,
                );
                println!(
                    "  Sample #{n}: CPU={:.1}%, Mem={}/{}MB, Urgency={:?}",
                    view.cpu_percent, view.memory_used_mb, view.memory_total_mb, urgency,
                );
            }
            MonitorMsg::PrintStatus => {
                println!(
                    "  Status: {} samples collected",
                    count.load(Ordering::SeqCst)
                );
            }
        }
    });

    // Demonstrate processing groups: join a "monitors" group
    runtime.join_group("monitors", &monitor).unwrap();
    let members = runtime.get_group_members::<MonitorMsg>("monitors").unwrap();
    println!("\nMonitor group has {} member(s)", members.len());

    // Periodic sampling
    println!("\nStarting periodic sampling (200ms interval)...");
    let timer = runtime.send_interval(
        &monitor,
        Duration::from_millis(200),
        MonitorMsg::Sample {
            cpu: 45.2,
            memory_mb: 8192,
        },
    );

    tokio::time::sleep(Duration::from_millis(700)).await;

    println!("\nCancelling periodic sampler...");
    timer.cancel();

    // One-shot alert
    println!("Scheduling high-memory alert in 100ms...");
    let _alert = runtime.send_after(
        &monitor,
        Duration::from_millis(100),
        MonitorMsg::Sample {
            cpu: 92.5,
            memory_mb: 14000,
        },
    );

    tokio::time::sleep(Duration::from_millis(200)).await;

    monitor.send(MonitorMsg::PrintStatus).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Demonstrate delta round-trip
    println!("\n--- Delta serialization round-trip ---");
    let delta = NodeResource::project_delta(&ResourceChange::MemoryUpdated {
        old: 8192,
        new: 14000,
    });
    let bytes = NodeResource::serialize_delta(&delta);
    let decoded = NodeResource::deserialize_delta(&bytes, 1).unwrap();
    println!("  Original delta: {delta:?}");
    println!("  Decoded delta:  {decoded:?}");
    println!("  Bytes: {} bytes", bytes.len());

    println!("\n=== Done ===");
}
