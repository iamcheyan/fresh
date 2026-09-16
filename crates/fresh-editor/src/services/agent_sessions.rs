//! Scanning a machine for agent sessions, through its [`Authority`].
//!
//! `fresh-agent-sessions` asks for one thing: a `Target`, meaning a machine it
//! can walk, read and optionally run a command on. The editor already knows
//! which machine that is — that is what an [`Authority`] is — so this module
//! is the adapter between the two, and the only place the editor and the
//! scanner know about each other.
//!
//! The dependency points this way deliberately. The scanner declares the
//! interface it needs and knows nothing about the editor; depending on
//! `fresh-editor` instead would be a cycle.
//!
//! # Using it
//!
//! ```ignore
//! // One scan per configured machine. Blocking — run it off the UI thread.
//! let result = collect_sessions_for(&authority, "build-box", &cancel);
//! if result.status.is_reachable() {
//!     rows.extend(result.sessions);
//! }
//! ```
//!
//! # What an authority can and cannot answer
//!
//! * **Walking** maps onto `FileSystem::walk_files`, which remote
//!   implementations already perform server-side — so a scan of a remote home
//!   directory is one round trip per store, not one per directory.
//! * **Reading** maps onto `FileSystem::read_range`, which is why the scanner
//!   only ever reads bounded prefixes: over SSH this fetches a transcript's
//!   header instead of pulling the whole file across.
//! * **The remote environment and uid** are not carried by an authority, so
//!   they are reported as absent. That is deliberate: answering with *this*
//!   process's values would send a remote scan to paths that exist only here
//!   and then label the results as the remote machine's. The cost is that a
//!   relocated store (`CODEX_HOME`) is missed and tmux's socket directory
//!   cannot be located remotely.
//! * **Running a command** is declined, because `ProcessSpawner::spawn` is
//!   async and `Target::run` is sync. File-based discovery is unaffected;
//!   only live multiplexer listings depend on it.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use fresh_agent_sessions::{
    collect_sessions, Entry, EntryKind, Platform, ScanResult, Target, WalkOpts,
};
use fresh_editor_core::model::filesystem::{EntryType, FileSystem};

use crate::services::authority::{Authority, SessionAuthoritySpec};

/// An [`Authority`] presented as a machine the session scanner can read.
pub struct AuthorityTarget {
    filesystem: Arc<dyn FileSystem + Send + Sync>,
    name: String,
    home: PathBuf,
    platform: Platform,
}

impl AuthorityTarget {
    /// Build an adapter for `authority`, named `name`.
    ///
    /// `home` is resolved once here: it cannot change for the life of an
    /// authority, and over SSH it costs a round trip.
    pub fn new(authority: &Authority, name: impl Into<String>) -> Self {
        let filesystem = Arc::clone(&authority.filesystem);
        let home = filesystem.home_dir().unwrap_or_default();
        // From the authority's own backend descriptor, not guessed from a
        // display string: `session_spec` is what actually knows whether this
        // is the local host or something reached over a transport.
        let platform = match authority.session_spec() {
            SessionAuthoritySpec::Local => Platform::host(),
            // Everything the scanner targets remotely is unix-shaped, and the
            // one platform-specific decision it makes (where desktop apps keep
            // their support directories) is only reachable locally anyway.
            _ => Platform::Linux,
        };
        Self {
            filesystem,
            name: name.into(),
            home,
            platform,
        }
    }
}

impl Target for AuthorityTarget {
    fn name(&self) -> &str {
        &self.name
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn walk(
        &self,
        root: &Path,
        opts: &WalkOpts,
        cancel: &AtomicBool,
    ) -> std::io::Result<Vec<Entry>> {
        // A missing root means "not installed", and `walk_files` on a missing
        // directory is an error, so answer that before walking.
        if !self.filesystem.is_dir(root).unwrap_or(false) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} is not a directory", root.display()),
            ));
        }

        let skip: Vec<&str> = opts.skip_dirs.iter().map(String::as_str).collect();
        let root_depth = root.components().count();
        let mut entries = Vec::new();

        // One server-side walk. `walk_files` yields files only, so the
        // directories between them are reconstructed from their paths rather
        // than by listing each level — which is the whole point: no round trip
        // per directory.
        let mut seen_dirs = std::collections::BTreeSet::new();
        self.filesystem
            .walk_files(root, &skip, cancel, &mut |path, _rel| {
                if entries.len() >= opts.max_entries {
                    return false;
                }
                if path.components().count() - root_depth > opts.max_depth {
                    return true;
                }
                let mut parent = path.parent();
                while let Some(dir) = parent {
                    if dir == root || !dir.starts_with(root) {
                        break;
                    }
                    if !seen_dirs.insert(dir.to_path_buf()) {
                        break;
                    }
                    entries.push(entry_at(dir, EntryKind::Dir, 0));
                    parent = dir.parent();
                }
                // `walk_files` carries no mtime, so it costs one stat per
                // *file* — not per directory, and not per level. Reducing this
                // further needs the metadata in the walk itself; see the
                // module docs.
                let modified = self
                    .filesystem
                    .metadata(path)
                    .ok()
                    .and_then(|meta| meta.modified)
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                entries.push(entry_at(path, EntryKind::File, modified));
                true
            })?;

        Ok(entries)
    }

    fn read_prefix(&self, path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
        // Over a remote filesystem this fetches the header instead of pulling
        // a multi-megabyte transcript across the wire.
        self.filesystem.read_range(path, 0, max_bytes)
    }

    fn platform(&self) -> Platform {
        self.platform
    }

    fn stat(&self, path: &Path) -> Option<Entry> {
        let meta = self.filesystem.metadata(path).ok()?;
        let is_dir = self.filesystem.is_dir(path).unwrap_or(false);
        Some(entry_at(
            path,
            if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::Unknown
            },
            meta.modified
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs()),
        ))
    }
}

fn entry_at(path: &Path, kind: EntryKind, modified_unix: u64) -> Entry {
    Entry {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kind,
        modified_unix,
        path: path.to_path_buf(),
    }
}

/// Map a `FileSystem` entry type onto the scanner's.
#[allow(dead_code, reason = "used once `walk_files` carries entry types")]
fn kind_of(entry_type: EntryType) -> EntryKind {
    match entry_type {
        EntryType::Directory => EntryKind::Dir,
        EntryType::File => EntryKind::File,
        EntryType::Symlink => EntryKind::Symlink,
    }
}

/// Scan one machine and return its sessions.
///
/// **Blocking.** Call it from a worker, not from the editor's event loop or an
/// async runtime thread, and deliver the result back over the async bridge.
/// `cancel` abandons a scan against a wedged connection.
pub fn collect_sessions_for(
    authority: &Authority,
    machine: &str,
    cancel: &AtomicBool,
) -> ScanResult {
    collect_sessions(&AuthorityTarget::new(authority, machine), cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::workspace_trust::WorkspaceTrust;

    fn local_authority() -> Authority {
        Authority::local(
            Arc::new(WorkspaceTrust::permissive()),
            Arc::new(crate::services::env_provider::EnvProvider::inactive()),
        )
    }

    #[test]
    fn a_local_authority_resolves_a_home_and_walks_it() {
        let authority = local_authority();
        let target = AuthorityTarget::new(&authority, "local");

        assert_eq!(target.name(), "local");
        assert!(!target.home().as_os_str().is_empty());
        assert_eq!(target.platform(), Platform::host());
    }

    #[test]
    fn the_scanner_runs_against_an_authority_end_to_end() {
        let authority = local_authority();
        let cancel = AtomicBool::new(false);
        let result = collect_sessions_for(&authority, "test-machine", &cancel);

        assert_eq!(result.machine, "test-machine");
        for session in &result.sessions {
            assert_eq!(session.key.machine.as_str(), "test-machine");
        }
    }

    #[test]
    fn an_authority_declines_commands_rather_than_blocking_a_runtime() {
        let authority = local_authority();
        let target = AuthorityTarget::new(&authority, "local");
        assert!(
            target.run("tmux", &["-V"]).is_err(),
            "ProcessSpawner is async; the sync port must decline, not block"
        );
    }

    #[test]
    fn a_cancelled_scan_stops_rather_than_finishing() {
        let authority = local_authority();
        let cancel = AtomicBool::new(true);
        let result = collect_sessions_for(&authority, "local", &cancel);
        assert!(
            result.sessions.is_empty(),
            "a pre-cancelled scan must not do the work"
        );
    }
}
