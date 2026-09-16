//! [`Target`] — the machine to scan, and the crate's entire input.
//!
//! Everything the scan needs comes through this one trait: who the machine is,
//! how to walk its filesystem, how to read files, and how to run a command on
//! it. The caller constructs it and the crate only asks questions.
//!
//! # Why one trait
//!
//! The caller already knows how it reached this machine — it built the SSH
//! connection, or the container exec, or nothing at all because it is local.
//! Splitting that knowledge into a filesystem here, an environment there and a
//! command runner somewhere else just makes the caller reassemble what it
//! already had in one piece. So: one object, constructed once, passed in.
//!
//! # Only three methods are mandatory
//!
//! [`Target::name`], [`Target::home`] and [`Target::walk`] have no default.
//! Everything else does, and every default is the conservative answer:
//!
//! * [`Target::run`] declines, so a target that cannot execute anything still
//!   gets complete file-based discovery — which is most of it.
//! * [`Target::env`] and [`Target::uid`] return `None`. A remote target that
//!   answered with *this* process's values would send the scan to paths that
//!   exist only here and then label the results as the remote machine's.
//! * [`Target::read_prefixes`] loops over [`Target::read_prefix`].
//!
//! So a minimal remote target is three methods, and each thing it cannot do
//! costs exactly the discovery that depends on it.
//!
//! # Bulk by design
//!
//! [`Target::walk`] returns a whole subtree in one call, and
//! [`Target::read_prefixes`] reads many files in one call. Neither has a
//! per-path counterpart in the hot path, because a scan that stats each file
//! individually is hundreds of round trips over SSH. A remote implementation
//! walks server-side; `fresh-editor`'s `FileSystem::walk_files` already does.
//!
//! # Reads are bounded
//!
//! There is no "read this whole file". Transcripts reach megabytes and
//! everything identifying is in the first records, so the only read this crate
//! can express is a bounded prefix.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

/// The operating system of the machine being scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl Platform {
    /// The platform this binary was compiled for.
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// Whether `value` is an absolute path *on `platform`*.
///
/// `Path::is_absolute` answers for the platform this binary was compiled for,
/// which is the wrong question here: a scan runs against a target that need
/// not share the host's path rules. A Windows editor scanning a Linux machine
/// must still read `/var/data` as absolute, and a Linux editor scanning a
/// Windows machine must still read `D:\\data` as absolute.
pub fn is_absolute_on(platform: Platform, value: &str) -> bool {
    if platform != Platform::Windows {
        return value.starts_with('/');
    }
    let bytes = value.as_bytes();
    // `\\server\share` (UNC), or a drive-qualified path like `C:\` or `C:/`.
    // A bare `\foo` is relative to the current drive, so it is not absolute.
    if value.starts_with("\\\\") {
        return true;
    }
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// What a walked entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Dir,
    /// A symlink, never followed — following one is how a walk over a home
    /// directory becomes unbounded.
    Symlink,
    /// A socket, where the target can tell. tmux and Herdr sockets are the
    /// reason this is distinguished at all.
    Socket,
    /// Something else entirely — a fifo, a device.
    Other,
    /// The target could not say. Treated as "might be a file" everywhere, so a
    /// transport that cannot report file types still discovers sessions.
    Unknown,
}

/// One entry from a walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// The final path component.
    pub name: String,
    pub kind: EntryKind,
    /// Modification time as a unix timestamp.
    ///
    /// Mandatory, so a walk always carries what the scan sorts by and there is
    /// no second round trip to fetch it. A target that genuinely cannot
    /// determine one supplies `0`, which sorts oldest — the walk itself must
    /// never become N stats to populate this.
    pub modified_unix: u64,
    /// Size in bytes, from the same walk.
    ///
    /// Carried for the same reason as the mtime, and used for a specific
    /// thing: a bounded read must not ask for more bytes than the file has.
    /// Some transports implement a ranged read with `read_exact`, which fails
    /// outright on a short file — and a transcript with a few records is far
    /// shorter than the header budget.
    pub size: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// Whether this could be a regular file. `Unknown` counts, so a transport
    /// that cannot report types still yields sessions.
    pub fn is_file_like(&self) -> bool {
        matches!(self.kind, EntryKind::File | EntryKind::Unknown)
    }
}

/// How far and where a walk may go.
#[derive(Debug, Clone)]
pub struct WalkOpts {
    /// Maximum depth below the root. Bounded because these roots live in
    /// `$HOME`, and an unbounded walk of a mis-set `*_HOME` variable would
    /// crawl the user's whole disk.
    pub max_depth: usize,
    /// Directory basenames to skip at every depth.
    pub skip_dirs: Vec<String>,
    /// Stop after this many entries. A walk that hits it is reported as
    /// truncated rather than silently short.
    pub max_entries: usize,
}

impl Default for WalkOpts {
    fn default() -> Self {
        Self {
            max_depth: 4,
            skip_dirs: Vec::new(),
            max_entries: 50_000,
        }
    }
}

/// The machine being scanned.
pub trait Target: Send + Sync {
    /// A stable name for this machine. It lands in every session key, so a
    /// name that changes between scans turns every row into a new one.
    fn name(&self) -> &str;

    /// The home directory to resolve every tool's store against.
    fn home(&self) -> PathBuf;

    /// Walk `root`, returning entries in one call.
    ///
    /// Returning `Err` for a missing root is normal and means "not installed".
    /// Implementations must honour `cancel`, which is how a scan against a
    /// wedged connection is abandoned.
    fn walk(
        &self,
        root: &Path,
        opts: &WalkOpts,
        cancel: &AtomicBool,
    ) -> std::io::Result<Vec<Entry>>;

    /// At most `max_bytes` from the start of a file.
    fn read_prefix(&self, path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>>;

    /// Read many prefixes at once.
    ///
    /// Override this wherever the transport can batch: it is called once per
    /// tool with every transcript the scan wants to identify, and the default
    /// turns that into one round trip each.
    fn read_prefixes(&self, reqs: &[(PathBuf, usize)]) -> Vec<std::io::Result<Vec<u8>>> {
        reqs.iter()
            .map(|(path, max)| self.read_prefix(path, *max))
            .collect()
    }

    fn platform(&self) -> Platform {
        Platform::host()
    }

    /// An environment variable *on the scanned machine*.
    fn uid(&self) -> Option<u32> {
        None
    }

    fn env(&self, _name: &str) -> Option<String> {
        None
    }

    /// Whether a single path exists and what it is.
    ///
    /// Only for the handful of probe-a-known-file checks that are not part of
    /// a walk. Never called per walked entry.
    fn stat(&self, path: &Path) -> Option<Entry>;

    /// Find an executable on the target's `PATH`.
    fn which(&self, _program: &str) -> Option<PathBuf> {
        None
    }

    /// Run a listing command on the target and return its stdout.
    ///
    /// The default declines. Live multiplexer state — `tmux list-sessions` and
    /// friends — is the only thing that depends on it; everything else is read
    /// from files.
    fn run(&self, program: &str, _args: &[&str]) -> Result<String, String> {
        Err(format!("{program}: this target cannot run commands"))
    }
}

/// Convenience: `home()` joined with `segments`.
pub fn home_join<I, S>(target: &dyn Target, segments: I) -> PathBuf
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut path = target.home();
    for segment in segments {
        path.push(segment.as_ref());
    }
    path
}

/// The first of `names` set on the target, as an absolute path, else `default`.
///
/// A relative override is rejected rather than resolved: tools spell these
/// variables as absolute paths, and resolving one against the scanning
/// process's cwd would aim the scan at an unrelated directory.
pub fn dir_override(
    target: &dyn Target,
    names: &[&str],
    default: PathBuf,
) -> (PathBuf, Option<String>) {
    for name in names {
        if let Some(value) = target.env(name).filter(|value| !value.trim().is_empty()) {
            if is_absolute_on(target.platform(), &value) {
                return (PathBuf::from(&value), Some((*name).to_string()));
            }
            tracing::debug!(
                var = name,
                %value,
                "ignoring relative path override; expected an absolute path"
            );
        }
    }
    (default, None)
}

/// A [`Target`] backed by an in-memory map, standing in for a remote machine.
///
/// The crate's proof that [`Target`] is a real boundary: nothing here exists
/// on any disk, so a provider that quietly reached for `std::fs` would find
/// nothing. Available outside `cfg(test)` so consumers can exercise their own
/// code against a scan without a machine to scan.
#[derive(Debug, Default, Clone)]
pub struct MapTarget {
    name: String,
    home: PathBuf,
    files: std::collections::BTreeMap<PathBuf, Vec<u8>>,
    dirs: std::collections::BTreeSet<PathBuf>,
    /// Canned command output, keyed by `program` plus its args.
    commands: std::collections::BTreeMap<String, String>,
    /// Executables this target reports on its `PATH`.
    executables: std::collections::BTreeSet<String>,
}

impl MapTarget {
    pub fn new(name: &str, home: &str) -> Self {
        let mut target = Self {
            name: name.to_string(),
            home: PathBuf::from(home),
            ..Self::default()
        };
        target.add_dirs(Path::new(home));
        target
    }

    /// Add a file, creating every parent directory.
    #[must_use]
    pub fn with_file(mut self, path: &str, body: &str) -> Self {
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            self.add_dirs(parent);
        }
        self.files.insert(path, body.as_bytes().to_vec());
        self
    }

    #[must_use]
    pub fn with_dir(mut self, path: &str) -> Self {
        self.add_dirs(Path::new(path));
        self
    }

    /// Canned stdout for one invocation. Also puts `program` on the PATH, so
    /// a provider that checks for the binary before running it finds one.
    #[must_use]
    pub fn with_command(mut self, program: &str, args: &[&str], stdout: &str) -> Self {
        self.commands
            .insert(command_key(program, args), stdout.to_string());
        self.executables.insert(program.to_string());
        self
    }

    /// Report `program` as installed without canning any output for it.
    #[must_use]
    pub fn with_executable(mut self, program: &str) -> Self {
        self.executables.insert(program.to_string());
        self
    }

    fn add_dirs(&mut self, dir: &Path) {
        let mut next = Some(dir.to_path_buf());
        while let Some(path) = next {
            next = path.parent().map(Path::to_path_buf);
            self.dirs.insert(path);
        }
    }

    fn entry_for(&self, path: &Path, kind: EntryKind) -> Entry {
        Entry {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind,
            modified_unix: 1_700_000_000,
            size: self.files.get(path).map_or(0, |body| body.len() as u64),
            path: path.to_path_buf(),
        }
    }
}

fn command_key(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join("\u{1}")
}

impl Target for MapTarget {
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
        _cancel: &AtomicBool,
    ) -> std::io::Result<Vec<Entry>> {
        if !self.dirs.contains(root) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} does not exist", root.display()),
            ));
        }
        let depth_of = |path: &Path| path.components().count() - root.components().count();
        let mut out = Vec::new();
        for dir in &self.dirs {
            if dir != root && dir.starts_with(root) && depth_of(dir) <= opts.max_depth {
                out.push(self.entry_for(dir, EntryKind::Dir));
            }
        }
        for file in self.files.keys() {
            if file.starts_with(root) && depth_of(file) <= opts.max_depth {
                out.push(self.entry_for(file, EntryKind::File));
            }
        }
        out.truncate(opts.max_entries);
        Ok(out)
    }

    fn read_prefix(&self, path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
        let mut bytes = self
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
        bytes.truncate(max_bytes);
        Ok(bytes)
    }

    fn platform(&self) -> Platform {
        Platform::Linux
    }

    fn uid(&self) -> Option<u32> {
        Some(1000)
    }

    fn stat(&self, path: &Path) -> Option<Entry> {
        if self.dirs.contains(path) {
            return Some(self.entry_for(path, EntryKind::Dir));
        }
        if self.files.contains_key(path) {
            return Some(self.entry_for(path, EntryKind::File));
        }
        None
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.executables
            .contains(program)
            .then(|| PathBuf::from("/usr/bin").join(program))
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        self.commands
            .get(&command_key(program, args))
            .cloned()
            .ok_or_else(|| format!("{program}: no canned output"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absoluteness_follows_the_target_not_the_host() {
        // The case that reached CI: a unix-absolute path judged by Windows
        // rules is not absolute, so an override on a *Windows* target is
        // rightly ignored — but the same value on a Linux target must be
        // honoured no matter which platform the scanner itself runs on.
        assert!(is_absolute_on(Platform::Linux, "/definitely/not/here"));
        assert!(is_absolute_on(Platform::MacOs, "/Users/x"));
        assert!(!is_absolute_on(Platform::Windows, "/definitely/not/here"));

        // And the mirror image: a Windows path is absolute for a Windows
        // target however the scanning host spells its own paths.
        assert!(is_absolute_on(Platform::Windows, "C:\\data"));
        assert!(is_absolute_on(Platform::Windows, "d:/data"));
        assert!(is_absolute_on(Platform::Windows, "\\\\server\\share"));
        assert!(!is_absolute_on(Platform::Linux, "C:\\data"));
    }

    /// A target that reports whatever platform and variable it is told to.
    struct Remote {
        platform: Platform,
        var: (&'static str, &'static str),
    }

    impl Target for Remote {
        fn name(&self) -> &str {
            "remote"
        }
        fn home(&self) -> PathBuf {
            PathBuf::from("/home/u")
        }
        fn walk(
            &self,
            _root: &Path,
            _opts: &WalkOpts,
            _cancel: &AtomicBool,
        ) -> std::io::Result<Vec<Entry>> {
            Ok(Vec::new())
        }
        fn read_prefix(&self, _path: &Path, _max_bytes: usize) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn platform(&self) -> Platform {
            self.platform
        }
        fn env(&self, name: &str) -> Option<String> {
            (name == self.var.0).then(|| self.var.1.to_string())
        }
        fn stat(&self, _path: &Path) -> Option<Entry> {
            None
        }
    }

    #[test]
    fn an_override_is_judged_by_the_targets_platform_not_the_scanners() {
        // The scan runs here; the machine is over there. A Windows target's
        // `D:\store` must be honoured, and a unix-shaped value on that same
        // target ignored — neither answer may depend on the host running this.
        let default = PathBuf::from("/default");

        let windows = Remote {
            platform: Platform::Windows,
            var: ("STORE", "D:\\store"),
        };
        assert_eq!(
            dir_override(&windows, &["STORE"], default.clone()),
            (PathBuf::from("D:\\store"), Some("STORE".to_string())),
        );

        let windows_given_unix_path = Remote {
            platform: Platform::Windows,
            var: ("STORE", "/store"),
        };
        assert_eq!(
            dir_override(&windows_given_unix_path, &["STORE"], default.clone()),
            (default.clone(), None),
            "a unix path is not absolute on Windows, so it is ignored"
        );

        let linux = Remote {
            platform: Platform::Linux,
            var: ("STORE", "/store"),
        };
        assert_eq!(
            dir_override(&linux, &["STORE"], default),
            (PathBuf::from("/store"), Some("STORE".to_string())),
        );
    }

    #[test]
    fn a_drive_relative_or_bare_relative_path_is_not_absolute() {
        // `\foo` names the current drive, and `C:foo` the current directory on
        // drive C — neither says where it is on its own.
        assert!(!is_absolute_on(Platform::Windows, "\\foo"));
        assert!(!is_absolute_on(Platform::Windows, "C:foo"));
        assert!(!is_absolute_on(Platform::Windows, "C:"));
        assert!(!is_absolute_on(Platform::Windows, ""));
        assert!(!is_absolute_on(Platform::Linux, "relative/path"));
        assert!(!is_absolute_on(Platform::Linux, ""));
    }
}
