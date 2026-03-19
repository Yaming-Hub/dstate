use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dstate::NodeId;

/// Result of intercepting a message in the transport layer.
pub enum InterceptResult {
    /// Deliver the message normally.
    Deliver(Vec<u8>),
    /// Drop the message silently.
    Drop,
    /// Delay delivery by the specified duration.
    Delay(Vec<u8>, Duration),
}

/// Trait for fault-injection interceptors on the in-process transport.
pub trait Interceptor: Send + Sync {
    fn intercept(&self, from: NodeId, to: NodeId, data: Vec<u8>) -> InterceptResult;
}

// ── Built-in interceptors ───────────────────────────────────────────

/// Drops every message unconditionally.
pub struct DropAll;

impl Interceptor for DropAll {
    fn intercept(&self, _from: NodeId, _to: NodeId, _data: Vec<u8>) -> InterceptResult {
        InterceptResult::Drop
    }
}

/// Drops messages with the given probability using a simple LCG PRNG.
pub struct DropRate {
    rate: f64,
    state: AtomicU64,
}

impl DropRate {
    pub fn new(rate: f64, seed: u64) -> Self {
        assert!((0.0..=1.0).contains(&rate), "rate must be in [0.0, 1.0]");
        Self {
            rate,
            state: AtomicU64::new(seed),
        }
    }

    fn next_u64(&self) -> u64 {
        // LCG constants (Knuth MMIX)
        let old = self.state.load(Ordering::Relaxed);
        let next = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state.store(next, Ordering::Relaxed);
        next
    }
}

impl Interceptor for DropRate {
    fn intercept(&self, _from: NodeId, _to: NodeId, data: Vec<u8>) -> InterceptResult {
        let val = self.next_u64();
        let threshold = (self.rate * u64::MAX as f64) as u64;
        if val < threshold {
            InterceptResult::Drop
        } else {
            InterceptResult::Deliver(data)
        }
    }
}

/// Drops messages that cross the partition boundary between two groups (both directions).
pub struct Partition {
    pub group_a: HashSet<NodeId>,
    pub group_b: HashSet<NodeId>,
}

impl Partition {
    pub fn new(group_a: HashSet<NodeId>, group_b: HashSet<NodeId>) -> Self {
        Self { group_a, group_b }
    }
}

impl Interceptor for Partition {
    fn intercept(&self, from: NodeId, to: NodeId, data: Vec<u8>) -> InterceptResult {
        let crosses = (self.group_a.contains(&from) && self.group_b.contains(&to))
            || (self.group_b.contains(&from) && self.group_a.contains(&to));
        if crosses {
            InterceptResult::Drop
        } else {
            InterceptResult::Deliver(data)
        }
    }
}

/// Delays every message by a fixed duration.
pub struct DelayAll {
    pub delay: Duration,
}

impl DelayAll {
    pub fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

impl Interceptor for DelayAll {
    fn intercept(&self, _from: NodeId, _to: NodeId, data: Vec<u8>) -> InterceptResult {
        InterceptResult::Delay(data, self.delay)
    }
}

/// Flips a random bit in the payload to simulate corruption.
pub struct CorruptBytes {
    state: AtomicU64,
}

impl CorruptBytes {
    pub fn new(seed: u64) -> Self {
        Self {
            state: AtomicU64::new(seed),
        }
    }

    fn next_u64(&self) -> u64 {
        let old = self.state.load(Ordering::Relaxed);
        let next = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state.store(next, Ordering::Relaxed);
        next
    }
}

impl Interceptor for CorruptBytes {
    fn intercept(&self, _from: NodeId, _to: NodeId, mut data: Vec<u8>) -> InterceptResult {
        if data.is_empty() {
            return InterceptResult::Deliver(data);
        }
        let r = self.next_u64();
        let byte_idx = (r >> 32) as usize % data.len();
        let bit_idx = (r & 0x7) as u8;
        data[byte_idx] ^= 1 << bit_idx;
        InterceptResult::Deliver(data)
    }
}
