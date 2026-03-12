use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dstate::{ClusterError, ClusterEvent, ClusterEvents, SubscriptionId};

type SubscriberMap = HashMap<SubscriptionId, Box<dyn Fn(ClusterEvent) + Send + Sync>>;

/// A dstate `ClusterEvents` implementation for the ractor adapter.
///
/// Provides a callback-based subscription system. In production, an
/// integration with `ractor_cluster` would feed real membership changes
/// into this subsystem via [`RactorClusterEvents::emit`].
#[derive(Clone)]
pub struct RactorClusterEvents {
    subscribers: Arc<Mutex<SubscriberMap>>,
    next_id: Arc<AtomicU64>,
}

impl RactorClusterEvents {
    /// Create a new cluster events subsystem.
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Emit a cluster event, notifying all subscribers.
    ///
    /// In production, this would be driven by `ractor_cluster` membership
    /// change notifications. For testing, call this directly.
    pub fn emit(&self, event: ClusterEvent) {
        let subs = self.subscribers.lock().unwrap();
        for sub in subs.values() {
            sub(event.clone());
        }
    }
}

impl Default for RactorClusterEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterEvents for RactorClusterEvents {
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
