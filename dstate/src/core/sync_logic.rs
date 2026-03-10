use std::time::Duration;

use crate::traits::state::SyncUrgency;
use crate::types::config::{PushMode, SyncStrategy};

/// Action to take after processing a mutation's sync urgency.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SyncAction {
    /// Broadcast a full snapshot to all peers immediately.
    BroadcastSnapshot,
    /// Broadcast a delta to all peers immediately.
    BroadcastDelta,
    /// Send a change-feed notification (lightweight, no payload).
    NotifyChangeFeed,
    /// Schedule a delayed send after the given duration.
    ScheduleDelayed(Duration),
    /// Suppress this mutation — don't send anything now.
    /// The caller should accumulate the delta for later.
    Suppress,
}

/// Pure logic for outbound synchronization decisions.
///
/// Given a `SyncStrategy` and a `SyncUrgency` per mutation, determines
/// what action the actor shell should take (broadcast, notify, delay, suppress).
pub(crate) struct SyncLogic {
    state_name: String,
    strategy: SyncStrategy,
    /// Whether this state uses deltas (DeltaDistributedState) or
    /// only full snapshots (DistributedState).
    supports_delta: bool,
}

impl SyncLogic {
    pub(crate) fn new(state_name: String, strategy: SyncStrategy, supports_delta: bool) -> Self {
        Self {
            state_name,
            strategy,
            supports_delta,
        }
    }

    /// Determine the outbound action for a given mutation urgency.
    pub(crate) fn resolve_action(&self, urgency: SyncUrgency) -> SyncAction {
        match urgency {
            SyncUrgency::Immediate => self.broadcast_action(),
            SyncUrgency::Delayed(d) => SyncAction::ScheduleDelayed(d),
            SyncUrgency::Suppress => SyncAction::Suppress,
            SyncUrgency::Default => match self.strategy.push_mode {
                Some(PushMode::ActivePush) => self.broadcast_action(),
                Some(PushMode::ActiveFeedLazyPull) => SyncAction::NotifyChangeFeed,
                None => SyncAction::Suppress,
            },
        }
    }

    pub(crate) fn strategy(&self) -> &SyncStrategy {
        &self.strategy
    }

    pub(crate) fn state_name(&self) -> &str {
        &self.state_name
    }

    pub(crate) fn should_periodic_sync(&self) -> bool {
        self.strategy.periodic_full_sync.is_some()
    }

    pub(crate) fn periodic_interval(&self) -> Option<Duration> {
        self.strategy.periodic_full_sync
    }

    fn broadcast_action(&self) -> SyncAction {
        if self.supports_delta {
            SyncAction::BroadcastDelta
        } else {
            SyncAction::BroadcastSnapshot
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_push_strategy() -> SyncStrategy {
        SyncStrategy::active_push()
    }

    fn feed_lazy_pull_strategy() -> SyncStrategy {
        SyncStrategy::feed_lazy_pull()
    }

    fn no_push_strategy() -> SyncStrategy {
        SyncStrategy {
            push_mode: None,
            periodic_full_sync: None,
            pull_on_query: false,
            change_feed: Default::default(),
        }
    }

    /// SE-01: Immediate urgency with delta support broadcasts a delta.
    #[test]
    fn immediate_urgency_broadcasts_delta() {
        let logic = SyncLogic::new("test".into(), active_push_strategy(), true);
        assert_eq!(logic.resolve_action(SyncUrgency::Immediate), SyncAction::BroadcastDelta);
    }

    /// SE-01b: Immediate urgency without delta support broadcasts a snapshot.
    #[test]
    fn immediate_urgency_broadcasts_snapshot_for_simple() {
        let logic = SyncLogic::new("test".into(), active_push_strategy(), false);
        assert_eq!(logic.resolve_action(SyncUrgency::Immediate), SyncAction::BroadcastSnapshot);
    }

    /// SE-02: Default urgency with ActivePush broadcasts delta (or snapshot).
    #[test]
    fn default_urgency_active_push() {
        let delta = SyncLogic::new("test".into(), active_push_strategy(), true);
        assert_eq!(delta.resolve_action(SyncUrgency::Default), SyncAction::BroadcastDelta);

        let simple = SyncLogic::new("test".into(), active_push_strategy(), false);
        assert_eq!(simple.resolve_action(SyncUrgency::Default), SyncAction::BroadcastSnapshot);
    }

    /// SE-03: Default urgency with ActiveFeedLazyPull notifies change feed.
    #[test]
    fn default_urgency_feed_lazy_pull() {
        let logic = SyncLogic::new("test".into(), feed_lazy_pull_strategy(), true);
        assert_eq!(logic.resolve_action(SyncUrgency::Default), SyncAction::NotifyChangeFeed);
    }

    /// SE-04: Default urgency with no push mode suppresses.
    #[test]
    fn default_urgency_no_push_mode() {
        let logic = SyncLogic::new("test".into(), no_push_strategy(), true);
        assert_eq!(logic.resolve_action(SyncUrgency::Default), SyncAction::Suppress);
    }

    /// SE-05: Delayed urgency schedules a delayed send.
    #[test]
    fn delayed_urgency() {
        let logic = SyncLogic::new("test".into(), active_push_strategy(), true);
        let delay = Duration::from_secs(5);
        assert_eq!(logic.resolve_action(SyncUrgency::Delayed(delay)), SyncAction::ScheduleDelayed(delay));
    }

    /// Suppress urgency always suppresses regardless of strategy.
    #[test]
    fn suppress_urgency() {
        let logic = SyncLogic::new("test".into(), active_push_strategy(), true);
        assert_eq!(logic.resolve_action(SyncUrgency::Suppress), SyncAction::Suppress);
    }

    /// Immediate overrides feed mode — even with ActiveFeedLazyPull,
    /// Immediate urgency broadcasts a delta.
    #[test]
    fn immediate_overrides_feed_mode() {
        let logic = SyncLogic::new("test".into(), feed_lazy_pull_strategy(), true);
        assert_eq!(logic.resolve_action(SyncUrgency::Immediate), SyncAction::BroadcastDelta);
    }

    /// Periodic sync is reported as configured when set.
    #[test]
    fn periodic_sync_configured() {
        let strategy = SyncStrategy::feed_with_periodic_sync(Duration::from_secs(300));
        let logic = SyncLogic::new("test".into(), strategy, true);
        assert!(logic.should_periodic_sync());
        assert_eq!(logic.periodic_interval(), Some(Duration::from_secs(300)));
    }

    /// Periodic sync is absent when not configured.
    #[test]
    fn periodic_sync_not_configured() {
        let logic = SyncLogic::new("test".into(), active_push_strategy(), true);
        assert!(!logic.should_periodic_sync());
        assert_eq!(logic.periodic_interval(), None);
    }

    // ── SYNC-01: ActivePush mutation produces broadcast ─────────

    #[test]
    fn active_push_mutation_produces_broadcast() {
        let logic = SyncLogic::new("counters".into(), active_push_strategy(), true);
        assert_eq!(logic.resolve_action(SyncUrgency::Default), SyncAction::BroadcastDelta);

        let simple = SyncLogic::new("counters".into(), active_push_strategy(), false);
        assert_eq!(simple.resolve_action(SyncUrgency::Default), SyncAction::BroadcastSnapshot);
    }

    // ── SYNC-04: Feed mode mutation produces notify ─────────────

    #[test]
    fn feed_mode_mutation_produces_notify() {
        let logic = SyncLogic::new("sessions".into(), feed_lazy_pull_strategy(), true);
        assert_eq!(logic.resolve_action(SyncUrgency::Default), SyncAction::NotifyChangeFeed);

        // Also verify for non-delta state — feed mode still notifies.
        let simple = SyncLogic::new("sessions".into(), feed_lazy_pull_strategy(), false);
        assert_eq!(simple.resolve_action(SyncUrgency::Default), SyncAction::NotifyChangeFeed);
    }

    // ── SYNC-07: Fresh view, pull_on_query flag ─────────────────

    #[test]
    fn fresh_view_no_pull() {
        let with_pull = SyncLogic::new("test".into(), feed_lazy_pull_strategy(), true);
        assert!(with_pull.strategy().pull_on_query);

        let without_pull = SyncLogic::new("test".into(), active_push_strategy(), true);
        assert!(!without_pull.strategy().pull_on_query);
    }

    // ── Strategy composition: feed_with_periodic_sync ────────────

    #[test]
    fn strategy_composition_feed_with_periodic() {
        let strategy = SyncStrategy::feed_with_periodic_sync(Duration::from_secs(300));
        assert_eq!(strategy.push_mode, Some(PushMode::ActiveFeedLazyPull));
        assert_eq!(strategy.periodic_full_sync, Some(Duration::from_secs(300)));
        assert!(strategy.pull_on_query);
    }

    // ── Strategy composition: periodic_only ──────────────────────

    #[test]
    fn strategy_composition_periodic_only() {
        let strategy = SyncStrategy::periodic_only(Duration::from_secs(600));
        assert_eq!(strategy.push_mode, None);
        assert_eq!(strategy.periodic_full_sync, Some(Duration::from_secs(600)));
        assert!(!strategy.pull_on_query);
    }

    // ── Immediate overrides any strategy (even push_mode=None) ──

    #[test]
    fn immediate_overrides_any_strategy() {
        let logic = SyncLogic::new("test".into(), no_push_strategy(), true);
        // Default urgency with no push_mode → Suppress.
        assert_eq!(logic.resolve_action(SyncUrgency::Default), SyncAction::Suppress);
        // But Immediate urgency always broadcasts, regardless of push_mode.
        assert_eq!(logic.resolve_action(SyncUrgency::Immediate), SyncAction::BroadcastDelta);
    }
}
