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
    /// Whether the scan may run other tools' listing commands. Always false
    /// today — see [`Target::run`] below — and carried so the decision has one
    /// home when a spawner bridge lands.
    allow_commands: bool,
}

impl AuthorityTarget {
    /// Build an adapter for `authority`, named `name`.
    ///
    /// `home` is resolved once here: it cannot change for the life of an
    /// authority, and over SSH it costs a round trip.
    pub fn new(authority: &Authority, name: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::clone(&authority.filesystem),
            name,
            platform_of(authority),
            false,
        )
    }

    /// Build one from the pieces the off-loop capability handle carries.
    ///
    /// The off-loop worker holds a filesystem, not an `Authority` — by design,
    /// since it must not be able to reach editor state — so this is the
    /// constructor that path uses.
    pub fn from_parts(
        filesystem: Arc<dyn FileSystem + Send + Sync>,
        name: impl Into<String>,
        platform: Platform,
        allow_commands: bool,
    ) -> Self {
        let home = filesystem.home_dir().unwrap_or_default();
        Self {
            filesystem,
            name: name.into(),
            home,
            platform,
            allow_commands,
        }
    }
}

/// The platform an authority's machine runs.
///
/// Read from the authority's own backend descriptor, not guessed from a
/// display string: `session_spec` is what actually knows whether this is the
/// local host or something reached over a transport.
pub fn platform_of(authority: &Authority) -> Platform {
    match authority.session_spec() {
        SessionAuthoritySpec::Local => Platform::host(),
        // Everything the scanner targets remotely is unix-shaped, and the one
        // platform-specific decision it makes (where desktop apps keep their
        // support directories) is only reachable locally anyway.
        _ => Platform::Linux,
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
        // `FileSystem::walk_files` is the bulk primitive, and a remote
        // implementation runs it server-side — but it skips every entry whose
        // name starts with `.`, because it exists for project file search.
        // Every store this scans is a dotfile directory (`~/.claude`,
        // `~/.codex`, `~/.config/herdr`), so using it here returns nothing at
        // all.
        //
        // So this walks with `read_dir`, which is correct but costs one round
        // trip per directory. That is tolerable only because these trees are
        // deliberately shallow — `WalkOpts::max_depth` is 1 to 4 — and it is
        // the one thing in this integration that should not stay as it is.
        // The fix is a bulk walk that can be told to include hidden entries;
        // see the module docs.
        let mut out = Vec::new();
        self.walk_dir(root, root, opts, cancel, 1, &mut out)?;
        Ok(out)
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

impl AuthorityTarget {
    fn walk_dir(
        &self,
        root: &Path,
        dir: &Path,
        opts: &WalkOpts,
        cancel: &AtomicBool,
        depth: usize,
        out: &mut Vec<Entry>,
    ) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;

        if depth > opts.max_depth || out.len() >= opts.max_entries {
            return Ok(());
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let entries = match self.filesystem.read_dir(dir) {
            Ok(entries) => entries,
            // A missing root means "not installed"; an unreadable subdirectory
            // is not worth failing the whole walk over.
            Err(err) if dir == root => return Err(err),
            Err(_) => return Ok(()),
        };

        for entry in entries {
            if out.len() >= opts.max_entries {
                return Ok(());
            }
            let kind = kind_of(entry.entry_type);
            let from_listing = entry
                .metadata
                .as_ref()
                .and_then(|meta| meta.modified)
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            // `read_dir` fills metadata lazily, so fall back to a stat for the
            // files whose mtime the scan actually sorts on.
            let modified_unix = if from_listing == 0 && kind == EntryKind::File {
                self.filesystem
                    .metadata(&entry.path)
                    .ok()
                    .and_then(|meta| meta.modified)
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs())
            } else {
                from_listing
            };

            let path = entry.path.clone();
            let name = entry.name.clone();
            out.push(Entry {
                path: path.clone(),
                name: name.clone(),
                kind,
                modified_unix,
            });
            // Symlinks are listed but never descended.
            if kind == EntryKind::Dir && !opts.skip_dirs.iter().any(|skip| *skip == name) {
                self.walk_dir(root, &path, opts, cancel, depth + 1, out)?;
            }
        }
        Ok(())
    }
}

/// Map a `FileSystem` entry type onto the scanner's.
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

impl AuthorityTarget {
    /// Whether this target was built to allow listing commands.
    pub fn allows_commands(&self) -> bool {
        self.allow_commands
    }
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
