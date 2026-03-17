//! Mock transport for routing serialized messages between nodes.
//!
//! All inter-node messages pass through byte-level serialization to enforce
//! the same wire boundary as real network transports.

use std::collections::VecDeque;

use dstate::engine::WireMessage;
use dstate::NodeId;

use crate::interceptor::{InterceptAction, InterceptContext, NetworkInterceptor};

/// A message in transit between nodes.
#[derive(Debug, Clone)]
struct InFlightMessage {
    from: NodeId,
    to: NodeId,
    data: Vec<u8>,
    deliver_at_tick: u64,
}

/// Mock transport that routes serialized messages between nodes.
///
/// Messages are serialized to bytes via bincode before entering the
/// transport, then deserialized on the receiving side. This enforces
/// the same wire boundary as real network transports.
///
/// Interceptors can be added to the pipeline to simulate network faults.
pub struct MockTransport {
    in_flight: VecDeque<InFlightMessage>,
    interceptors: Vec<Box<dyn NetworkInterceptor>>,
    message_counter: u64,
    current_tick: u64,
    rng_seed: u64,
}

/// A message delivered to a node after deserialization.
#[derive(Debug)]
pub struct DeliveredMessage {
    pub from: NodeId,
    pub to: NodeId,
    pub message: WireMessage,
}

/// A message that failed to deserialize (e.g., due to corruption).
#[derive(Debug)]
pub struct CorruptedMessage {
    pub from: NodeId,
    pub to: NodeId,
    pub error: String,
}

/// Result of delivering messages for a tick.
#[derive(Debug, Default)]
pub struct DeliveryResult {
    /// Successfully deserialized messages.
    pub delivered: Vec<DeliveredMessage>,
    /// Messages that failed deserialization (corrupted by interceptors).
    pub corrupted: Vec<CorruptedMessage>,
}

impl MockTransport {
    /// Create a new transport with the given RNG seed for deterministic behavior.
    pub fn new(rng_seed: u64) -> Self {
        Self {
            in_flight: VecDeque::new(),
            interceptors: Vec::new(),
            message_counter: 0,
            current_tick: 0,
            rng_seed,
        }
    }

    /// Send a message from one node to another.
    ///
    /// The message is serialized to bytes, passed through the interceptor
    /// pipeline, and queued for delivery.
    pub fn send(&mut self, from: NodeId, to: NodeId, msg: &WireMessage) {
        let bytes = bincode::serialize(msg).expect("WireMessage serialization should not fail");
        self.send_bytes(from, to, bytes);
    }

    /// Broadcast a message from one node to all other nodes.
    pub fn broadcast(&mut self, from: NodeId, msg: &WireMessage, all_nodes: &[NodeId]) {
        let bytes = bincode::serialize(msg).expect("WireMessage serialization should not fail");
        for &to in all_nodes {
            if to != from {
                self.send_bytes(from, to, bytes.clone());
            }
        }
    }

    /// Deliver messages that are due at the current tick.
    ///
    /// Returns successfully deserialized messages and any that were
    /// corrupted by interceptors.
    pub fn deliver_due(&mut self) -> DeliveryResult {
        let mut result = DeliveryResult::default();

        // Partition: due now vs. still waiting
        let mut still_waiting = VecDeque::new();
        while let Some(msg) = self.in_flight.pop_front() {
            if msg.deliver_at_tick <= self.current_tick {
                match bincode::deserialize::<WireMessage>(&msg.data) {
                    Ok(wire_msg) => {
                        result.delivered.push(DeliveredMessage {
                            from: msg.from,
                            to: msg.to,
                            message: wire_msg,
                        });
                    }
                    Err(e) => {
                        result.corrupted.push(CorruptedMessage {
                            from: msg.from,
                            to: msg.to,
                            error: e.to_string(),
                        });
                    }
                }
            } else {
                still_waiting.push_back(msg);
            }
        }
        self.in_flight = still_waiting;

        result
    }

    /// Advance the transport tick counter.
    pub fn advance_tick(&mut self) {
        self.current_tick += 1;
    }

    /// Current tick number.
    pub fn current_tick(&self) -> u64 {
        self.current_tick
    }

    /// Number of messages currently in flight.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Add an interceptor to the pipeline.
    pub fn add_interceptor(&mut self, interceptor: Box<dyn NetworkInterceptor>) {
        self.interceptors.push(interceptor);
    }

    /// Remove all interceptors (restore clean network).
    pub fn clear_interceptors(&mut self) {
        self.interceptors.clear();
    }

    /// Drop all in-flight messages involving a specific node (from or to).
    /// Used to simulate a hard crash where queued messages are lost.
    pub fn drop_in_flight_for(&mut self, node_id: NodeId) {
        self.in_flight
            .retain(|m| m.from != node_id && m.to != node_id);
    }

    // ── Internal ────────────────────────────────────────────────

    fn send_bytes(&mut self, from: NodeId, to: NodeId, bytes: Vec<u8>) {
        let msg_id = self.message_counter;
        self.message_counter += 1;

        let context = InterceptContext {
            current_tick: self.current_tick,
            message_id: msg_id,
            rng_seed: self.rng_seed,
        };

        // Run through interceptor pipeline
        let actions = self.apply_interceptors(from, to, &bytes, &context);

        for action in actions {
            match action {
                InterceptAction::Deliver(data) => {
                    self.in_flight.push_back(InFlightMessage {
                        from,
                        to,
                        data,
                        deliver_at_tick: self.current_tick + 1, // deliver next tick
                    });
                }
                InterceptAction::Drop => {
                    // Message silently dropped
                }
                InterceptAction::Delay { bytes: data, ticks } => {
                    // Delay is additive to the baseline 1-tick latency
                    self.in_flight.push_back(InFlightMessage {
                        from,
                        to,
                        data,
                        deliver_at_tick: self.current_tick + 1 + ticks as u64,
                    });
                }
                InterceptAction::DeliverMany(copies) => {
                    for data in copies {
                        self.in_flight.push_back(InFlightMessage {
                            from,
                            to,
                            data,
                            deliver_at_tick: self.current_tick + 1,
                        });
                    }
                }
            }
        }
    }

    /// Apply all interceptors in sequence. Returns the final list of actions.
    ///
    /// Each interceptor sees every pending message and decides what to do.
    /// `Delay` actions finalize the message (skip further interceptors for
    /// that message) but do NOT drop other pending messages.
    fn apply_interceptors(
        &mut self,
        from: NodeId,
        to: NodeId,
        bytes: &[u8],
        context: &InterceptContext,
    ) -> Vec<InterceptAction> {
        if self.interceptors.is_empty() {
            return vec![InterceptAction::Deliver(bytes.to_vec())];
        }

        // Start with the original message
        let mut pending = vec![bytes.to_vec()];
        // Finalized actions (e.g., delayed messages that skip further interceptors)
        let mut finalized: Vec<InterceptAction> = Vec::new();

        for interceptor in &mut self.interceptors {
            let mut next_pending = Vec::new();
            for msg_bytes in &pending {
                match interceptor.intercept(from, to, msg_bytes, context) {
                    InterceptAction::Deliver(data) => {
                        next_pending.push(data);
                    }
                    InterceptAction::Drop => {
                        // Message dropped by this interceptor
                    }
                    InterceptAction::Delay { bytes: data, ticks } => {
                        // Finalize: skip further interceptors for this message
                        finalized.push(InterceptAction::Delay { bytes: data, ticks });
                    }
                    InterceptAction::DeliverMany(copies) => {
                        next_pending.extend(copies);
                    }
                }
            }
            pending = next_pending;
        }

        // Convert remaining pending bytes into Deliver actions
        finalized.extend(pending.into_iter().map(InterceptAction::Deliver));
        finalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interceptor::{DelayTicks, DropAll, DuplicateAll};
    use dstate::{Generation, SyncMessage};

    fn test_msg() -> WireMessage {
        WireMessage::Sync(SyncMessage::RequestSnapshot {
            state_name: "test".into(),
            requester: NodeId(1),
        })
    }

    #[test]
    fn send_and_deliver() {
        let mut transport = MockTransport::new(42);
        transport.send(NodeId(1), NodeId(2), &test_msg());

        // Not delivered yet (deliver at tick 1, currently tick 0)
        let result = transport.deliver_due();
        assert!(result.delivered.is_empty());

        // Advance tick and deliver
        transport.advance_tick();
        let result = transport.deliver_due();
        assert_eq!(result.delivered.len(), 1);
        assert_eq!(result.delivered[0].from, NodeId(1));
        assert_eq!(result.delivered[0].to, NodeId(2));
    }

    #[test]
    fn broadcast_sends_to_all_except_self() {
        let mut transport = MockTransport::new(42);
        let all = vec![NodeId(1), NodeId(2), NodeId(3)];
        transport.broadcast(NodeId(1), &test_msg(), &all);

        assert_eq!(transport.in_flight_count(), 2); // to 2 and 3

        transport.advance_tick();
        let result = transport.deliver_due();
        assert_eq!(result.delivered.len(), 2);
    }

    #[test]
    fn drop_all_interceptor() {
        let mut transport = MockTransport::new(42);
        transport.add_interceptor(Box::new(DropAll));
        transport.send(NodeId(1), NodeId(2), &test_msg());

        assert_eq!(transport.in_flight_count(), 0);
        transport.advance_tick();
        let result = transport.deliver_due();
        assert!(result.delivered.is_empty());
    }

    #[test]
    fn delay_interceptor() {
        let mut transport = MockTransport::new(42);
        transport.add_interceptor(Box::new(DelayTicks(3)));
        transport.send(NodeId(1), NodeId(2), &test_msg());

        // Baseline 1 tick + 3 extra = deliver at tick 4
        // Not delivered for ticks 1..3
        for _ in 0..3 {
            transport.advance_tick();
            assert!(transport.deliver_due().delivered.is_empty());
        }

        // Delivered on tick 4
        transport.advance_tick();
        let result = transport.deliver_due();
        assert_eq!(result.delivered.len(), 1);
    }

    #[test]
    fn duplicate_interceptor() {
        let mut transport = MockTransport::new(42);
        transport.add_interceptor(Box::new(DuplicateAll));
        transport.send(NodeId(1), NodeId(2), &test_msg());

        assert_eq!(transport.in_flight_count(), 2);
        transport.advance_tick();
        let result = transport.deliver_due();
        assert_eq!(result.delivered.len(), 2);
    }

    #[test]
    fn clear_interceptors_restores_clean_network() {
        let mut transport = MockTransport::new(42);
        transport.add_interceptor(Box::new(DropAll));
        transport.send(NodeId(1), NodeId(2), &test_msg());
        assert_eq!(transport.in_flight_count(), 0);

        transport.clear_interceptors();
        transport.send(NodeId(1), NodeId(2), &test_msg());
        assert_eq!(transport.in_flight_count(), 1);
    }

    #[test]
    fn corrupted_message_detected() {
        use crate::interceptor::CorruptBytes;

        let mut transport = MockTransport::new(42);
        transport.add_interceptor(Box::new(CorruptBytes::new(99)));

        // Send a real message
        let msg = WireMessage::Sync(SyncMessage::FullSnapshot {
            state_name: "test".into(),
            source_node: NodeId(1),
            generation: Generation::new(1, 1),
            wire_version: 1,
            data: vec![1, 2, 3],
        });
        transport.send(NodeId(1), NodeId(2), &msg);

        transport.advance_tick();
        let result = transport.deliver_due();
        // The message may or may not deserialize depending on which bit was flipped.
        // Either way, we should get exactly 1 result total.
        assert_eq!(result.delivered.len() + result.corrupted.len(), 1);
    }

    #[test]
    fn duplicate_then_delay_preserves_both_copies() {
        let mut transport = MockTransport::new(42);
        transport.add_interceptor(Box::new(DuplicateAll));
        transport.add_interceptor(Box::new(DelayTicks(2)));
        transport.send(NodeId(1), NodeId(2), &test_msg());

        // Both copies should be delayed: baseline 1 + 2 extra = tick 3
        for _ in 0..2 {
            transport.advance_tick();
            assert!(transport.deliver_due().delivered.is_empty());
        }
        transport.advance_tick();
        let result = transport.deliver_due();
        assert_eq!(result.delivered.len(), 2, "both duplicated copies must survive delay");
    }

    #[test]
    fn delay_zero_delivers_next_tick() {
        let mut transport = MockTransport::new(42);
        transport.add_interceptor(Box::new(DelayTicks(0)));
        transport.send(NodeId(1), NodeId(2), &test_msg());

        // DelayTicks(0) = baseline only = deliver at tick 1
        let result = transport.deliver_due();
        assert!(result.delivered.is_empty(), "not yet at tick 1");
        transport.advance_tick();
        let result = transport.deliver_due();
        assert_eq!(result.delivered.len(), 1);
    }

    #[test]
    fn drop_in_flight_for_node() {
        let mut transport = MockTransport::new(42);
        transport.send(NodeId(1), NodeId(2), &test_msg());
        transport.send(NodeId(3), NodeId(2), &test_msg());
        assert_eq!(transport.in_flight_count(), 2);

        transport.drop_in_flight_for(NodeId(1));
        assert_eq!(transport.in_flight_count(), 1);

        transport.advance_tick();
        let result = transport.deliver_due();
        assert_eq!(result.delivered.len(), 1);
        assert_eq!(result.delivered[0].from, NodeId(3));
    }
}
