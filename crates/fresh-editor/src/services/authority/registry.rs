//! The editor's open connections.
//!
//! Every live connection to a machine — the local one the editor booted on, a
//! born-attached SSH or Kubernetes window's, a container a plugin installed —
//! is registered here, and everything that reads a machine holds a reference
//! into this map rather than a connection of its own. A window's
//! `Arc<Connection>` and a plugin's machine handle are two references to one
//! entry, so a reconnect that replaces a window's connection is visible to
//! whoever else was reading it, and a carrier is torn down exactly once, when
//! the last reference goes.
//!
//! The registry owns the connections; nothing else does. That is what makes
//! "what is this editor connected to?" a question with one answer instead of a
//! walk over the windows map plus whatever handles a plugin happens to hold.

use std::collections::HashMap;
use std::sync::Arc;

use super::Connection;

/// A connection's identity in the registry. Stable for the connection's life
/// and never reused: a stale id resolves to `None` rather than to whatever was
/// opened next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(pub u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conn:{}", self.0)
    }
}

/// Every connection this editor has open.
#[derive(Default)]
pub struct ConnectionRegistry {
    open: HashMap<ConnectionId, Arc<Connection>>,
    /// Source of ids. Starts at 1 so 0 can mean "none" on a wire that needs it.
    next: u64,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self {
            open: HashMap::new(),
            next: 1,
        }
    }

    /// Register `connection` and hand back a reference to it.
    ///
    /// The caller gets the `Arc` to store (on a window, in a machine handle);
    /// the registry keeps its own, so an entry outlives every borrower and is
    /// removed by [`Self::prune`] once nothing else refers to it.
    pub fn register(&mut self, connection: Connection) -> (ConnectionId, Arc<Connection>) {
        self.share(Arc::new(connection))
    }

    /// Register a connection that is already behind an `Arc` — one built
    /// before the registry existed (the boot connection) or handed over by a
    /// caller that already shared it.
    pub fn share(&mut self, connection: Arc<Connection>) -> (ConnectionId, Arc<Connection>) {
        // An `Arc` already in the map is the same connection, not a new one:
        // registering it twice would leave two entries claiming one carrier.
        if let Some((id, existing)) = self
            .open
            .iter()
            .find(|(_, c)| Arc::ptr_eq(c, &connection))
            .map(|(id, c)| (*id, Arc::clone(c)))
        {
            return (id, existing);
        }
        let id = ConnectionId(self.next);
        self.next += 1;
        self.open.insert(id, Arc::clone(&connection));
        tracing::debug!(
            "connection registry: opened {id} ({})",
            label_of(&connection)
        );
        (id, connection)
    }

    /// The connection `id` names, if it is still open.
    pub fn get(&self, id: ConnectionId) -> Option<&Arc<Connection>> {
        self.open.get(&id)
    }

    /// The id of an `Arc` this registry handed out.
    pub fn id_of(&self, connection: &Arc<Connection>) -> Option<ConnectionId> {
        self.open
            .iter()
            .find(|(_, c)| Arc::ptr_eq(c, connection))
            .map(|(id, _)| *id)
    }

    /// Every open connection, id first. Unordered — the caller sorts if it is
    /// showing them to anyone.
    pub fn iter(&self) -> impl Iterator<Item = (ConnectionId, &Arc<Connection>)> {
        self.open.iter().map(|(id, c)| (*id, c))
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Drop the entries nothing else holds, tearing down their carriers, and
    /// report how many went.
    ///
    /// Reference counting decides, not bookkeeping: an entry whose only
    /// reference is the registry's own is a connection no window is attached
    /// to and no handle is reading. Called where references are known to have
    /// been dropped — a window closing, a machine handle closing — rather than
    /// on a timer, so a connection never outlives its last user by longer than
    /// the event that released it.
    pub fn prune(&mut self) -> usize {
        let before = self.open.len();
        self.open.retain(|id, connection| {
            let live = Arc::strong_count(connection) > 1;
            if !live {
                tracing::debug!(
                    "connection registry: closed {id} ({})",
                    label_of(connection)
                );
            }
            live
        });
        before - self.open.len()
    }
}

/// A connection's display label, or `local` for the unlabelled local one.
fn label_of(connection: &Connection) -> &str {
    let label = connection.authority.display_label.as_str();
    if label.is_empty() {
        "local"
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::authority::Authority;
    use crate::services::env_provider::EnvProvider;
    use crate::services::workspace_trust::WorkspaceTrust;

    fn local() -> Connection {
        Connection::plain(Authority::local(
            Arc::new(WorkspaceTrust::permissive()),
            Arc::new(EnvProvider::inactive()),
        ))
    }

    #[test]
    fn a_registered_connection_resolves_by_id() {
        let mut registry = ConnectionRegistry::new();
        let (id, connection) = registry.register(local());
        assert!(Arc::ptr_eq(
            registry.get(id).expect("the entry is open"),
            &connection
        ));
        assert_eq!(registry.id_of(&connection), Some(id));
    }

    #[test]
    fn sharing_the_same_arc_twice_is_one_entry() {
        let mut registry = ConnectionRegistry::new();
        let (first, connection) = registry.register(local());
        let (second, _) = registry.share(connection);
        assert_eq!(first, second, "the same connection keeps its id");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn prune_keeps_what_is_still_referenced_and_drops_what_is_not() {
        let mut registry = ConnectionRegistry::new();
        let (held_id, held) = registry.register(local());
        let (dropped_id, dropped) = registry.register(local());
        drop(dropped);

        assert_eq!(registry.prune(), 1, "only the unreferenced one goes");
        assert!(registry.get(held_id).is_some());
        assert!(registry.get(dropped_id).is_none());
        drop(held);
    }

    #[test]
    fn an_id_is_never_reused() {
        let mut registry = ConnectionRegistry::new();
        let (first, connection) = registry.register(local());
        drop(connection);
        registry.prune();
        let (second, _) = registry.register(local());
        assert_ne!(first, second, "a stale id must not resolve to a new entry");
    }
}
