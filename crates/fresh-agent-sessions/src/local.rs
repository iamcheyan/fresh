//! [`LocalTarget`] — this machine, and the reference implementation.
//!
//! Also what the tests and the CLI use, so the trait is exercised by its own
//! crate rather than only by the editor.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use crate::target::{Entry, EntryKind, Platform, Target, WalkOpts};

/// The machine this process is running on.
#[derive(Debug, Clone)]
pub struct LocalTarget {
    name: String,
    home: PathBuf,
    vars: BTreeMap<String, String>,
    platform: Platform,
    uid: Option<u32>,
    path_dirs: Vec<PathBuf>,
    allow_commands: bool,
}

impl LocalTarget {
    /// Read from this process's environment. Commands are off; see
    /// [`Self::with_commands`].
    pub fn from_process() -> Self {
        Self {
            name: "local".to_string(),
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/")),
            vars: std::env::vars().collect(),
            platform: Platform::host(),
            uid: Some(current_uid()),
            path_dirs: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            allow_commands: false,
        }
    }

    /// A target rooted at `home` with nothing else set. For tests.
    ///
    /// The platform is the real host's, not a fixed one: this target reads the
    /// machine it is running on, so claiming to be Linux on Windows would make
    /// it judge that machine's own paths by rules the machine does not use.
    /// Use [`Self::with_platform`] to pretend otherwise on purpose.
    pub fn at(home: impl Into<PathBuf>) -> Self {
        Self {
            name: "local".to_string(),
            home: home.into(),
            vars: BTreeMap::new(),
            platform: Platform::host(),
            uid: Some(1000),
            path_dirs: Vec::new(),
            allow_commands: false,
        }
    }

    /// Let the scan run tools' own listing commands.
    ///
    /// Off by default: reading files is harmless, forking `tmux` because a
    /// library was linked is not.
    #[must_use]
    pub fn with_commands(mut self) -> Self {
        self.allow_commands = true;
        self
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    #[must_use]
    pub fn with_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    #[must_use]
    pub fn with_uid(mut self, uid: Option<u32>) -> Self {
        self.uid = uid;
        self
    }

    #[must_use]
    pub fn with_path_dirs(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.path_dirs = dirs.into_iter().collect();
        self
    }
}

impl Target for LocalTarget {
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
        let mut out = Vec::new();
        walk_dir(root, root, opts, cancel, 1, &mut out)?;
        Ok(out)
    }

    fn read_prefix(&self, path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
        use std::io::Read;

        let mut file = std::fs::File::open(path)?;
        let mut buffer = vec![0u8; max_bytes];
        let read = file.read(&mut buffer)?;
        buffer.truncate(read);
        Ok(buffer)
    }

    fn platform(&self) -> Platform {
        self.platform
    }

    fn uid(&self) -> Option<u32> {
        self.uid
    }

    fn env(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }

    fn stat(&self, path: &Path) -> Option<Entry> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Entry {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind: kind_of(&meta.file_type()),
            modified_unix: modified(&meta).unwrap_or(0),
            size: meta.len(),
            path: path.to_path_buf(),
        })
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.path_dirs
            .iter()
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        if !self.allow_commands {
            return Err(format!("commands are disabled; not running {program}"));
        }
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|err| format!("{program}: {err}"))?;
        if !output.status.success() {
            return Err(format!("{program}: exited {}", output.status));
        }
        String::from_utf8(output.stdout).map_err(|err| format!("{program}: non-utf8 output: {err}"))
    }
}

fn walk_dir(
    root: &Path,
    dir: &Path,
    opts: &WalkOpts,
    cancel: &AtomicBool,
    depth: usize,
    out: &mut Vec<Entry>,
) -> std::io::Result<()> {
    if depth > opts.max_depth || out.len() >= opts.max_entries {
        return Ok(());
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    // A root that does not exist is "not installed", not an error to surface.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if dir == root => return Err(err),
        Err(_) => return Ok(()),
    };

    let mut children: Vec<_> = entries.flatten().collect();
    children.sort_by_key(std::fs::DirEntry::path);

    for child in children {
        if out.len() >= opts.max_entries {
            return Ok(());
        }
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        let name = child.file_name().to_string_lossy().into_owned();
        let path = child.path();
        let kind = kind_of(&file_type);
        // One metadata call, used for both fields: free here, because the
        // directory read already holds it.
        let meta = child.metadata().ok();
        out.push(Entry {
            path: path.clone(),
            name: name.clone(),
            kind,
            modified_unix: meta.as_ref().and_then(modified).unwrap_or(0),
            size: meta.as_ref().map_or(0, std::fs::Metadata::len),
        });
        // Symlinks are listed but never descended: one pointing back up the
        // tree would make this walk unbounded.
        if kind == EntryKind::Dir && !opts.skip_dirs.contains(&name) {
            walk_dir(root, &path, opts, cancel, depth + 1, out)?;
        }
    }
    Ok(())
}

fn kind_of(file_type: &std::fs::FileType) -> EntryKind {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_socket() {
            return EntryKind::Socket;
        }
    }
    if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}

fn modified(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, always succeeds and never sets
    // errno, so there is nothing to get wrong.
    unsafe { libc_getuid() }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::is_absolute_on;

    #[test]
    fn a_local_target_judges_its_own_machines_paths_by_that_machines_rules() {
        // This target reads the real local filesystem, so the platform it
        // reports has to be the real one. When it claimed to be Linux, a
        // genuine Windows path like `D:\...` was judged by unix rules, and
        // every `*_HOME` override on Windows was silently ignored.
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = LocalTarget::at(tmp.path());

        assert_eq!(target.platform(), Platform::host());

        let real_path = tmp.path().to_string_lossy().into_owned();
        assert!(
            is_absolute_on(target.platform(), &real_path),
            "a real local absolute path must read as absolute to the target \
             that produced it: {real_path}"
        );
    }

    #[test]
    fn with_platform_still_overrides_it_on_purpose() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = LocalTarget::at(tmp.path()).with_platform(Platform::Windows);
        assert_eq!(target.platform(), Platform::Windows);
    }
}
