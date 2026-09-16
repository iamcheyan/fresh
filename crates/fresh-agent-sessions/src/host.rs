//! The machine being scanned, as a trait — so a scan can run somewhere else.
//!
//! Every tool this crate knows about stores its state in a home directory on
//! some machine. Which machine is not this crate's business, so all filesystem
//! access goes through [`Host`] rather than `std::fs`. Point it at a different
//! implementation and the same providers scan an SSH host, a container, or a
//! fixture that never touches a disk.
//!
//! # Why a local trait instead of depending on the editor
//!
//! `fresh-editor` already has this concept: an `Authority` is its single slot
//! for "where does the editor act?", carrying a `FileSystem`, a
//! `ProcessSpawner` and a terminal wrapper, with local, SSH, container and
//! plugin-supplied backends behind it. Scanning a remote machine ought to
//! reuse that, not reinvent it.
//!
//! It does — but from the *other* side of the dependency. This crate defines
//! the small read-only interface it needs and the editor writes a dozen-line
//! adapter onto it. Depending on `fresh-editor` directly would be wrong three
//! times over:
//!
//! * **Direction.** `fresh-editor` is the top of this workspace. A library it
//!   might one day use cannot depend on it without a cycle.
//! * **Weight.** `Authority` reaches LSP, terminals, workspace trust and the
//!   env provider. This crate needs to list a directory and read the first few
//!   kilobytes of a file.
//! * **Read-only-ness.** `FileSystem` can write, truncate, patch and delete.
//!   This crate promises it never writes anything, and the cheapest way to
//!   keep a promise is to hold an interface that cannot express breaking it.
//!   Nothing below returns a writer.
//!
//! # Wiring it to an `Authority`
//!
//! The adapter lives in the editor, where both types are in scope:
//!
//! ```ignore
//! struct AuthorityHost {
//!     authority: Arc<Authority>,
//!     home: PathBuf,   // authority.filesystem.home_dir()
//!     platform: Platform,
//! }
//!
//! impl Host for AuthorityHost {
//!     fn home_dir(&self) -> Option<PathBuf> { Some(self.home.clone()) }
//!     fn platform(&self) -> Platform { self.platform }
//!
//!     fn read_dir(&self, path: &Path) -> io::Result<Vec<HostEntry>> {
//!         Ok(self.authority.filesystem.read_dir(path)?.into_iter()
//!             .map(|entry| HostEntry {
//!                 path: entry.path,
//!                 name: entry.name,
//!                 kind: match entry.entry_type { /* … */ },
//!             })
//!             .collect())
//!     }
//!
//!     fn read_file_prefix(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
//!         // `FileSystem::read_range` is exactly this, and over SSH it reads
//!         // the header instead of pulling a megabyte transcript across.
//!         self.authority.filesystem.read_range(path, 0, max)
//!     }
//!     // metadata / symlink_metadata map straight across.
//! }
//! ```
//!
//! `ProcessSpawner` is async while this trait is sync, so [`Host::run`] is the
//! one method an adapter may decline: leave it at its default and remote scans
//! fall back to reading files, which is most of what they do anyway. An editor
//! that wants live `tmux list-sessions` over SSH bridges to the spawner in its
//! own adapter, where the runtime it should block on is known.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::env::Platform;

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A symlink, *not* followed. Kept distinct because following one is how
    /// a walk over a home directory becomes unbounded.
    Symlink,
    Other,
}

/// One entry in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
}

/// The parts of a file's metadata this crate uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostMetadata {
    pub kind_is_dir: bool,
    pub kind_is_file: bool,
    /// Modification time as a unix timestamp, when the host reports one.
    pub modified_unix: Option<u64>,
}

/// A machine that can be scanned.
///
/// Implementations are read-only by construction: there is no method here that
/// writes, creates, moves or deletes.
pub trait Host: Send + Sync {
    /// A short label for evidence and UI — `"local"`, `"ssh://build-box"`.
    fn label(&self) -> &str {
        "local"
    }

    /// The home directory to resolve every tool's store against.
    fn home_dir(&self) -> Option<PathBuf>;

    /// An environment variable *on the scanned machine*.
    ///
    /// A remote adapter that cannot read the remote environment should return
    /// `None` rather than this process's value: reporting the local
    /// `CODEX_HOME` as the remote one would send the scan to a path that does
    /// not exist there, and label the result as though it did.
    fn var(&self, name: &str) -> Option<String>;

    fn platform(&self) -> Platform;

    /// The scanning user's uid, used for `/tmp/tmux-<uid>`. `None` when the
    /// host cannot say, which makes socket discovery unavailable rather than
    /// wrong.
    fn uid(&self) -> Option<u32>;

    fn read_dir(&self, path: &Path) -> io::Result<Vec<HostEntry>>;

    /// Metadata, following symlinks.
    fn metadata(&self, path: &Path) -> io::Result<HostMetadata>;

    /// Metadata *without* following symlinks.
    fn symlink_metadata(&self, path: &Path) -> io::Result<HostMetadata>;

    /// At most `max_bytes` from the start of a file.
    ///
    /// Bounded on purpose, and the only way this crate reads a file: transcripts
    /// grow to megabytes, everything identifying is in the first records, and
    /// over a network the difference between reading a prefix and reading the
    /// file is the difference between a usable picker and an unusable one.
    fn read_file_prefix(&self, path: &Path, max_bytes: usize) -> io::Result<Vec<u8>>;

    /// Find an executable on the host's `PATH`.
    fn which(&self, program: &str) -> Option<PathBuf>;

    /// Run a listing command on the host.
    ///
    /// Optional. The default declines, which is the correct answer for any
    /// host that cannot run commands synchronously — providers then report
    /// what the filesystem alone can prove.
    fn run(&self, program: &str, _args: &[&str]) -> Result<String, String> {
        Err(format!("{program}: this host cannot run commands"))
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.metadata(path).is_ok_and(|meta| meta.kind_is_dir)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.metadata(path).is_ok_and(|meta| meta.kind_is_file)
    }

    fn exists(&self, path: &Path) -> bool {
        self.metadata(path).is_ok()
    }

    /// `read_dir`, sorted by path, with unreadable directories reported as an
    /// error rather than as an empty one.
    ///
    /// Providers use this everywhere instead of `read_dir` so a truncated or
    /// partial scan is at least deterministic, rather than depending on
    /// whatever order the filesystem handed back.
    fn read_dir_sorted(&self, path: &Path) -> io::Result<Vec<HostEntry>> {
        let mut entries = self.read_dir(path)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }
}

/// The machine this process is running on.
#[derive(Debug, Clone)]
pub struct LocalHost {
    pub(crate) home: PathBuf,
    pub(crate) vars: std::collections::BTreeMap<String, String>,
    pub(crate) platform: Platform,
    pub(crate) uid: Option<u32>,
    pub(crate) path_dirs: Vec<PathBuf>,
}

impl Host for LocalHost {
    fn home_dir(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }

    fn var(&self, name: &str) -> Option<String> {
        self.vars
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
    }

    fn platform(&self) -> Platform {
        self.platform
    }

    fn uid(&self) -> Option<u32> {
        self.uid
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<HostEntry>> {
        std::fs::read_dir(path)?
            .map(|entry| {
                let entry = entry?;
                let path = entry.path();
                // `file_type` here does not follow symlinks, which is what the
                // walkers need to stay bounded.
                let kind = match entry.file_type() {
                    Ok(file_type) if file_type.is_dir() => EntryKind::Dir,
                    Ok(file_type) if file_type.is_file() => EntryKind::File,
                    Ok(file_type) if file_type.is_symlink() => EntryKind::Symlink,
                    Ok(_) => EntryKind::Other,
                    Err(_) => EntryKind::Other,
                };
                Ok(HostEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    path,
                    kind,
                })
            })
            .collect()
    }

    fn metadata(&self, path: &Path) -> io::Result<HostMetadata> {
        to_metadata(std::fs::metadata(path)?)
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<HostMetadata> {
        to_metadata(std::fs::symlink_metadata(path)?)
    }

    fn read_file_prefix(&self, path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
        use std::io::Read;

        let mut file = std::fs::File::open(path)?;
        let mut buffer = vec![0u8; max_bytes];
        let read = file.read(&mut buffer)?;
        buffer.truncate(read);
        Ok(buffer)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.path_dirs
            .iter()
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    }
}

fn to_metadata(meta: std::fs::Metadata) -> io::Result<HostMetadata> {
    Ok(HostMetadata {
        kind_is_dir: meta.is_dir(),
        kind_is_file: meta.is_file(),
        modified_unix: meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
    })
}

/// Test fixtures for scanning something that is not this machine.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A host with no filesystem at all, standing in for a remote one.
    ///
    /// This is the crate's proof that the [`Host`] port is real: a provider
    /// that still reached for `std::fs` would find nothing here, because
    /// nothing here exists on any disk.
    #[derive(Debug, Default)]
    pub struct MapHost {
        pub files: std::collections::BTreeMap<PathBuf, Vec<u8>>,
        pub dirs: std::collections::BTreeSet<PathBuf>,
    }

    impl Host for MapHost {
        fn label(&self) -> &str {
            "ssh://test-host"
        }
        fn home_dir(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/home/remote"))
        }
        fn var(&self, _name: &str) -> Option<String> {
            None
        }
        fn platform(&self) -> Platform {
            Platform::Linux
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn read_dir(&self, path: &Path) -> io::Result<Vec<HostEntry>> {
            if !self.dirs.contains(path) {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such directory"));
            }
            let mut out = Vec::new();
            for dir in &self.dirs {
                if dir.parent() == Some(path) {
                    out.push(HostEntry {
                        path: dir.clone(),
                        name: dir
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        kind: EntryKind::Dir,
                    });
                }
            }
            for file in self.files.keys() {
                if file.parent() == Some(path) {
                    out.push(HostEntry {
                        path: file.clone(),
                        name: file
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        kind: EntryKind::File,
                    });
                }
            }
            Ok(out)
        }
        fn metadata(&self, path: &Path) -> io::Result<HostMetadata> {
            if self.dirs.contains(path) {
                return Ok(HostMetadata {
                    kind_is_dir: true,
                    modified_unix: Some(1_700_000_000),
                    ..HostMetadata::default()
                });
            }
            if self.files.contains_key(path) {
                return Ok(HostMetadata {
                    kind_is_file: true,
                    modified_unix: Some(1_700_000_000),
                    ..HostMetadata::default()
                });
            }
            Err(io::Error::new(io::ErrorKind::NotFound, "no such path"))
        }
        fn symlink_metadata(&self, path: &Path) -> io::Result<HostMetadata> {
            self.metadata(path)
        }
        fn read_file_prefix(&self, path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
            let mut bytes = self
                .files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))?;
            bytes.truncate(max_bytes);
            Ok(bytes)
        }
        fn which(&self, _program: &str) -> Option<PathBuf> {
            None
        }
    }

    impl MapHost {
        /// Add a file, creating every parent directory.
        pub fn with_file(mut self, path: &str, body: &str) -> Self {
            let path = PathBuf::from(path);
            let mut parent = path.parent();
            while let Some(dir) = parent {
                self.dirs.insert(dir.to_path_buf());
                parent = dir.parent();
            }
            self.files.insert(path, body.as_bytes().to_vec());
            self
        }

        /// Add a directory, creating every parent.
        pub fn with_dir(mut self, path: &str) -> Self {
            let mut next = Some(PathBuf::from(path));
            while let Some(dir) = next {
                next = dir.parent().map(Path::to_path_buf);
                self.dirs.insert(dir);
            }
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MapHost;
    use super::*;

    #[test]
    fn a_host_that_cannot_run_commands_declines_rather_than_pretending() {
        let host = MapHost::default();
        let err = host.run("tmux", &["-V"]).expect_err("must decline");
        assert!(err.contains("cannot run commands"), "{err}");
    }

    #[test]
    fn read_file_prefix_is_bounded() {
        let host = MapHost::default().with_file("/a/big.jsonl", &"x".repeat(10_000));
        let prefix = host
            .read_file_prefix(Path::new("/a/big.jsonl"), 64)
            .expect("read");
        assert_eq!(prefix.len(), 64, "a transcript is never read whole");
    }
}
