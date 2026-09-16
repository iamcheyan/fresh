//! Wiring `fresh-agent-sessions` onto an [`Authority`].
//!
//! The scanner crate needs to read a home directory on *some* machine. The
//! editor already knows which machine that is — that is exactly what an
//! [`Authority`] is — so this module is the adapter between the two, and the
//! only place the editor and the scanner know about each other.
//!
//! The dependency points this way on purpose. `fresh-agent-sessions` declares
//! the small read-only interface it needs (`Host`) and knows nothing about the
//! editor; if it depended on `fresh-editor` instead, the editor could never
//! use it without a cycle. See that crate's `host` module.
//!
//! # Using it
//!
//! ```ignore
//! // One scan per configured machine; rows concatenate because every session
//! // key carries its machine.
//! let mut rows = Vec::new();
//! for (name, authority) in configured_authorities {
//!     rows.extend(collect_sessions(&authority, &name));
//! }
//! ```
//!
//! # What an authority can and cannot answer
//!
//! A `FileSystem` answers everything the scan reads off disk. Three things it
//! does not carry, and what the scanner does about each:
//!
//! * **The remote environment.** `CODEX_HOME` and friends are the *scanned*
//!   machine's, and an authority does not expose them, so [`AuthorityHost::var`]
//!   returns `None`. That is deliberate: answering with this process's value
//!   would aim a remote scan at a path that exists only here.
//! * **The remote uid.** Returned as `None`, which makes the scanner skip
//!   `/tmp/tmux-<uid>` rather than guess a uid and read another user's
//!   directory. tmux discovery over SSH therefore finds nothing until the
//!   spawner is wired up below.
//! * **Running commands.** `ProcessSpawner::spawn` is `async` and `Host::run`
//!   is sync, so [`AuthorityHost::run`] declines and the scan reports what
//!   files alone prove. Bridging it is a follow-up: `services::async_bridge`
//!   already exists for exactly this shape of problem, and only the live
//!   multiplexer listings depend on it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fresh_agent_sessions::{
    collect, scan, CollectedSession, EntryKind, Env, Host, HostEntry, HostMetadata, Platform,
};
use fresh_editor_core::model::filesystem::{EntryType, FileSystem};

use crate::services::authority::Authority;

/// An [`Authority`] presented as a machine the session scanner can read.
pub struct AuthorityHost {
    filesystem: Arc<dyn FileSystem + Send + Sync>,
    label: String,
    home: Option<PathBuf>,
    platform: Platform,
}

impl AuthorityHost {
    /// Build an adapter for `authority`, labelled `label`.
    ///
    /// `home` is resolved once here rather than per call: it cannot change for
    /// the life of an authority, and over SSH it costs a round trip.
    pub fn new(authority: &Authority, label: impl Into<String>) -> Self {
        let filesystem = Arc::clone(&authority.filesystem);
        let home = filesystem.home_dir().ok();
        // An authority does not report the remote platform. Everything the
        // scanner targets on a remote host is unix-shaped, and the one
        // platform-specific decision it makes (where desktop apps keep their
        // support directories) is only reachable for a local authority anyway.
        let platform = if authority.display_label.is_empty() {
            Platform::host()
        } else {
            Platform::Linux
        };
        Self {
            filesystem,
            label: label.into(),
            home,
            platform,
        }
    }
}

impl Host for AuthorityHost {
    fn label(&self) -> &str {
        &self.label
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.home.clone()
    }

    fn var(&self, _name: &str) -> Option<String> {
        // See the module docs: this process's environment is not the scanned
        // machine's, and guessing is worse than not knowing.
        None
    }

    fn platform(&self) -> Platform {
        self.platform
    }

    fn uid(&self) -> Option<u32> {
        None
    }

    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<HostEntry>> {
        Ok(self
            .filesystem
            .read_dir(path)?
            .into_iter()
            .map(|entry| HostEntry {
                path: entry.path,
                name: entry.name,
                kind: match entry.entry_type {
                    EntryType::Directory => EntryKind::Dir,
                    EntryType::File => EntryKind::File,
                    EntryType::Symlink => EntryKind::Symlink,
                },
            })
            .collect())
    }

    fn metadata(&self, path: &Path) -> std::io::Result<HostMetadata> {
        let meta = self.filesystem.metadata(path)?;
        let is_dir = self.filesystem.is_dir(path).unwrap_or(false);
        Ok(HostMetadata {
            kind_is_dir: is_dir,
            kind_is_file: !is_dir,
            modified_unix: meta.modified.and_then(|time| {
                time.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            }),
        })
    }

    fn symlink_metadata(&self, path: &Path) -> std::io::Result<HostMetadata> {
        let meta = self.filesystem.symlink_metadata(path)?;
        let is_dir = self.filesystem.is_dir(path).unwrap_or(false);
        Ok(HostMetadata {
            kind_is_dir: is_dir,
            kind_is_file: !is_dir,
            modified_unix: meta.modified.and_then(|time| {
                time.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            }),
        })
    }

    fn read_file_prefix(&self, path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
        // `read_range` is the whole reason the scanner reads prefixes: over a
        // remote filesystem this fetches the header instead of pulling a
        // multi-megabyte transcript across the wire.
        self.filesystem.read_range(path, 0, max_bytes)
    }

    fn which(&self, _program: &str) -> Option<PathBuf> {
        // No remote `PATH` without running something. Returning `None` costs
        // only the version strings; the scanner still detects a tool from the
        // state directory it left behind.
        None
    }
}

/// Scan one machine and return its sessions, flattened for a list.
///
/// Stateless: call it per authority, whenever the caller wants, and append the
/// results. Every returned key carries `machine`, so rows from different
/// machines cannot collide.
pub fn collect_sessions(authority: &Authority, machine: &str) -> Vec<CollectedSession> {
    let host = AuthorityHost::new(authority, machine);
    let env = Env::from_process()
        .with_host(Arc::new(host))
        .with_machine(machine);
    collect(&scan(&env))
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
    fn a_local_authority_scans_this_machine() {
        let authority = local_authority();
        let host = AuthorityHost::new(&authority, "local");

        assert_eq!(host.label(), "local");
        assert!(
            host.home_dir().is_some(),
            "a local authority must resolve a home directory"
        );
        assert!(host.is_dir(&host.home_dir().expect("home")));
    }

    #[test]
    fn the_scanner_runs_against_an_authority_end_to_end() {
        // The point of the test is that this compiles and completes: the
        // adapter satisfies `Host`, and a scan driven through an `Authority`
        // returns rows whose keys carry the machine name.
        let authority = local_authority();
        let rows = collect_sessions(&authority, "test-machine");
        for row in &rows {
            assert_eq!(row.key.machine.as_str(), "test-machine");
        }
    }

    #[test]
    fn an_authority_declines_commands_rather_than_blocking_a_runtime() {
        let authority = local_authority();
        let host = AuthorityHost::new(&authority, "local");
        assert!(
            host.run("tmux", &["-V"]).is_err(),
            "ProcessSpawner is async; the sync port must decline, not block"
        );
    }
}
