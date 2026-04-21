use std::any::{Any, TypeId};
use std::collections::HashMap;

use crate::types::errors::RegistryError;
use crate::types::node::NodeId;

/// Type-erased interface for a registered state shard.
///
/// Each registered state type has one `AnyStateShard` implementation
/// stored in the `StateRegistry`. The trait allows the registry to
/// broadcast cluster events (node join/leave) without knowing the
/// concrete state type.
pub trait AnyStateShard: Send + Sync + 'static {
    /// The globally unique name for this state type.
    fn state_name(&self) -> &str;

    /// Handle a new node joining the cluster.
    fn on_node_joined(&self, node_id: NodeId);

    /// Handle a node leaving the cluster.
    fn on_node_left(&self, node_id: NodeId);

    /// Handle a mark-stale notification from the change feed.
    fn on_mark_stale(&self, source: NodeId, incarnation: u64, age: u64);

    /// Downcast to a concrete type.
    fn as_any(&self) -> &dyn Any;
}

/// Registry of all distributed state types on this node.
///
/// Stores type-erased `AnyStateShard` entries keyed by state name.
/// Provides lookup (with typed downcasting) and cluster-event broadcasting.
pub struct StateRegistry {
    shards: HashMap<String, (TypeId, Box<dyn AnyStateShard>)>,
}

impl StateRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            shards: HashMap::new(),
        }
    }

    /// Register a state shard.
    ///
    /// The type parameter `S` must match the concrete type of the shard.
    /// The `TypeId` is derived from `S` for later typed lookups via
    /// `lookup_typed`.
    ///
    /// Returns `Err(RegistryError::DuplicateName)` if a shard with the
    /// same name is already registered.
    pub fn register<S: AnyStateShard>(
        &mut self,
        shard: S,
    ) -> Result<(), RegistryError> {
        let name = shard.state_name().to_string();
        if self.shards.contains_key(&name) {
            return Err(RegistryError::DuplicateName(name));
        }
        let type_id = TypeId::of::<S>();
        self.shards.insert(name, (type_id, Box::new(shard)));
        Ok(())
    }

    /// Look up a shard by name (type-erased).
    pub fn lookup(&self, name: &str) -> Result<&dyn AnyStateShard, RegistryError> {
        self.shards
            .get(name)
            .map(|(_, shard)| shard.as_ref())
            .ok_or_else(|| RegistryError::StateNotRegistered(name.to_string()))
    }

    /// Look up a shard by name and downcast to a concrete type `T`.
    ///
    /// Returns `Err(TypeMismatch)` if the registered `TypeId` doesn't
    /// match `TypeId::of::<T>()`.
    pub fn lookup_typed<T: AnyStateShard + 'static>(
        &self,
        name: &str,
    ) -> Result<&T, RegistryError> {
        let (registered_tid, shard) = self
            .shards
            .get(name)
            .ok_or_else(|| RegistryError::StateNotRegistered(name.to_string()))?;

        let requested_tid = TypeId::of::<T>();
        if *registered_tid != requested_tid {
            return Err(RegistryError::TypeMismatch {
                name: name.to_string(),
                expected: std::any::type_name::<T>(),
                actual: "different type",
            });
        }

        shard
            .as_any()
            .downcast_ref::<T>()
            .ok_or_else(|| RegistryError::TypeMismatch {
                name: name.to_string(),
                expected: std::any::type_name::<T>(),
                actual: "downcast failed",
            })
    }

    /// Broadcast `NodeJoined` to all registered shards.
    pub fn broadcast_node_joined(&self, node_id: NodeId) {
        for (_, shard) in self.shards.values() {
            shard.on_node_joined(node_id.clone());
        }
    }

    /// Broadcast `NodeLeft` to all registered shards.
    pub fn broadcast_node_left(&self, node_id: NodeId) {
        for (_, shard) in self.shards.values() {
            shard.on_node_left(node_id.clone());
        }
    }

    /// Number of registered state types.
    pub fn len(&self) -> usize {
        self.shards.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.shards.is_empty()
    }

    /// Iterate over all registered state names.
    pub fn state_names(&self) -> impl Iterator<Item = &str> {
        self.shards.keys().map(|s| s.as_str())
    }
}

impl Default for StateRegistry {
    fn default() -> Self {
        Self::new()
    }
}
