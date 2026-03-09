use std::time::Duration;

use crate::types::node::VersionMismatchPolicy;

/// Push mode for synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushMode {
    /// Push deltas (or full state for simple types) immediately on every mutation.
    ActivePush,
    /// Send change-feed notifications; peers lazily pull when they need fresh data.
    ActiveFeedLazyPull,
}

/// Configuration for the change feed aggregator.
#[derive(Debug, Clone)]
pub struct ChangeFeedConfig {
    /// How often accumulated change notifications are flushed and broadcast.
    pub batch_interval: Duration,
}

impl Default for ChangeFeedConfig {
    fn default() -> Self {
        Self {
            batch_interval: Duration::from_secs(1),
        }
    }
}

/// Synchronization strategy composed from independent options.
#[derive(Debug, Clone)]
pub struct SyncStrategy {
    /// How mutations are pushed to peers. `None` = no automatic push.
    pub push_mode: Option<PushMode>,
    /// If set, a full snapshot is pushed at this interval as a safety net.
    pub periodic_full_sync: Option<Duration>,
    /// If `true`, queries with freshness requirements will pull from peers
    /// on demand.
    pub pull_on_query: bool,
    /// Configuration for the change feed (only used with `ActiveFeedLazyPull`).
    pub change_feed: ChangeFeedConfig,
}

impl SyncStrategy {
    /// Active push: immediate delta/snapshot push on every mutation.
    pub fn active_push() -> Self {
        Self {
            push_mode: Some(PushMode::ActivePush),
            periodic_full_sync: None,
            pull_on_query: false,
            change_feed: ChangeFeedConfig::default(),
        }
    }

    /// Feed + lazy pull: change notifications + on-demand pull.
    pub fn feed_lazy_pull() -> Self {
        Self {
            push_mode: Some(PushMode::ActiveFeedLazyPull),
            periodic_full_sync: None,
            pull_on_query: true,
            change_feed: ChangeFeedConfig::default(),
        }
    }

    /// Feed + periodic safety-net full sync.
    pub fn feed_with_periodic_sync(interval: Duration) -> Self {
        Self {
            push_mode: Some(PushMode::ActiveFeedLazyPull),
            periodic_full_sync: Some(interval),
            pull_on_query: true,
            change_feed: ChangeFeedConfig::default(),
        }
    }

    /// Periodic-only: no push, just periodic full snapshots.
    pub fn periodic_only(interval: Duration) -> Self {
        Self {
            push_mode: None,
            periodic_full_sync: Some(interval),
            pull_on_query: false,
            change_feed: ChangeFeedConfig::default(),
        }
    }
}

/// Per-state-type configuration.
#[derive(Debug, Clone)]
pub struct StateConfig {
    /// How this state type synchronizes across the cluster.
    pub sync_strategy: SyncStrategy,
    /// Maximum time to wait for a pull response from a peer.
    pub pull_timeout: Duration,
    /// Policy for handling incoming messages at unsupported wire versions.
    pub version_mismatch_policy: VersionMismatchPolicy,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            sync_strategy: SyncStrategy::active_push(),
            pull_timeout: Duration::from_secs(5),
            version_mismatch_policy: VersionMismatchPolicy::KeepStale,
        }
    }
}
