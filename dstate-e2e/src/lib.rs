pub mod cluster;
pub mod interceptor;
pub mod scenarios;
pub mod transport;

use dstate::{ActorRuntime, ClusterEvent};

/// Extension trait for ActorRuntime implementations that can be used in E2E tests.
pub trait TestableRuntime: ActorRuntime + Clone + 'static {
    fn new_for_testing() -> Self;
    fn emit_cluster_event(&self, event: ClusterEvent);
}

// ── TestableRuntime impls (publish = false, so no orphan issues) ─────

impl TestableRuntime for dstate_ractor::RactorRuntime {
    fn new_for_testing() -> Self {
        dstate_ractor::RactorRuntime::new()
    }
    fn emit_cluster_event(&self, event: ClusterEvent) {
        self.cluster_events_handle().emit(event);
    }
}

impl TestableRuntime for dstate_kameo::KameoRuntime {
    fn new_for_testing() -> Self {
        dstate_kameo::KameoRuntime::new()
    }
    fn emit_cluster_event(&self, event: ClusterEvent) {
        self.cluster_events_handle().emit(event);
    }
}
