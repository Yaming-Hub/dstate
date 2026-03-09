use crate::types::errors::MutationError;
use crate::types::node::NodeId;
use std::fmt;
use std::marker::PhantomData;

/// Messages for a StateShard managing a simple `DistributedState`.
/// State == View — no delta projection.
pub(crate) enum SimpleShardMsg<S: Clone + Send + 'static> {
    /// Apply a mutation to the local state.
    Mutate {
        closure: Box<dyn FnOnce(&mut S) + Send>,
        reply: tokio::sync::oneshot::Sender<Result<(), MutationError>>,
    },
    /// Inbound full snapshot from a peer (via SyncEngine).
    InboundSnapshot {
        source: NodeId,
        incarnation: u64,
        age: u64,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// A new node joined the cluster.
    NodeJoined(NodeId),
    /// A node left the cluster.
    NodeLeft(NodeId),
    /// ChangeFeed reports a peer has newer data.
    MarkStale {
        source: NodeId,
        incarnation: u64,
        age: u64,
    },
}

impl<S: Clone + Send + 'static> fmt::Debug for SimpleShardMsg<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mutate { .. } => f.debug_struct("Mutate").finish_non_exhaustive(),
            Self::InboundSnapshot {
                source,
                incarnation,
                age,
                wire_version,
                ..
            } => f
                .debug_struct("InboundSnapshot")
                .field("source", source)
                .field("incarnation", incarnation)
                .field("age", age)
                .field("wire_version", wire_version)
                .finish_non_exhaustive(),
            Self::NodeJoined(id) => f.debug_tuple("NodeJoined").field(id).finish(),
            Self::NodeLeft(id) => f.debug_tuple("NodeLeft").field(id).finish(),
            Self::MarkStale {
                source,
                incarnation,
                age,
            } => f
                .debug_struct("MarkStale")
                .field("source", source)
                .field("incarnation", incarnation)
                .field("age", age)
                .finish(),
        }
    }
}

/// Messages for a StateShard managing a `DeltaDistributedState`.
///
/// - `S`  — the authoritative state type (`DeltaDistributedState::State`)
/// - `V`  — the projected view type (`DeltaDistributedState::View`)
/// - `VD` — the view-delta type (`DeltaDistributedState::ViewDelta`)
/// - `DC` — the state-delta-change type (`DeltaDistributedState::StateDeltaChange`)
pub(crate) enum DeltaShardMsg<S, V, VD, DC>
where
    S: Clone + Send + 'static,
    V: Clone + Send + 'static,
    VD: Clone + Send + 'static,
    DC: Clone + Send + 'static,
{
    /// Apply a mutation to the local state, returning a delta change.
    MutateWithDelta {
        closure: Box<dyn FnOnce(&mut S) -> DC + Send>,
        reply: tokio::sync::oneshot::Sender<Result<(), MutationError>>,
    },
    /// Apply a mutation to the local state (no delta).
    Mutate {
        closure: Box<dyn FnOnce(&mut S) + Send>,
        reply: tokio::sync::oneshot::Sender<Result<(), MutationError>>,
    },
    /// Inbound full snapshot from a peer (via SyncEngine).
    InboundSnapshot {
        source: NodeId,
        incarnation: u64,
        age: u64,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// Inbound delta update from a peer.
    InboundDelta {
        source: NodeId,
        from_age: u64,
        to_age: u64,
        wire_version: u32,
        data: Vec<u8>,
    },
    /// A new node joined the cluster.
    NodeJoined(NodeId),
    /// A node left the cluster.
    NodeLeft(NodeId),
    /// ChangeFeed reports a peer has newer data.
    MarkStale {
        source: NodeId,
        incarnation: u64,
        age: u64,
    },
    /// Carries the `V` and `VD` type parameters.
    #[doc(hidden)]
    _Phantom(PhantomData<(V, VD)>),
}

impl<S, V, VD, DC> fmt::Debug for DeltaShardMsg<S, V, VD, DC>
where
    S: Clone + Send + 'static,
    V: Clone + Send + 'static,
    VD: Clone + Send + 'static,
    DC: Clone + Send + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MutateWithDelta { .. } => {
                f.debug_struct("MutateWithDelta").finish_non_exhaustive()
            }
            Self::Mutate { .. } => f.debug_struct("Mutate").finish_non_exhaustive(),
            Self::InboundSnapshot {
                source,
                incarnation,
                age,
                wire_version,
                ..
            } => f
                .debug_struct("InboundSnapshot")
                .field("source", source)
                .field("incarnation", incarnation)
                .field("age", age)
                .field("wire_version", wire_version)
                .finish_non_exhaustive(),
            Self::InboundDelta {
                source,
                from_age,
                to_age,
                wire_version,
                ..
            } => f
                .debug_struct("InboundDelta")
                .field("source", source)
                .field("from_age", from_age)
                .field("to_age", to_age)
                .field("wire_version", wire_version)
                .finish_non_exhaustive(),
            Self::NodeJoined(id) => f.debug_tuple("NodeJoined").field(id).finish(),
            Self::NodeLeft(id) => f.debug_tuple("NodeLeft").field(id).finish(),
            Self::MarkStale {
                source,
                incarnation,
                age,
            } => f
                .debug_struct("MarkStale")
                .field("source", source)
                .field("incarnation", incarnation)
                .field("age", age)
                .finish(),
            Self::_Phantom(_) => unreachable!(),
        }
    }
}
