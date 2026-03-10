use std::fmt::Debug;

/// Buffer for accumulating suppressed view deltas.
///
/// When [`SyncLogic`](super::sync_logic::SyncLogic) returns
/// [`SyncAction::Suppress`](super::sync_logic::SyncAction::Suppress), the
/// actor shell pushes the delta into this accumulator instead of broadcasting.
/// On the next non-suppressed mutation, the actor shell calls [`flush`](Self::flush)
/// to drain all accumulated deltas, which it can then combine with the current
/// delta before broadcasting.
///
/// # Threading model
///
/// This struct is intended for single-actor use. It is owned exclusively by
/// the `StateShard` actor and accessed sequentially within that actor's
/// message loop — no internal locking is needed.
#[allow(dead_code)]
pub(crate) struct DeltaAccumulator<VD> {
    buffer: Vec<VD>,
}

#[allow(dead_code)]
impl<VD: Clone + Debug> DeltaAccumulator<VD> {
    /// Create an empty accumulator.
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Add a suppressed delta to the buffer.
    pub fn accumulate(&mut self, delta: VD) {
        self.buffer.push(delta);
    }

    /// Drain and return all accumulated deltas, leaving the buffer empty.
    ///
    /// Returns `None` if no deltas have been accumulated.
    pub fn flush(&mut self) -> Option<Vec<VD>> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(self.buffer.drain(..).collect())
        }
    }

    /// Number of deltas currently buffered.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_flush_returns_none() {
        let mut acc: DeltaAccumulator<i32> = DeltaAccumulator::new();
        assert!(acc.flush().is_none());
        assert!(acc.is_empty());
    }

    #[test]
    fn accumulate_and_flush() {
        let mut acc = DeltaAccumulator::new();
        acc.accumulate(10);
        acc.accumulate(20);
        acc.accumulate(30);
        assert_eq!(acc.len(), 3);

        let flushed = acc.flush().unwrap();
        assert_eq!(flushed, vec![10, 20, 30]);
        assert!(acc.is_empty());
        assert!(acc.flush().is_none());
    }

    #[test]
    fn flush_clears_buffer() {
        let mut acc = DeltaAccumulator::new();
        acc.accumulate("delta1".to_string());
        acc.accumulate("delta2".to_string());
        let _ = acc.flush();

        acc.accumulate("delta3".to_string());
        let flushed = acc.flush().unwrap();
        assert_eq!(flushed, vec!["delta3".to_string()]);
    }

    /// SYNC-17: Suppressed deltas accumulate — buffer holds all suppressed
    /// deltas until flushed, preserving insertion order.
    #[test]
    fn suppressed_deltas_accumulate_in_order() {
        let mut acc = DeltaAccumulator::new();
        for i in 0..5 {
            acc.accumulate(i);
        }
        let flushed = acc.flush().unwrap();
        assert_eq!(flushed, vec![0, 1, 2, 3, 4]);
    }
}
