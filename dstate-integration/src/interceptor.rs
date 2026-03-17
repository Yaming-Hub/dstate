//! Network interceptors for simulating faults in integration tests.
//!
//! Interceptors sit in the message pipeline between nodes and can drop,
//! delay, duplicate, or corrupt messages to test fault-tolerance.

use dstate::NodeId;
use std::collections::HashSet;

/// Decision returned by a [`NetworkInterceptor`] for a message in transit.
#[derive(Debug, Clone)]
pub enum InterceptAction {
    /// Deliver the message (possibly modified bytes).
    Deliver(Vec<u8>),
    /// Drop the message silently.
    Drop,
    /// Hold the message for `ticks` ticks before delivery.
    Delay { bytes: Vec<u8>, ticks: usize },
    /// Deliver multiple copies (for duplication testing).
    DeliverMany(Vec<Vec<u8>>),
}

/// Context passed to interceptors for deterministic decisions.
#[derive(Debug, Clone)]
pub struct InterceptContext {
    /// Current simulation tick.
    pub current_tick: u64,
    /// Unique message identifier (monotonically increasing).
    pub message_id: u64,
    /// Seed for deterministic pseudo-random decisions.
    pub rng_seed: u64,
}

/// Trait for intercepting messages in the mock transport pipeline.
///
/// Each interceptor sees every message and decides what to do with it.
/// Multiple interceptors can be chained; the output of one feeds the next.
pub trait NetworkInterceptor: Send {
    /// Inspect a message in transit and return an action controlling delivery.
    fn intercept(
        &mut self,
        from: NodeId,
        to: NodeId,
        msg_bytes: &[u8],
        context: &InterceptContext,
    ) -> InterceptAction;
}

// ── Built-in interceptors ───────────────────────────────────────

/// Delivers all messages unmodified.
pub struct PassThrough;

impl NetworkInterceptor for PassThrough {
    fn intercept(
        &mut self,
        _from: NodeId,
        _to: NodeId,
        msg_bytes: &[u8],
        _context: &InterceptContext,
    ) -> InterceptAction {
        InterceptAction::Deliver(msg_bytes.to_vec())
    }
}

/// Drops all messages (total network failure).
pub struct DropAll;

impl NetworkInterceptor for DropAll {
    fn intercept(
        &mut self,
        _from: NodeId,
        _to: NodeId,
        _msg_bytes: &[u8],
        _context: &InterceptContext,
    ) -> InterceptAction {
        InterceptAction::Drop
    }
}

/// Drops messages with a given probability using deterministic seeded RNG.
pub struct DropRate {
    rate: f64,
    seed: u64,
}

impl DropRate {
    pub fn new(rate: f64, seed: u64) -> Self {
        assert!((0.0..=1.0).contains(&rate), "rate must be in [0.0, 1.0]");
        Self { rate, seed }
    }
}

impl NetworkInterceptor for DropRate {
    fn intercept(
        &mut self,
        _from: NodeId,
        _to: NodeId,
        msg_bytes: &[u8],
        context: &InterceptContext,
    ) -> InterceptAction {
        // Simple deterministic hash: combine seed, message_id, and tick
        let hash = splitmix64(self.seed ^ context.message_id ^ context.current_tick);
        let prob = (hash as f64) / (u64::MAX as f64);
        if prob < self.rate {
            InterceptAction::Drop
        } else {
            InterceptAction::Deliver(msg_bytes.to_vec())
        }
    }
}

/// Drops messages between two sets of nodes (network partition).
///
/// When `asymmetric` is false, messages are dropped in both directions.
/// When `asymmetric` is true, only messages from set A to set B are dropped.
pub struct Partition {
    set_a: HashSet<NodeId>,
    set_b: HashSet<NodeId>,
    asymmetric: bool,
}

impl Partition {
    /// Create a symmetric partition (A cannot reach B and B cannot reach A).
    pub fn symmetric(a: impl IntoIterator<Item = NodeId>, b: impl IntoIterator<Item = NodeId>) -> Self {
        Self {
            set_a: a.into_iter().collect(),
            set_b: b.into_iter().collect(),
            asymmetric: false,
        }
    }

    /// Create an asymmetric partition (A cannot reach B, but B can reach A).
    pub fn asymmetric(a: impl IntoIterator<Item = NodeId>, b: impl IntoIterator<Item = NodeId>) -> Self {
        Self {
            set_a: a.into_iter().collect(),
            set_b: b.into_iter().collect(),
            asymmetric: true,
        }
    }
}

impl NetworkInterceptor for Partition {
    fn intercept(
        &mut self,
        from: NodeId,
        to: NodeId,
        msg_bytes: &[u8],
        _context: &InterceptContext,
    ) -> InterceptAction {
        let a_to_b = self.set_a.contains(&from) && self.set_b.contains(&to);
        let b_to_a = self.set_b.contains(&from) && self.set_a.contains(&to);

        if a_to_b || (!self.asymmetric && b_to_a) {
            InterceptAction::Drop
        } else {
            InterceptAction::Deliver(msg_bytes.to_vec())
        }
    }
}

/// Delays all messages by an additional number of ticks beyond the baseline
/// 1-tick latency. `DelayTicks(0)` behaves like normal delivery (1 tick).
/// `DelayTicks(3)` delivers after 4 ticks total (1 baseline + 3 extra).
pub struct DelayTicks(pub usize);

impl NetworkInterceptor for DelayTicks {
    fn intercept(
        &mut self,
        _from: NodeId,
        _to: NodeId,
        msg_bytes: &[u8],
        _context: &InterceptContext,
    ) -> InterceptAction {
        InterceptAction::Delay {
            bytes: msg_bytes.to_vec(),
            ticks: self.0,
        }
    }
}

/// Delivers each message twice (duplication testing).
pub struct DuplicateAll;

impl NetworkInterceptor for DuplicateAll {
    fn intercept(
        &mut self,
        _from: NodeId,
        _to: NodeId,
        msg_bytes: &[u8],
        _context: &InterceptContext,
    ) -> InterceptAction {
        let bytes = msg_bytes.to_vec();
        InterceptAction::DeliverMany(vec![bytes.clone(), bytes])
    }
}

/// Flips random bits in serialized data (deterministic seeded RNG).
pub struct CorruptBytes {
    seed: u64,
}

impl CorruptBytes {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }
}

impl NetworkInterceptor for CorruptBytes {
    fn intercept(
        &mut self,
        _from: NodeId,
        _to: NodeId,
        msg_bytes: &[u8],
        context: &InterceptContext,
    ) -> InterceptAction {
        let mut bytes = msg_bytes.to_vec();
        if !bytes.is_empty() {
            let hash = splitmix64(self.seed ^ context.message_id);
            let idx = (hash as usize) % bytes.len();
            let bit = ((hash >> 8) % 8) as u8;
            bytes[idx] ^= 1 << bit;
        }
        InterceptAction::Deliver(bytes)
    }
}

// ── Deterministic RNG helper ────────────────────────────────────

/// SplitMix64: fast deterministic hash function for seeded RNG.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tick: u64, msg_id: u64) -> InterceptContext {
        InterceptContext {
            current_tick: tick,
            message_id: msg_id,
            rng_seed: 42,
        }
    }

    #[test]
    fn passthrough_delivers() {
        let mut p = PassThrough;
        let action = p.intercept(NodeId(1), NodeId(2), b"hello", &ctx(0, 0));
        assert!(matches!(action, InterceptAction::Deliver(d) if d == b"hello"));
    }

    #[test]
    fn drop_all_drops() {
        let mut d = DropAll;
        let action = d.intercept(NodeId(1), NodeId(2), b"hello", &ctx(0, 0));
        assert!(matches!(action, InterceptAction::Drop));
    }

    #[test]
    fn drop_rate_deterministic() {
        let mut dr = DropRate::new(0.5, 12345);
        let mut dropped = 0;
        let mut delivered = 0;
        for i in 0..100 {
            match dr.intercept(NodeId(1), NodeId(2), b"x", &ctx(0, i)) {
                InterceptAction::Drop => dropped += 1,
                InterceptAction::Deliver(_) => delivered += 1,
                _ => panic!("unexpected action"),
            }
        }
        assert!(dropped > 0 && delivered > 0, "should have both drops and deliveries");

        // Run again with same seed — should produce same results
        let mut dr2 = DropRate::new(0.5, 12345);
        let mut dropped2 = 0;
        for i in 0..100 {
            if matches!(dr2.intercept(NodeId(1), NodeId(2), b"x", &ctx(0, i)), InterceptAction::Drop) {
                dropped2 += 1;
            }
        }
        assert_eq!(dropped, dropped2, "same seed must produce same results");
    }

    #[test]
    fn symmetric_partition() {
        let mut p = Partition::symmetric([NodeId(1)], [NodeId(2)]);
        // A→B blocked
        assert!(matches!(
            p.intercept(NodeId(1), NodeId(2), b"x", &ctx(0, 0)),
            InterceptAction::Drop
        ));
        // B→A also blocked
        assert!(matches!(
            p.intercept(NodeId(2), NodeId(1), b"x", &ctx(0, 1)),
            InterceptAction::Drop
        ));
        // A→C not blocked
        assert!(matches!(
            p.intercept(NodeId(1), NodeId(3), b"x", &ctx(0, 2)),
            InterceptAction::Deliver(_)
        ));
    }

    #[test]
    fn asymmetric_partition() {
        let mut p = Partition::asymmetric([NodeId(1)], [NodeId(2)]);
        // A→B blocked
        assert!(matches!(
            p.intercept(NodeId(1), NodeId(2), b"x", &ctx(0, 0)),
            InterceptAction::Drop
        ));
        // B→A allowed
        assert!(matches!(
            p.intercept(NodeId(2), NodeId(1), b"x", &ctx(0, 1)),
            InterceptAction::Deliver(_)
        ));
    }

    #[test]
    fn delay_ticks() {
        let mut d = DelayTicks(3);
        let action = d.intercept(NodeId(1), NodeId(2), b"x", &ctx(0, 0));
        assert!(matches!(action, InterceptAction::Delay { ticks: 3, .. }));
    }

    #[test]
    fn duplicate_all() {
        let mut d = DuplicateAll;
        let action = d.intercept(NodeId(1), NodeId(2), b"x", &ctx(0, 0));
        match action {
            InterceptAction::DeliverMany(copies) => {
                assert_eq!(copies.len(), 2);
                assert_eq!(copies[0], copies[1]);
            }
            _ => panic!("expected DeliverMany"),
        }
    }

    #[test]
    fn corrupt_bytes_flips_bit() {
        let mut c = CorruptBytes::new(42);
        let original = b"hello world";
        let action = c.intercept(NodeId(1), NodeId(2), original, &ctx(0, 0));
        match action {
            InterceptAction::Deliver(bytes) => {
                assert_ne!(bytes.as_slice(), original);
                // Exactly one bit should differ
                let diff_count: u32 = bytes
                    .iter()
                    .zip(original.iter())
                    .map(|(a, b)| (a ^ b).count_ones())
                    .sum();
                assert_eq!(diff_count, 1);
            }
            _ => panic!("expected Deliver"),
        }
    }
}
