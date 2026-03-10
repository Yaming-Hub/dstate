use std::collections::HashMap;

use crate::types::node::{Generation, NodeId};
use crate::types::sync_message::{BatchedChangeFeed, ChangeNotification};

/// Pure state machine for change feed notification batching and deduplication.
///
/// Collects `notify_change` calls from local state shards, deduplicates by
/// `(state_name, source_node)` keeping the highest `Generation`, and produces
/// a `BatchedChangeFeed` on `flush()`.
pub(crate) struct ChangeFeedLogic {
    node_id: NodeId,
    /// Pending notifications: key = (state_name, source_node), value = highest generation seen.
    pending: HashMap<(String, NodeId), Generation>,
}

impl ChangeFeedLogic {
    /// Create a new `ChangeFeedLogic` with the given node identity and an empty pending map.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            pending: HashMap::new(),
        }
    }

    /// Record a change notification. If an entry for the same `(state_name, source_node)`
    /// already exists, the higher `Generation` (by `Ord`) is kept.
    pub fn notify_change(
        &mut self,
        state_name: String,
        source_node: NodeId,
        generation: Generation,
    ) {
        let key = (state_name, source_node);
        self.pending
            .entry(key)
            .and_modify(|existing| {
                if generation > *existing {
                    *existing = generation;
                }
            })
            .or_insert(generation);
    }

    /// Drain all pending notifications into a [`BatchedChangeFeed`].
    ///
    /// Returns `None` when there is nothing to flush.
    pub fn flush(&mut self) -> Option<BatchedChangeFeed> {
        if self.pending.is_empty() {
            return None;
        }

        let notifications = self
            .pending
            .drain()
            .map(|((state_name, source_node), generation)| ChangeNotification {
                state_name,
                source_node,
                generation,
            })
            .collect();

        Some(BatchedChangeFeed {
            source_node: self.node_id,
            notifications,
        })
    }

    /// Number of distinct `(state_name, source_node)` entries waiting to be flushed.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Filter an inbound [`BatchedChangeFeed`] for local routing.
    ///
    /// Returns an empty `Vec` when `batch.source_node == self_node_id` (self-message).
    /// Otherwise returns one `(state_name, source_node, generation)` tuple per
    /// notification, ready to be dispatched to the appropriate `StateShard` via
    /// `mark_stale`.
    pub fn route_inbound_batch(
        batch: &BatchedChangeFeed,
        self_node_id: NodeId,
    ) -> Vec<(String, NodeId, Generation)> {
        if batch.source_node == self_node_id {
            return Vec::new();
        }

        batch
            .notifications
            .iter()
            .map(|n| (n.state_name.clone(), n.source_node, n.generation))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(incarnation: u64, age: u64) -> Generation {
        Generation::new(incarnation, age)
    }

    // CFA-01
    #[test]
    fn notify_dedup_keeps_highest() {
        let mut logic = ChangeFeedLogic::new(NodeId(1));

        logic.notify_change("counters".into(), NodeId(2), gen(1, 1));
        logic.notify_change("counters".into(), NodeId(2), gen(1, 2));
        logic.notify_change("counters".into(), NodeId(2), gen(1, 3));

        assert_eq!(logic.pending_count(), 1);
        assert_eq!(
            logic.pending[&("counters".to_string(), NodeId(2))],
            gen(1, 3),
        );
    }

    // CFA-02
    #[test]
    fn flush_sends_batched_feed() {
        let mut logic = ChangeFeedLogic::new(NodeId(1));

        logic.notify_change("counters".into(), NodeId(2), gen(1, 1));
        logic.notify_change("sessions".into(), NodeId(3), gen(2, 5));

        let batch = logic.flush().expect("should produce a batch");
        assert_eq!(batch.source_node, NodeId(1));
        assert_eq!(batch.notifications.len(), 2);
    }

    // CFA-03
    #[test]
    fn empty_pending_no_flush() {
        let mut logic = ChangeFeedLogic::new(NodeId(1));
        assert!(logic.flush().is_none());
    }

    // CFA-05
    #[test]
    fn self_message_filtered() {
        let batch = BatchedChangeFeed {
            source_node: NodeId(1),
            notifications: vec![ChangeNotification {
                state_name: "counters".into(),
                source_node: NodeId(1),
                generation: gen(1, 1),
            }],
        };

        let routed = ChangeFeedLogic::route_inbound_batch(&batch, NodeId(1));
        assert!(routed.is_empty());
    }

    // CFA-06
    #[test]
    fn cross_state_batching() {
        let mut logic = ChangeFeedLogic::new(NodeId(1));

        logic.notify_change("counters".into(), NodeId(2), gen(1, 1));
        logic.notify_change("sessions".into(), NodeId(2), gen(1, 1));

        let batch = logic.flush().expect("should produce a batch");
        assert_eq!(batch.source_node, NodeId(1));
        assert_eq!(batch.notifications.len(), 2);

        let names: Vec<&str> = batch
            .notifications
            .iter()
            .map(|n| n.state_name.as_str())
            .collect();
        assert!(names.contains(&"counters"));
        assert!(names.contains(&"sessions"));
    }

    // CFA-07
    #[test]
    fn higher_incarnation_wins_dedup() {
        let mut logic = ChangeFeedLogic::new(NodeId(1));

        logic.notify_change("counters".into(), NodeId(2), gen(100, 50));
        logic.notify_change("counters".into(), NodeId(2), gen(200, 1));

        assert_eq!(logic.pending_count(), 1);
        assert_eq!(
            logic.pending[&("counters".to_string(), NodeId(2))],
            gen(200, 1),
        );
    }

    #[test]
    fn flush_clears_pending() {
        let mut logic = ChangeFeedLogic::new(NodeId(1));

        logic.notify_change("counters".into(), NodeId(2), gen(1, 1));
        logic.notify_change("sessions".into(), NodeId(3), gen(2, 5));
        assert_eq!(logic.pending_count(), 2);

        let _ = logic.flush();
        assert_eq!(logic.pending_count(), 0);
    }

    #[test]
    fn route_inbound_batch_routes_correctly() {
        let batch = BatchedChangeFeed {
            source_node: NodeId(5),
            notifications: vec![
                ChangeNotification {
                    state_name: "counters".into(),
                    source_node: NodeId(2),
                    generation: gen(1, 10),
                },
                ChangeNotification {
                    state_name: "counters".into(),
                    source_node: NodeId(3),
                    generation: gen(2, 5),
                },
                ChangeNotification {
                    state_name: "sessions".into(),
                    source_node: NodeId(2),
                    generation: gen(1, 7),
                },
            ],
        };

        let routed = ChangeFeedLogic::route_inbound_batch(&batch, NodeId(1));
        assert_eq!(routed.len(), 3);

        assert!(routed.contains(&("counters".to_string(), NodeId(2), gen(1, 10))));
        assert!(routed.contains(&("counters".to_string(), NodeId(3), gen(2, 5))));
        assert!(routed.contains(&("sessions".to_string(), NodeId(2), gen(1, 7))));
    }

    // ── CFA-04: Inbound batch routes to correct shards ──────────
    // Verifies that a BatchedChangeFeed with notifications for different
    // state_names produces correctly separated routing tuples.

    #[test]
    fn inbound_batch_routes_to_correct_shards() {
        let batch = BatchedChangeFeed {
            source_node: NodeId(10),
            notifications: vec![
                ChangeNotification {
                    state_name: "counters".into(),
                    source_node: NodeId(2),
                    generation: gen(1, 20),
                },
                ChangeNotification {
                    state_name: "sessions".into(),
                    source_node: NodeId(2),
                    generation: gen(1, 15),
                },
            ],
        };

        let routed = ChangeFeedLogic::route_inbound_batch(&batch, NodeId(1));
        assert_eq!(routed.len(), 2);

        // Verify each notification maps to the correct (state_name, source_node, generation).
        let counter_entry = routed.iter().find(|(name, _, _)| name == "counters");
        let session_entry = routed.iter().find(|(name, _, _)| name == "sessions");

        let (cn, cs, cg) = counter_entry.expect("counters entry should be present");
        assert_eq!(cn, "counters");
        assert_eq!(*cs, NodeId(2));
        assert_eq!(*cg, gen(1, 20));

        let (sn, ss, sg) = session_entry.expect("sessions entry should be present");
        assert_eq!(sn, "sessions");
        assert_eq!(*ss, NodeId(2));
        assert_eq!(*sg, gen(1, 15));
    }

    // CFA-08/09: mark_stale via ShardCore is covered by shard_core::tests
    // (shard_07_mark_stale_sets_pending, shard_08_mark_stale_old_incarnation_noop,
    //  wire_change_feed_marks_peer_stale).
}
