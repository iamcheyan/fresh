//! The scan's view of a [`Target`]: bulk calls out, cached answers in.
//!
//! Providers are written in terms of "is this a directory", "list this
//! directory", "what is this file's mtime" — the questions the formats
//! actually pose. Answering each of those with its own call is what makes a
//! remote scan hundreds of round trips.
//!
//! [`Machine`] closes that gap. Every listing goes out as one bulk
//! [`Target::walk`], and every entry it returns is remembered, so the
//! follow-up questions about those paths are answered from memory. A path
//! nothing has walked still falls back to [`Target::stat`], which is why the
//! handful of probe-a-known-file checks (`~/.claude.json`, an app bundle) stay
//! correct without any provider having to know where its answers come from.
//!
//! The cache lives for one scan and is thrown away with it. Nothing here is
//! shared between scans, which is what keeps [`crate::collect_sessions`] a
//! pure function of its target.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::target::{dir_override, home_join, Entry, EntryKind, Platform, Target, WalkOpts};

pub struct Machine<'a> {
    target: &'a dyn Target,
    cancel: &'a AtomicBool,
    /// Everything any walk has seen, keyed by path.
    index: RefCell<HashMap<PathBuf, Entry>>,
    /// Roots already walked, with the depth they were walked to, so a second
    /// request for the same subtree is free and a deeper one still works.
    walked: RefCell<HashMap<PathBuf, usize>>,
    /// Paths `stat` has already answered for, including misses.
    probed: RefCell<HashMap<PathBuf, Option<Entry>>>,
}

impl<'a> Machine<'a> {
    pub fn new(target: &'a dyn Target, cancel: &'a AtomicBool) -> Self {
        Self {
            target,
            cancel,
            index: RefCell::new(HashMap::new()),
            walked: RefCell::new(HashMap::new()),
            probed: RefCell::new(HashMap::new()),
        }
    }

    pub fn name(&self) -> &str {
        self.target.name()
    }

    pub fn platform(&self) -> Platform {
        self.target.platform()
    }

    pub fn uid(&self) -> Option<u32> {
        self.target.uid()
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// An environment variable on the scanned machine.
    pub fn var(&self, name: &str) -> Option<String> {
        self.env(name)
    }

    /// Entries directly inside `dir`, or an error if it cannot be listed.
    pub fn list_io(&self, dir: &Path) -> std::io::Result<Vec<Entry>> {
        if !self.is_dir(dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} is not a directory", dir.display()),
            ));
        }
        Ok(self.list(dir))
    }

    /// A bounded prefix, as an `io::Result` for call sites that match on it.
    pub fn read_prefix_io(&self, path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
        self.read_prefix(path, max_bytes)
            .ok_or_else(|| std::io::Error::other(format!("cannot read {}", path.display())))
    }

    pub fn env(&self, name: &str) -> Option<String> {
        self.target
            .env(name)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn home_join<I, S>(&self, segments: I) -> PathBuf
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        home_join(self.target, segments)
    }

    /// Resolve a store root, returning the variable that moved it, if any.
    pub fn dir_override(&self, names: &[&str], default: PathBuf) -> (PathBuf, Option<String>) {
        dir_override(self.target, names, default)
    }

    pub fn which(&self, program: &str) -> Option<PathBuf> {
        self.target.which(program)
    }

    pub fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        if self.cancelled() {
            return Err("scan cancelled".to_string());
        }
        tracing::debug!(
            program,
            ?args,
            machine = self.name(),
            "running listing command"
        );
        self.target.run(program, args)
    }

    /// Walk `root` to `max_depth`, caching every entry seen.
    ///
    /// Returns the entries below `root`, sorted by path, so a truncated scan
    /// is deterministic rather than however the target happened to order them.
    pub fn walk(&self, root: &Path, opts: &WalkOpts) -> Vec<Entry> {
        if self.cancelled() {
            return Vec::new();
        }
        // A shallower walk of an already-walked root is already answered.
        if let Some(depth) = self.walked.borrow().get(root) {
            if *depth >= opts.max_depth {
                return self.cached_under(root, opts.max_depth);
            }
        }
        let entries = match self.target.walk(root, opts, self.cancel) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };
        {
            let mut index = self.index.borrow_mut();
            for entry in &entries {
                index.insert(entry.path.clone(), entry.clone());
            }
            self.walked
                .borrow_mut()
                .insert(root.to_path_buf(), opts.max_depth);
        }
        let mut entries = entries;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries
    }

    /// Entries directly inside `dir`.
    pub fn list(&self, dir: &Path) -> Vec<Entry> {
        let opts = WalkOpts {
            max_depth: 1,
            ..WalkOpts::default()
        };
        let mut entries: Vec<Entry> = self
            .walk(dir, &opts)
            .into_iter()
            .filter(|entry| entry.path.parent() == Some(dir))
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries
    }

    /// Whatever is known about `path`, from a previous walk or a fresh probe.
    pub fn stat(&self, path: &Path) -> Option<Entry> {
        if let Some(entry) = self.index.borrow().get(path) {
            return Some(entry.clone());
        }
        if let Some(cached) = self.probed.borrow().get(path) {
            return cached.clone();
        }
        let found = self.target.stat(path);
        self.probed
            .borrow_mut()
            .insert(path.to_path_buf(), found.clone());
        found
    }

    pub fn is_dir(&self, path: &Path) -> bool {
        self.stat(path).is_some_and(|entry| entry.is_dir())
    }

    pub fn is_file(&self, path: &Path) -> bool {
        self.stat(path).is_some_and(|entry| entry.is_file_like())
    }

    pub fn exists(&self, path: &Path) -> bool {
        self.stat(path).is_some()
    }

    /// True when `path` is a socket, or might be one.
    ///
    /// A target that cannot report socket-ness says [`EntryKind::Unknown`],
    /// and inside a multiplexer's own socket directory that is close enough —
    /// the alternative is reporting no tmux servers at all over a transport
    /// that does not carry file types.
    pub fn is_socket_like(&self, path: &Path) -> bool {
        self.stat(path).is_some_and(|entry| {
            matches!(
                entry.kind,
                EntryKind::Socket | EntryKind::Unknown | EntryKind::File
            ) && !entry.is_dir()
        })
    }

    /// A file's mtime. `None` only when the path is unknown entirely.
    pub fn modified_unix(&self, path: &Path) -> Option<u64> {
        self.stat(path).map(|entry| entry.modified_unix)
    }

    pub fn read_prefix(&self, path: &Path, max_bytes: usize) -> Option<Vec<u8>> {
        if self.cancelled() {
            return None;
        }
        self.target.read_prefix(path, max_bytes).ok()
    }

    /// Read many prefixes in one call, in the order requested.
    pub fn read_prefixes(&self, reqs: &[(PathBuf, usize)]) -> Vec<Option<Vec<u8>>> {
        if self.cancelled() || reqs.is_empty() {
            return vec![None; reqs.len()];
        }
        self.target
            .read_prefixes(reqs)
            .into_iter()
            .map(Result::ok)
            .collect()
    }

    /// The first line of a file, read from a bounded prefix.
    pub fn read_first_line(&self, path: &Path, max_bytes: usize) -> Option<String> {
        let bytes = self.read_prefix(path, max_bytes)?;
        Some(first_line(&bytes))
    }

    fn cached_under(&self, root: &Path, _max_depth: usize) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .index
            .borrow()
            .values()
            .filter(|entry| entry.path.starts_with(root) && entry.path != root)
            .cloned()
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries
    }
}

/// The first line of `bytes`, or the whole thing when there is no newline —
/// a `.json` file has none before its closing brace.
pub fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match text.split_once('\n') {
        Some((line, _)) => line.to_string(),
        None => text.into_owned(),
    }
}

/// The first of `fields` present as a non-empty string in a JSON object.
pub fn json_field(text: &str, fields: &[&str]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    fields.iter().find_map(|field| {
        value
            .get(*field)
            .and_then(serde_json::Value::as_str)
            .filter(|found| !found.is_empty())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod testing {
    use super::*;
    use crate::local::LocalTarget;

    /// A cancel flag that is never set. Tests are not cancelled, and a static
    /// removes the borrow that would otherwise force every call site to hold
    /// its own.
    pub(crate) fn never_cancelled() -> &'static AtomicBool {
        static FLAG: AtomicBool = AtomicBool::new(false);
        &FLAG
    }

    impl Machine<'static> {
        /// A machine over a leaked [`LocalTarget`], so a test can build one in
        /// an expression. Test-only: the leak is bounded by the test binary.
        pub(crate) fn local(home: impl Into<std::path::PathBuf>) -> Self {
            Self::over(LocalTarget::at(home))
        }

        /// A machine over a caller-configured target, leaked as above.
        pub(crate) fn over(target: impl Target + 'static) -> Self {
            Machine::new(Box::leak(Box::new(target)), never_cancelled())
        }
    }
}
