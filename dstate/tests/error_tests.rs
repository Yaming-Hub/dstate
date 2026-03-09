/// ERR-01, ERR-02: Error propagation tests
use dstate::{MutationError, NodeId, QueryError, RegistryError};

#[test]
fn err_01_state_not_registered_propagates_through_query_error() {
    let reg_err = RegistryError::StateNotRegistered("my_state".into());
    let query_err: QueryError = reg_err.into();
    match query_err {
        QueryError::Registry(RegistryError::StateNotRegistered(name)) => {
            assert_eq!(name, "my_state");
        }
        other => panic!("expected Registry(StateNotRegistered), got {other:?}"),
    }
}

#[test]
fn err_02_type_mismatch_propagates_through_query_error() {
    let reg_err = RegistryError::TypeMismatch {
        name: "x".into(),
        expected: "TypeA",
        actual: "TypeB",
    };
    let query_err: QueryError = reg_err.into();
    match query_err {
        QueryError::Registry(RegistryError::TypeMismatch {
            name,
            expected,
            actual,
        }) => {
            assert_eq!(name, "x");
            assert_eq!(expected, "TypeA");
            assert_eq!(actual, "TypeB");
        }
        other => panic!("expected Registry(TypeMismatch), got {other:?}"),
    }
}

#[test]
fn stale_peer_error_includes_node_id() {
    let err = QueryError::StalePeer {
        node_id: NodeId(42),
        reason: "unreachable".into(),
    };
    let msg = format!("{err}");
    assert!(msg.contains("42"));
    assert!(msg.contains("unreachable"));
}

#[test]
fn mutation_error_persistence_failed() {
    let err = MutationError::PersistenceFailed("disk full".into());
    let msg = format!("{err}");
    assert!(msg.contains("disk full"));
}
