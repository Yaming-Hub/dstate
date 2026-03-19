use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dstate::NodeId;
use tokio::sync::mpsc;

use crate::interceptor::{InterceptResult, Interceptor};

/// In-process byte-level transport routing between test nodes via tokio mpsc channels.
pub struct InProcessTransport {
    senders: Arc<Mutex<HashMap<NodeId, mpsc::UnboundedSender<Vec<u8>>>>>,
    interceptors: Arc<Mutex<Vec<Box<dyn Interceptor>>>>,
}

impl InProcessTransport {
    pub fn new() -> Self {
        Self {
            senders: Arc::new(Mutex::new(HashMap::new())),
            interceptors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a node's inbound channel.
    pub fn register(&self, node_id: NodeId, sender: mpsc::UnboundedSender<Vec<u8>>) {
        self.senders.lock().unwrap().insert(node_id, sender);
    }

    /// Remove a node from the transport.
    pub fn unregister(&self, node_id: NodeId) {
        self.senders.lock().unwrap().remove(&node_id);
    }

    /// Route a message through interceptors, then deliver to the target node.
    pub fn send(&self, from: NodeId, to: NodeId, data: Vec<u8>) {
        let interceptors = self.interceptors.lock().unwrap();
        let mut current_data = data;

        for interceptor in interceptors.iter() {
            match interceptor.intercept(from, to, current_data) {
                InterceptResult::Deliver(d) => {
                    current_data = d;
                }
                InterceptResult::Drop => return,
                InterceptResult::Delay(d, duration) => {
                    let senders = Arc::clone(&self.senders);
                    tokio::spawn(async move {
                        tokio::time::sleep(duration).await;
                        let senders = senders.lock().unwrap();
                        if let Some(sender) = senders.get(&to) {
                            let _ = sender.send(d);
                        }
                    });
                    return;
                }
            }
        }

        let senders = self.senders.lock().unwrap();
        if let Some(sender) = senders.get(&to) {
            let _ = sender.send(current_data);
        }
    }

    /// Broadcast a message to all registered nodes except the sender.
    pub fn broadcast(&self, from: NodeId, data: Vec<u8>) {
        let node_ids: Vec<NodeId> = {
            let senders = self.senders.lock().unwrap();
            senders.keys().copied().collect()
        };
        for to in node_ids {
            if to != from {
                self.send(from, to, data.clone());
            }
        }
    }

    /// Add an interceptor to the pipeline.
    pub fn add_interceptor(&self, interceptor: Box<dyn Interceptor>) {
        self.interceptors.lock().unwrap().push(interceptor);
    }

    /// Remove all interceptors.
    pub fn clear_interceptors(&self) {
        self.interceptors.lock().unwrap().clear();
    }
}
