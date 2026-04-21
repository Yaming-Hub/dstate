use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::traits::runtime::{
    ClusterError, ClusterEvent, ClusterEvents, SubscriptionId, TimerHandle,
};

// ---------------------------------------------------------------------------
// TestClusterEvents
// ---------------------------------------------------------------------------

type ClusterSubscriberMap = HashMap<SubscriptionId, Box<dyn Fn(ClusterEvent) + Send + Sync>>;

/// Test cluster events that can be triggered manually.
///
/// Use this in integration tests to simulate cluster membership changes
/// without a real actor framework.
#[derive(Clone)]
pub struct TestClusterEvents {
    subscribers: Arc<Mutex<ClusterSubscriberMap>>,
    next_id: Arc<AtomicU64>,
}

impl TestClusterEvents {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Simulate a cluster event, notifying all subscribers.
    pub fn emit(&self, event: ClusterEvent) {
        let subs = self.subscribers.lock().unwrap();
        for sub in subs.values() {
            sub(event.clone());
        }
    }
}

impl Default for TestClusterEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterEvents for TestClusterEvents {
    fn subscribe(
        &self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> Result<SubscriptionId, ClusterError> {
        let id = SubscriptionId::from_raw(self.next_id.fetch_add(1, Ordering::SeqCst));
        let mut subs = self.subscribers.lock().unwrap();
        subs.insert(id, on_event);
        Ok(id)
    }

    fn unsubscribe(&self, id: SubscriptionId) -> Result<(), ClusterError> {
        let mut subs = self.subscribers.lock().unwrap();
        subs.remove(&id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TestTimerHandle
// ---------------------------------------------------------------------------

/// Test timer handle backed by a tokio JoinHandle.
pub struct TestTimerHandle {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl TestTimerHandle {
    /// Create a new `TestTimerHandle` wrapping a tokio task.
    pub fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }
}

impl TimerHandle for TestTimerHandle {
    fn cancel(mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}