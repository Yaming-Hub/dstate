use crate::types::errors::DeserializeError;
use crate::types::node::VersionMismatchPolicy;

/// Action the actor shell should take after a version mismatch on inbound sync.
///
/// Returned by [`VersioningLogic::on_deserialize_error`] when an inbound
/// snapshot or delta cannot be deserialized due to a wire version the local
/// node does not understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VersionMismatchAction {
    /// Keep the last successfully deserialized view; log a warning.
    /// The view becomes increasingly stale until the local node is upgraded.
    KeepStale {
        wire_version: u32,
        reason: String,
    },
    /// Remove the peer's view from the view map until a compatible version
    /// arrives. Queries will not see this peer.
    DropView {
        wire_version: u32,
        reason: String,
    },
}

/// Result of attempting to deserialize an inbound sync message.
///
/// The actor shell calls [`VersioningLogic::on_inbound`] with the raw bytes
/// and wire version. This enum tells it what happened and what to do next.
#[derive(Debug)]
pub(crate) enum InboundVersionResult<V> {
    /// Deserialization succeeded — apply the view/delta normally.
    Ok(V),
    /// Deserialization failed — take the prescribed action.
    VersionMismatch(VersionMismatchAction),
    /// Deserialization failed for a non-version reason (malformed data).
    MalformedData(String),
}

/// Pure logic for handling wire version compatibility and storage version
/// migration.
///
/// # Threading model
///
/// This struct is intended for single-actor use. It is owned exclusively by
/// a `StateShard` actor and accessed sequentially within that actor's
/// message loop — no internal locking is needed.
///
/// # Ownership and Data Flow
///
/// ```text
/// ┌──────────────────────────────────────────────────────────────────┐
/// │ StateShard actor (inbound sync path)                            │
/// │                                                                  │
/// │  receive SyncMessage(wire_version=V, data)                       │
/// │    └─ VersioningLogic.on_inbound(data, V, deserialize_fn)        │
/// │         │                                                        │
/// │         ├─ Ok(view) ──────────────► ShardCore.accept_inbound()   │
/// │         ├─ VersionMismatch ─────► match policy:                  │
/// │         │    ├─ KeepStale ────────► log warning, keep old view    │
/// │         │    └─ DropView ────────► remove peer from view map     │
/// │         └─ MalformedData ───────► log error, discard             │
/// └──────────────────────────────────────────────────────────────────┘
/// ```
///
/// # Who calls what
///
/// | Caller | Method | When |
/// |--------|--------|------|
/// | StateShard (inbound handler) | `on_inbound()` | On every inbound snapshot/delta |
/// | StateShard (inbound handler) | `on_deserialize_error()` | When deserialization fails |
/// | StateShard (startup) | `try_load_with_migration()` | Loading persisted state on startup |
#[allow(dead_code)]
pub(crate) struct VersioningLogic {
    state_name: String,
    policy: VersionMismatchPolicy,
    current_wire_version: u32,
    current_storage_version: u32,
}

#[allow(dead_code)]
impl VersioningLogic {
    /// Create a new `VersioningLogic` for a given state type.
    pub fn new(
        state_name: String,
        policy: VersionMismatchPolicy,
        current_wire_version: u32,
        current_storage_version: u32,
    ) -> Self {
        Self {
            state_name,
            policy,
            current_wire_version,
            current_storage_version,
        }
    }

    /// Attempt to deserialize an inbound view/delta using the provided function.
    ///
    /// If deserialization succeeds, returns `InboundVersionResult::Ok(value)`.
    /// If it fails with `UnknownVersion`, applies the configured policy.
    /// If it fails with `Malformed`, returns `MalformedData`.
    pub fn on_inbound<V, F>(
        &self,
        data: &[u8],
        wire_version: u32,
        deserialize_fn: F,
    ) -> InboundVersionResult<V>
    where
        F: FnOnce(&[u8], u32) -> Result<V, DeserializeError>,
    {
        match deserialize_fn(data, wire_version) {
            Ok(value) => InboundVersionResult::Ok(value),
            Err(e) => match e {
                DeserializeError::UnknownVersion(v) => {
                    InboundVersionResult::VersionMismatch(
                        self.on_deserialize_error(v),
                    )
                }
                DeserializeError::Malformed(msg) => {
                    InboundVersionResult::MalformedData(msg)
                }
            },
        }
    }

    /// Determine the action for a version mismatch based on the configured policy.
    pub fn on_deserialize_error(&self, wire_version: u32) -> VersionMismatchAction {
        let reason = format!(
            "state '{}': received wire_version={}, local outbound wire_version is {}",
            self.state_name, wire_version, self.current_wire_version,
        );
        match self.policy {
            VersionMismatchPolicy::KeepStale => VersionMismatchAction::KeepStale {
                wire_version,
                reason,
            },
            VersionMismatchPolicy::DropAndWait => VersionMismatchAction::DropView {
                wire_version,
                reason,
            },
        }
    }

    /// Attempt to load persisted state, migrating if the stored version differs
    /// from the current storage version.
    ///
    /// The decision is version-driven: if `stored_version` matches
    /// `current_storage_version`, the data is deserialized directly.
    /// If they differ, `migrate_fn` is called to transform the data.
    ///
    /// The caller provides two functions:
    /// - `deserialize_fn`: deserializes at the current storage version
    /// - `migrate_fn`: migrates from an older (or unknown) storage version
    ///
    /// Returns `Ok(state)` on success, or `Err` if deserialization/migration fails.
    pub fn try_load_with_migration<S, D, M>(
        &self,
        data: &[u8],
        stored_version: u32,
        deserialize_fn: D,
        migrate_fn: M,
    ) -> Result<S, DeserializeError>
    where
        D: FnOnce(&[u8], u32) -> Result<S, DeserializeError>,
        M: FnOnce(&[u8], u32) -> Result<S, DeserializeError>,
    {
        if stored_version == self.current_storage_version {
            deserialize_fn(data, stored_version)
        } else {
            migrate_fn(data, stored_version)
        }
    }

    /// The state name this logic handles.
    pub fn state_name(&self) -> &str {
        &self.state_name
    }

    /// The configured policy.
    pub fn policy(&self) -> &VersionMismatchPolicy {
        &self.policy
    }

    /// The current wire version this node supports.
    pub fn current_wire_version(&self) -> u32 {
        self.current_wire_version
    }

    /// The current storage version this node uses.
    pub fn current_storage_version(&self) -> u32 {
        self.current_storage_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keep_stale_logic() -> VersioningLogic {
        VersioningLogic::new(
            "test_state".into(),
            VersionMismatchPolicy::KeepStale,
            2, // current wire version
            1, // current storage version
        )
    }

    fn drop_and_wait_logic() -> VersioningLogic {
        VersioningLogic::new(
            "test_state".into(),
            VersionMismatchPolicy::DropAndWait,
            2,
            1,
        )
    }

    // Helper: deserialize function that supports V1 and V2.
    fn deserialize_v1_v2(data: &[u8], wire_version: u32) -> Result<String, DeserializeError> {
        match wire_version {
            1 => Ok(format!("v1:{}", String::from_utf8_lossy(data))),
            2 => Ok(format!("v2:{}", String::from_utf8_lossy(data))),
            v => Err(DeserializeError::UnknownVersion(v)),
        }
    }

    // Helper: deserialize that only supports V2.
    fn deserialize_v2_only(data: &[u8], wire_version: u32) -> Result<String, DeserializeError> {
        match wire_version {
            2 => Ok(format!("v2:{}", String::from_utf8_lossy(data))),
            v => Err(DeserializeError::UnknownVersion(v)),
        }
    }

    // Helper: deserialize that returns malformed error.
    fn deserialize_malformed(_data: &[u8], _wire_version: u32) -> Result<String, DeserializeError> {
        Err(DeserializeError::Malformed("corrupt data".into()))
    }

    // ── VER-06: Same wire_version: sync works ───────────────────

    #[test]
    fn ver_06_same_wire_version_sync_works() {
        let logic = keep_stale_logic();
        let result = logic.on_inbound(b"hello", 2, deserialize_v1_v2);
        match result {
            InboundVersionResult::Ok(val) => assert_eq!(val, "v2:hello"),
            _ => panic!("expected Ok"),
        }
    }

    // ── VER-07: Receiver supports older wire_version ────────────

    #[test]
    fn ver_07_receiver_supports_older_wire_version() {
        let logic = keep_stale_logic();
        // Sender sends V1, receiver supports V1+V2.
        let result = logic.on_inbound(b"data", 1, deserialize_v1_v2);
        match result {
            InboundVersionResult::Ok(val) => assert_eq!(val, "v1:data"),
            _ => panic!("expected Ok for supported older version"),
        }
    }

    // ── VER-08: Unknown version — KeepStale ─────────────────────

    #[test]
    fn ver_08_unknown_version_keep_stale() {
        let logic = keep_stale_logic();
        let result = logic.on_inbound(b"data", 3, deserialize_v1_v2);
        match result {
            InboundVersionResult::VersionMismatch(action) => {
                assert_eq!(action, VersionMismatchAction::KeepStale {
                    wire_version: 3,
                    reason: "state 'test_state': received wire_version=3, local outbound wire_version is 2".into(),
                });
            }
            _ => panic!("expected VersionMismatch"),
        }
    }

    // ── VER-09: Unknown version — DropAndWait ───────────────────

    #[test]
    fn ver_09_unknown_version_drop_and_wait() {
        let logic = drop_and_wait_logic();
        let result = logic.on_inbound(b"data", 3, deserialize_v1_v2);
        match result {
            InboundVersionResult::VersionMismatch(action) => {
                assert_eq!(action, VersionMismatchAction::DropView {
                    wire_version: 3,
                    reason: "state 'test_state': received wire_version=3, local outbound wire_version is 2".into(),
                });
            }
            _ => panic!("expected VersionMismatch with DropView"),
        }
    }

    // ── VER-10: Malformed data is not treated as version mismatch

    #[test]
    fn ver_10_malformed_data_not_version_mismatch() {
        let logic = keep_stale_logic();
        let result = logic.on_inbound(b"corrupt", 2, deserialize_malformed);
        match result {
            InboundVersionResult::MalformedData(msg) => {
                assert_eq!(msg, "corrupt data");
            }
            _ => panic!("expected MalformedData"),
        }
    }

    // ── VER-11: on_deserialize_error produces correct action ────

    #[test]
    fn ver_11_on_deserialize_error_actions() {
        let keep = keep_stale_logic();
        let action = keep.on_deserialize_error(5);
        assert!(matches!(action, VersionMismatchAction::KeepStale { wire_version: 5, .. }));

        let drop = drop_and_wait_logic();
        let action = drop.on_deserialize_error(5);
        assert!(matches!(action, VersionMismatchAction::DropView { wire_version: 5, .. }));
    }

    // ── VER-12: Phase 1 — old nodes read old, new nodes read both

    #[test]
    fn ver_12_phase1_read_both_send_old() {
        // Phase 1: WIRE_VERSION=1, but deserialize supports V1+V2.
        let logic = VersioningLogic::new("res".into(), VersionMismatchPolicy::KeepStale, 1, 1);

        // Old node sends V1 → accepted.
        let r1 = logic.on_inbound(b"data", 1, deserialize_v1_v2);
        assert!(matches!(r1, InboundVersionResult::Ok(_)));

        // New node sends V2 (shouldn't happen in phase 1, but receiver handles it).
        let r2 = logic.on_inbound(b"data", 2, deserialize_v1_v2);
        assert!(matches!(r2, InboundVersionResult::Ok(_)));
    }

    // ── VER-13: Phase 2 — all nodes read V2 after switch ────────

    #[test]
    fn ver_13_phase2_switch_to_v2() {
        // Phase 2: WIRE_VERSION=2, deserialize supports V1+V2.
        let logic = VersioningLogic::new("res".into(), VersionMismatchPolicy::KeepStale, 2, 1);

        // V2 message from another phase 2 node → accepted.
        let r1 = logic.on_inbound(b"data", 2, deserialize_v1_v2);
        assert!(matches!(r1, InboundVersionResult::Ok(ref v) if v == "v2:data"));

        // Straggler V1 message from a phase 1 node → still accepted (V1 support kept).
        let r2 = logic.on_inbound(b"data", 1, deserialize_v1_v2);
        assert!(matches!(r2, InboundVersionResult::Ok(ref v) if v == "v1:data"));
    }

    // ── VER-14: Phase 2 rollback — revert to phase 1 code ──────

    #[test]
    fn ver_14_phase2_rollback_to_phase1() {
        // Rolled-back node: WIRE_VERSION=1 (phase 1 code), reads V1+V2.
        let logic = VersioningLogic::new("res".into(), VersionMismatchPolicy::KeepStale, 1, 1);

        // Phase 2 nodes are sending V2 → rolled-back node can still read it.
        let r = logic.on_inbound(b"data", 2, deserialize_v1_v2);
        assert!(matches!(r, InboundVersionResult::Ok(ref v) if v == "v2:data"));
    }

    // ── VER-15: Phase 3 — V1 deserialization removed ────────────

    #[test]
    fn ver_15_phase3_v1_removed() {
        // Phase 3: WIRE_VERSION=2, deserialize only supports V2.
        let logic = VersioningLogic::new("res".into(), VersionMismatchPolicy::KeepStale, 2, 1);

        // V2 message → accepted.
        let r1 = logic.on_inbound(b"data", 2, deserialize_v2_only);
        assert!(matches!(r1, InboundVersionResult::Ok(ref v) if v == "v2:data"));

        // V1 message → rejected with KeepStale.
        let r2 = logic.on_inbound(b"data", 1, deserialize_v2_only);
        assert!(matches!(r2, InboundVersionResult::VersionMismatch(
            VersionMismatchAction::KeepStale { wire_version: 1, .. }
        )));
    }

    // ── VER-16: Persist and load at same storage_version ────────

    #[test]
    fn ver_16_persist_load_same_version() {
        let logic = keep_stale_logic();
        let data = b"persisted_state";
        let result = logic.try_load_with_migration(
            data,
            1, // stored at version 1, current is 1
            |d, v| {
                assert_eq!(v, 1);
                Ok(format!("loaded:{}", String::from_utf8_lossy(d)))
            },
            |_, v| Err(DeserializeError::UnknownVersion(v)),
        );
        assert_eq!(result.unwrap(), "loaded:persisted_state");
    }

    // ── VER-17: Load from older storage_version with migration ──

    #[test]
    fn ver_17_load_older_version_with_migration() {
        let logic = VersioningLogic::new("test".into(), VersionMismatchPolicy::KeepStale, 2, 2);
        let data = b"old_format_data";
        let result = logic.try_load_with_migration(
            data,
            1, // stored at V1, current is V2 → version mismatch, migrate_fn called
            |_, _| panic!("deserialize_fn should not be called when versions differ"),
            |d, v| {
                assert_eq!(v, 1);
                Ok(format!("migrated:{}", String::from_utf8_lossy(d)))
            },
        );
        assert_eq!(result.unwrap(), "migrated:old_format_data");
    }

    // ── VER-18: Load from unknown storage_version fails ─────────

    #[test]
    fn ver_18_load_unknown_version_fails_gracefully() {
        let logic = keep_stale_logic();
        let data = b"future_data";
        let result = logic.try_load_with_migration::<String, _, _>(
            data,
            99, // future version ≠ current → migrate_fn called, which also fails
            |_, _| panic!("deserialize_fn should not be called when versions differ"),
            |_, v| Err(DeserializeError::UnknownVersion(v)),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), DeserializeError::UnknownVersion(99));
    }

    // ── Accessors ───────────────────────────────────────────────

    #[test]
    fn accessors() {
        let logic = keep_stale_logic();
        assert_eq!(logic.state_name(), "test_state");
        assert_eq!(logic.policy(), &VersionMismatchPolicy::KeepStale);
        assert_eq!(logic.current_wire_version(), 2);
        assert_eq!(logic.current_storage_version(), 1);
    }
}
