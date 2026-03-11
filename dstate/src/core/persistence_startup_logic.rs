use crate::traits::clock::Clock;
use crate::traits::persistence::PersistError;
use crate::types::envelope::StateObject;
use crate::types::node::Generation;

/// Result of the startup initialization flow.
///
/// Tells the actor shell what happened so it can log appropriately and
/// decide how to proceed (e.g., re-announce to cluster on `Restored`).
#[derive(Debug)]
pub(crate) enum StartupOutcome<S> {
    /// No persisted state found — fresh start with new incarnation.
    Fresh {
        state: StateObject<S>,
    },
    /// Persisted state loaded successfully at the current storage version.
    Restored {
        state: StateObject<S>,
    },
    /// Persisted state loaded and migrated from an older storage version.
    Migrated {
        state: StateObject<S>,
        from_version: u32,
    },
    /// Persistence load or migration failed — fell back to empty state
    /// with a new incarnation.
    FallbackToEmpty {
        state: StateObject<S>,
        reason: String,
    },
}

#[allow(dead_code)]
impl<S> StartupOutcome<S> {
    /// Extract the state regardless of outcome variant.
    pub fn into_state(self) -> StateObject<S> {
        match self {
            Self::Fresh { state }
            | Self::Restored { state }
            | Self::Migrated { state, .. }
            | Self::FallbackToEmpty { state, .. } => state,
        }
    }

    /// Reference to the state regardless of outcome variant.
    pub fn state(&self) -> &StateObject<S> {
        match self {
            Self::Fresh { state }
            | Self::Restored { state }
            | Self::Migrated { state, .. }
            | Self::FallbackToEmpty { state, .. } => state,
        }
    }
}

/// Pure logic for computing the initial state at actor startup from
/// persisted storage.
///
/// Handles:
/// - Loading persisted state and deciding whether to use, migrate, or
///   discard it
/// - Incarnation generation (clock-based for fresh starts, preserved for
///   successful loads)
/// - Storage version comparison and migration dispatch
/// - Graceful fallback to empty state on any failure
///
/// # Threading model
///
/// This struct is intended for single-actor use. It is created once at
/// startup and used to compute the initial `StateObject`. It does not
/// persist beyond initialization.
///
/// # Incarnation Rules
///
/// | Scenario | Incarnation |
/// |----------|-------------|
/// | No persistence / load returns None | `clock.unix_ms()` |
/// | Load succeeds, same storage version | Preserved from loaded state |
/// | Load succeeds, migration succeeds | Preserved from loaded state |
/// | Load fails | `clock.unix_ms()` |
/// | Migration fails | `clock.unix_ms()` (new incarnation) |
#[allow(dead_code)]
pub(crate) struct PersistenceStartupLogic {
    current_storage_version: u32,
}

#[allow(dead_code)]
impl PersistenceStartupLogic {
    pub fn new(current_storage_version: u32) -> Self {
        Self {
            current_storage_version,
        }
    }

    /// Compute the initial state from a persistence load result.
    ///
    /// - `load_result`: the result of calling `StatePersistence::load()`
    /// - `default_value`: the default state value for fresh starts
    /// - `clock`: used for incarnation generation and timestamps
    /// - `migrate_fn`: called when `stored_version != current_storage_version`;
    ///   receives `(old_value, old_version)` and returns the migrated value
    ///   or an error string
    pub fn initialize<S, F>(
        &self,
        load_result: Result<Option<StateObject<S>>, PersistError>,
        default_value: S,
        clock: &dyn Clock,
        migrate_fn: F,
    ) -> StartupOutcome<S>
    where
        F: FnOnce(S, u32) -> Result<S, String>,
    {
        match load_result {
            Err(e) => {
                let state = self.fresh_state(default_value, clock);
                StartupOutcome::FallbackToEmpty {
                    state,
                    reason: e.to_string(),
                }
            }
            Ok(None) => {
                let state = self.fresh_state(default_value, clock);
                StartupOutcome::Fresh { state }
            }
            Ok(Some(loaded)) => {
                if loaded.storage_version == self.current_storage_version {
                    StartupOutcome::Restored { state: loaded }
                } else {
                    let from_version = loaded.storage_version;
                    match migrate_fn(loaded.value, loaded.storage_version) {
                        Ok(migrated_value) => {
                            let state = StateObject {
                                generation: loaded.generation,
                                storage_version: self.current_storage_version,
                                value: migrated_value,
                                created_time: loaded.created_time,
                                modified_time: loaded.modified_time,
                            };
                            StartupOutcome::Migrated { state, from_version }
                        }
                        Err(reason) => {
                            // Guarantee monotonic incarnation: use the
                            // greater of clock time and loaded incarnation + 1
                            // to prevent peers from rejecting the new state.
                            let state = self.fresh_state_at_least(
                                default_value,
                                clock,
                                loaded.generation.incarnation,
                            );
                            StartupOutcome::FallbackToEmpty { state, reason }
                        }
                    }
                }
            }
        }
    }

    fn fresh_state<S>(&self, value: S, clock: &dyn Clock) -> StateObject<S> {
        let now = clock.unix_ms();
        StateObject {
            generation: Generation::new(now as u64, 0),
            storage_version: self.current_storage_version,
            value,
            created_time: now,
            modified_time: now,
        }
    }

    /// Like `fresh_state` but guarantees the incarnation is strictly
    /// greater than `old_incarnation` (prevents peers from rejecting
    /// the new state on fast restarts or clock skew).
    fn fresh_state_at_least<S>(
        &self,
        value: S,
        clock: &dyn Clock,
        old_incarnation: u64,
    ) -> StateObject<S> {
        let now = clock.unix_ms();
        let incarnation = std::cmp::max(now as u64, old_incarnation + 1);
        StateObject {
            generation: Generation::new(incarnation, 0),
            storage_version: self.current_storage_version,
            value,
            created_time: now,
            modified_time: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_clock::TestClock;

    fn make_clock() -> TestClock {
        TestClock::with_base_unix_ms(1_700_000_000_000) // ~2023
    }

    // ── INC-04: Incarnation generated on startup without persistence ──

    #[test]
    fn inc_04_fresh_startup_incarnation_from_clock() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(1);

        let outcome = logic.initialize::<String, _>(
            Ok(None),
            "default".into(),
            &clock,
            |_, _| unreachable!("no migration on fresh start"),
        );

        match &outcome {
            StartupOutcome::Fresh { state } => {
                assert_eq!(state.generation.incarnation, 1_700_000_000_000);
                assert_eq!(state.generation.age, 0);
                assert_eq!(state.storage_version, 1);
                assert_eq!(state.value, "default");
            }
            _ => panic!("expected Fresh, got {:?}", std::mem::discriminant(&outcome)),
        }
    }

    // ── INC-05: Incarnation preserved on startup with persistence ─────

    #[test]
    fn inc_05_loaded_incarnation_preserved() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(1);

        let loaded = StateObject {
            generation: Generation::new(42, 5),
            storage_version: 1,
            value: "persisted".to_string(),
            created_time: 100,
            modified_time: 200,
        };

        let outcome = logic.initialize(
            Ok(Some(loaded)),
            "default".into(),
            &clock,
            |_, _| unreachable!("no migration when versions match"),
        );

        match &outcome {
            StartupOutcome::Restored { state } => {
                assert_eq!(state.generation, Generation::new(42, 5));
                assert_eq!(state.value, "persisted");
                assert_eq!(state.created_time, 100);
                assert_eq!(state.modified_time, 200);
            }
            _ => panic!("expected Restored"),
        }
    }

    // ── INC-06: Incarnation changes on migration failure ──────────────

    #[test]
    fn inc_06_migration_failure_new_incarnation() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(2); // current version is 2

        let loaded = StateObject {
            generation: Generation::new(42, 5),
            storage_version: 1, // old version
            value: "old_data".to_string(),
            created_time: 100,
            modified_time: 200,
        };

        let outcome = logic.initialize(
            Ok(Some(loaded)),
            "default".into(),
            &clock,
            |_, _| Err("migration not supported".into()),
        );

        match &outcome {
            StartupOutcome::FallbackToEmpty { state, reason } => {
                // New incarnation must be > old incarnation (42)
                assert!(state.generation.incarnation > 42);
                assert_eq!(state.generation.age, 0);
                assert_eq!(state.value, "default");
                assert!(reason.contains("migration not supported"));
            }
            _ => panic!("expected FallbackToEmpty"),
        }
    }

    // ── INC-06b: Monotonic incarnation when clock < old incarnation ───

    #[test]
    fn inc_06b_migration_failure_incarnation_monotonic() {
        // Clock time (100) is less than loaded incarnation (9999).
        // Incarnation must still be > 9999.
        let clock = TestClock::with_base_unix_ms(100);
        let logic = PersistenceStartupLogic::new(2);

        let loaded = StateObject {
            generation: Generation::new(9999, 5),
            storage_version: 1,
            value: "old".to_string(),
            created_time: 50,
            modified_time: 60,
        };

        let outcome = logic.initialize(
            Ok(Some(loaded)),
            "default".into(),
            &clock,
            |_, _| Err("fail".into()),
        );

        match &outcome {
            StartupOutcome::FallbackToEmpty { state, .. } => {
                assert!(
                    state.generation.incarnation > 9999,
                    "incarnation {} must be > 9999 (old)",
                    state.generation.incarnation
                );
            }
            _ => panic!("expected FallbackToEmpty"),
        }
    }

    // ── PERSIST-06: load() returns None on fresh node ─────────────────

    #[test]
    fn persist_06_load_none_fresh_start() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(1);

        let outcome = logic.initialize::<u64, _>(
            Ok(None),
            0,
            &clock,
            |_, _| unreachable!(),
        );

        let state = outcome.into_state();
        assert_eq!(state.generation.age, 0);
        assert_eq!(state.value, 0);
    }

    // ── PERSIST-11: load() error falls back to empty state ────────────

    #[test]
    fn persist_11_load_error_fallback() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(1);

        let outcome = logic.initialize::<String, _>(
            Err(PersistError::StorageUnavailable("disk full".into())),
            "empty".into(),
            &clock,
            |_, _| unreachable!(),
        );

        match &outcome {
            StartupOutcome::FallbackToEmpty { state, reason } => {
                assert_eq!(state.generation.age, 0);
                assert_eq!(state.value, "empty");
                assert!(reason.contains("disk full"));
            }
            _ => panic!("expected FallbackToEmpty"),
        }
    }

    // ── PERSIST-12: load() error reason captured for logging ──────────

    #[test]
    fn persist_12_load_error_reason_captured() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(1);

        let outcome = logic.initialize::<String, _>(
            Err(PersistError::Io("connection refused".into())),
            "empty".into(),
            &clock,
            |_, _| unreachable!(),
        );

        match outcome {
            StartupOutcome::FallbackToEmpty { reason, .. } => {
                assert!(reason.contains("connection refused"));
            }
            _ => panic!("expected FallbackToEmpty with reason"),
        }
    }

    // ── PERSIST-13: Same storage_version used as-is ───────────────────

    #[test]
    fn persist_13_same_version_used_as_is() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(1);

        let loaded = StateObject {
            generation: Generation::new(100, 7),
            storage_version: 1,
            value: "data_v1".to_string(),
            created_time: 500,
            modified_time: 600,
        };

        let outcome = logic.initialize(
            Ok(Some(loaded)),
            "default".into(),
            &clock,
            |_, _| unreachable!("no migration when versions match"),
        );

        let state = outcome.into_state();
        assert_eq!(state.generation, Generation::new(100, 7));
        assert_eq!(state.storage_version, 1);
        assert_eq!(state.value, "data_v1");
        assert_eq!(state.created_time, 500);
        assert_eq!(state.modified_time, 600);
    }

    // ── PERSIST-14: Different storage_version triggers migration ──────

    #[test]
    fn persist_14_different_version_triggers_migration() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(2);

        let loaded = StateObject {
            generation: Generation::new(100, 7),
            storage_version: 1,
            value: "old_format".to_string(),
            created_time: 500,
            modified_time: 600,
        };

        let outcome = logic.initialize(
            Ok(Some(loaded)),
            "default".into(),
            &clock,
            |old_value, old_version| {
                assert_eq!(old_version, 1);
                Ok(format!("migrated_from_{}", old_value))
            },
        );

        match &outcome {
            StartupOutcome::Migrated { state, from_version } => {
                assert_eq!(*from_version, 1);
                assert_eq!(state.value, "migrated_from_old_format");
                // Incarnation preserved from loaded state
                assert_eq!(state.generation, Generation::new(100, 7));
                assert_eq!(state.storage_version, 2); // updated to current
            }
            _ => panic!("expected Migrated"),
        }
    }

    // ── PERSIST-15: Migration failure falls back to empty ─────────────

    #[test]
    fn persist_15_migration_failure_fallback() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(2);

        let loaded = StateObject {
            generation: Generation::new(100, 7),
            storage_version: 1,
            value: "old_data".to_string(),
            created_time: 500,
            modified_time: 600,
        };

        let outcome = logic.initialize(
            Ok(Some(loaded)),
            "default".into(),
            &clock,
            |_, _| Err("incompatible schema".into()),
        );

        match &outcome {
            StartupOutcome::FallbackToEmpty { state, reason } => {
                assert_eq!(state.generation.age, 0);
                assert_eq!(state.value, "default");
                assert!(reason.contains("incompatible schema"));
                // New incarnation, not the loaded one
                assert_ne!(state.generation.incarnation, 100);
            }
            _ => panic!("expected FallbackToEmpty"),
        }
    }

    // ── PERSIST-16: Successful migration restores age and timestamps ──

    #[test]
    fn persist_16_migration_preserves_metadata() {
        let clock = make_clock();
        let logic = PersistenceStartupLogic::new(3);

        let loaded = StateObject {
            generation: Generation::new(42, 10),
            storage_version: 2,
            value: 100u64,
            created_time: 1000,
            modified_time: 2000,
        };

        let outcome = logic.initialize(
            Ok(Some(loaded)),
            0u64,
            &clock,
            |old_val, _| Ok(old_val + 1), // simple migration
        );

        let state = outcome.into_state();
        assert_eq!(state.generation, Generation::new(42, 10)); // age preserved
        assert_eq!(state.created_time, 1000);
        assert_eq!(state.modified_time, 2000);
        assert_eq!(state.storage_version, 3); // bumped to current
        assert_eq!(state.value, 101); // migrated
    }
}
