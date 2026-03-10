use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;

use crate::traits::clock::Clock;
use crate::types::envelope::StateViewObject;
use crate::types::node::NodeId;

/// A concurrent view map using per-node `ArcSwap` entries for O(1) single-node
/// updates without cloning the entire map.
///
/// # Structure
///
/// ```text
/// ArcSwap< HashMap< NodeId, Arc<ArcSwap<StateViewObject<V>>> > >
///   ^                          ^
///   |                          |
///   outer: swapped only on     inner: swapped on every
///   node join/leave (rare)     snapshot/delta update (frequent)
/// ```
///
/// # Concurrency
///
/// - **Read (single node):** Two atomic loads — fully lock-free.
/// - **Read (all nodes):** One atomic load + N atomic loads — fully lock-free.
/// - **Write (update one node):** One atomic store into the per-node `ArcSwap`
///   — O(1), no map clone.
/// - **Write (node join/leave):** Clone the outer `HashMap` (cheap — only `Arc`
///   pointer bumps), insert/remove, then atomic swap of the outer `ArcSwap`.
#[allow(dead_code)]
pub(crate) struct ViewMap<V>
where
    V: Clone + Send + Sync + Debug + 'static,
{
    inner: ArcSwap<HashMap<NodeId, Arc<ArcSwap<StateViewObject<V>>>>>,
}

#[allow(dead_code)]
impl<V> ViewMap<V>
where
    V: Clone + Send + Sync + Debug + 'static,
{
    /// Create a new `ViewMap` with a single entry for the local node.
    pub fn new(node_id: NodeId, initial_view: StateViewObject<V>) -> Self {
        let mut map = HashMap::new();
        map.insert(node_id, Arc::new(ArcSwap::from_pointee(initial_view)));
        Self {
            inner: ArcSwap::from_pointee(map),
        }
    }

    /// Read a single node's view. Fully lock-free (two atomic loads).
    ///
    /// Returns `None` if the node has no entry.
    pub fn get(&self, node_id: &NodeId) -> Option<Arc<StateViewObject<V>>> {
        let map = self.inner.load();
        map.get(node_id).map(|entry| entry.load_full())
    }

    /// Update an existing node's view. O(1) — just an atomic swap on the
    /// per-node `ArcSwap`. No map cloning.
    ///
    /// Returns `false` if the node has no entry (caller should use
    /// `insert_node` for new nodes).
    pub fn update(&self, node_id: &NodeId, view: StateViewObject<V>) -> bool {
        let map = self.inner.load();
        match map.get(node_id) {
            Some(entry) => {
                entry.store(Arc::new(view));
                true
            }
            None => false,
        }
    }

    /// Insert a new node into the view map. Clones the outer map (cheap —
    /// only `Arc` pointer bumps) and atomically swaps it in.
    ///
    /// Returns `false` if the node already existed (no update performed).
    pub fn insert_node(&self, node_id: NodeId, view: StateViewObject<V>) -> bool {
        let map = self.inner.load();
        if map.contains_key(&node_id) {
            return false;
        }
        let mut new_map = (**map).clone();
        new_map.insert(node_id, Arc::new(ArcSwap::from_pointee(view)));
        self.inner.store(Arc::new(new_map));
        true
    }

    /// Insert or update a node's view. If the node exists, updates in-place
    /// (O(1)). If not, inserts a new entry (clones outer map).
    pub fn upsert(&self, node_id: NodeId, view: StateViewObject<V>) {
        let map = self.inner.load();
        match map.get(&node_id) {
            Some(entry) => {
                entry.store(Arc::new(view));
            }
            None => {
                let mut new_map = (**map).clone();
                new_map.insert(node_id, Arc::new(ArcSwap::from_pointee(view)));
                self.inner.store(Arc::new(new_map));
            }
        }
    }

    /// Remove a node from the view map. Clones the outer map and atomically
    /// swaps it in.
    ///
    /// Returns `true` if the node was present and removed.
    pub fn remove_node(&self, node_id: &NodeId) -> bool {
        let map = self.inner.load();
        if !map.contains_key(node_id) {
            return false;
        }
        let mut new_map = (**map).clone();
        new_map.remove(node_id);
        self.inner.store(Arc::new(new_map));
        true
    }

    /// Check whether a node has an entry.
    pub fn contains(&self, node_id: &NodeId) -> bool {
        self.inner.load().contains_key(node_id)
    }

    /// Number of nodes in the view map.
    pub fn len(&self) -> usize {
        self.inner.load().len()
    }

    /// Collect a full snapshot of all views. Lock-free but O(n) — use
    /// `get()` for single-node reads when possible.
    pub fn snapshot(&self) -> HashMap<NodeId, StateViewObject<V>> {
        let map = self.inner.load();
        map.iter()
            .map(|(k, v)| (*k, (*v.load_full()).clone()))
            .collect()
    }

    /// Collect all node IDs currently in the map.
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.inner.load().keys().copied().collect()
    }

    /// Return peers whose views are stale, excluding the local node.
    ///
    /// A view is stale if:
    /// - `pending_remote_generation` is set, OR
    /// - `synced_at` is older than `max_staleness`
    pub fn stale_peers(
        &self,
        local_node: NodeId,
        max_staleness: Duration,
        clock: &dyn Clock,
    ) -> Vec<NodeId> {
        let map = self.inner.load();
        let now = clock.now();

        map.iter()
            .filter(|(id, _)| **id != local_node)
            .filter(|(_, entry)| {
                let vo = entry.load();
                vo.pending_remote_generation.is_some()
                    || now.duration_since(vo.synced_at) > max_staleness
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Conditionally update a node's view using a closure that receives
    /// the current view (if any) and returns the new view (or `None` to
    /// skip the update).
    ///
    /// This is useful for compare-and-swap style updates (e.g.,
    /// `should_accept` checks).
    pub fn update_if<F>(&self, node_id: &NodeId, f: F) -> bool
    where
        F: FnOnce(Option<&StateViewObject<V>>) -> Option<StateViewObject<V>>,
    {
        let map = self.inner.load();
        match map.get(node_id) {
            Some(entry) => {
                let current = entry.load();
                match f(Some(&current)) {
                    Some(new_view) => {
                        entry.store(Arc::new(new_view));
                        true
                    }
                    None => false,
                }
            }
            None => {
                // Node not in map — call f with None, and if it returns
                // a view, insert a new entry.
                match f(None) {
                    Some(new_view) => {
                        let mut new_map = (**map).clone();
                        new_map.insert(
                            *node_id,
                            Arc::new(ArcSwap::from_pointee(new_view)),
                        );
                        self.inner.store(Arc::new(new_map));
                        true
                    }
                    None => false,
                }
            }
        }
    }
}
