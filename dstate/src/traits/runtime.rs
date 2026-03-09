use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::types::node::NodeId;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Error returned by `ActorRef::send()`.
#[derive(Debug, Clone)]
pub struct ActorSendError(pub String);

impl fmt::Display for ActorSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "actor send failed: {}", self.0)
    }
}

impl std::error::Error for ActorSendError {}

/// Error returned by `ActorRef::request()`.
#[derive(Debug, Clone)]
pub enum ActorRequestError {
    /// The actor did not reply before the timeout expired.
    Timeout,
    /// The actor is no longer running.
    ActorStopped,
    /// Other send/receive failure.
    Other(String),
}

impl fmt::Display for ActorRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "request timed out"),
            Self::ActorStopped => write!(f, "actor stopped"),
            Self::Other(msg) => write!(f, "request failed: {msg}"),
        }
    }
}

impl std::error::Error for ActorRequestError {}

/// Error returned by `ProcessingGroup` operations.
#[derive(Debug, Clone)]
pub struct GroupError(pub String);

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "group error: {}", self.0)
    }
}

impl std::error::Error for GroupError {}

/// Error returned by `ClusterEvents` operations.
#[derive(Debug, Clone)]
pub struct ClusterError(pub String);

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cluster error: {}", self.0)
    }
}

impl std::error::Error for ClusterError {}

// ---------------------------------------------------------------------------
// Cluster events
// ---------------------------------------------------------------------------

/// Events emitted by the cluster membership system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterEvent {
    NodeJoined(NodeId),
    NodeLeft(NodeId),
}

// ---------------------------------------------------------------------------
// Abstract traits
// ---------------------------------------------------------------------------

/// A handle to a running actor that can receive messages of type `M`.
///
/// `ActorRef` is the primary communication mechanism between actors and between
/// external code and actors. Implementations must be cheaply cloneable and safe
/// to share across threads.
pub trait ActorRef<M: Send + 'static>: Clone + Send + Sync + 'static {
    /// Fire-and-forget: deliver `msg` to the actor's mailbox.
    fn send(&self, msg: M) -> Result<(), ActorSendError>;

    /// Request-reply: send `msg` and wait for a reply of type `R`.
    fn request<R: Send + 'static>(
        &self,
        msg: M,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<R, ActorRequestError>> + Send + '_>>;
}

/// Named groups of actors for broadcasting.
pub trait ProcessingGroup: Send + Sync + 'static {
    /// The concrete `ActorRef` type used by this runtime.
    type Ref<M: Send + 'static>: ActorRef<M>;

    /// Add an actor to a named group.
    fn join<M: Send + 'static>(
        &self,
        group_name: &str,
        actor: &Self::Ref<M>,
    ) -> Result<(), GroupError>;

    /// Remove an actor from a named group.
    fn leave<M: Send + 'static>(
        &self,
        group_name: &str,
        actor: &Self::Ref<M>,
    ) -> Result<(), GroupError>;

    /// Broadcast a message to all members of a named group.
    fn broadcast<M: Clone + Send + 'static>(
        &self,
        group_name: &str,
        msg: M,
    ) -> Result<(), GroupError>;

    /// Get all members of a named group.
    fn get_members<M: Send + 'static>(
        &self,
        group_name: &str,
    ) -> Result<Vec<Self::Ref<M>>, GroupError>;
}

/// Subscription to cluster membership events.
pub trait ClusterEvents: Send + Sync + 'static {
    /// Subscribe to cluster membership changes. The callback is invoked for
    /// each `NodeJoined` / `NodeLeft` event.
    fn subscribe(
        &self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> Result<(), ClusterError>;
}

/// A handle to a scheduled timer that can be cancelled.
pub trait TimerHandle: Send + 'static {
    /// Cancel the timer. Idempotent — calling cancel on an already-cancelled
    /// or fired timer is a no-op.
    fn cancel(self);
}

/// The top-level actor runtime abstraction. One instance per node.
///
/// Provides actor spawning, timer scheduling, and access to the processing
/// group and cluster event subsystems.
pub trait ActorRuntime: Send + Sync + 'static {
    /// The concrete actor reference type.
    type Ref<M: Send + 'static>: ActorRef<M>;

    /// The processing group implementation.
    type Group: ProcessingGroup<Ref<u8> = Self::Ref<u8>>;

    /// The cluster events implementation.
    type Events: ClusterEvents;

    /// The timer handle type.
    type Timer: TimerHandle;

    /// Spawn a new actor with the given name and message handler.
    ///
    /// The handler receives messages one at a time (mailbox serialization).
    /// `state` is the actor's initial mutable state, accessible inside the
    /// handler via the closure capture.
    fn spawn<M, H>(
        &self,
        name: &str,
        handler: H,
    ) -> Self::Ref<M>
    where
        M: Send + 'static,
        H: FnMut(M) + Send + 'static;

    /// Schedule a recurring message to `target` at the given interval.
    fn send_interval<M: Clone + Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        interval: Duration,
        msg: M,
    ) -> Self::Timer;

    /// Schedule a one-shot message to `target` after the given delay.
    fn send_after<M: Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        delay: Duration,
        msg: M,
    ) -> Self::Timer;

    /// Access the processing group subsystem.
    fn groups(&self) -> &Self::Group;

    /// Access the cluster event subsystem.
    fn cluster_events(&self) -> &Self::Events;
}
