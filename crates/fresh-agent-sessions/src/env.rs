//! The machine a scan runs against — injected, not assumed.
//!
//! Every provider reads paths under a home directory, consults environment
//! variables that relocate a tool's store, and occasionally runs a command.
//! All three go through [`Env`], for two reasons:
//!
//! * **Tests.** A test builds an [`Env`] pointing at a `tempfile::TempDir`
//!   laid out like a real install, and asserts on what the provider makes of
//!   it. Without that, testing this crate would mean writing into the
//!   developer's actual `~/.claude`.
//! * **Running commands is opt-in.** Reading files is harmless. Executing
//!   `tmux` is not something a library should do because it was linked, so it
//!   happens only when the caller calls [`Env::with_commands`].

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::host::{Host, HostEntry, LocalHost};

/// The operating system a scan is reporting about.
///
/// Explicit rather than read from `cfg!` at each use site, so a test can ask
/// "what would this provider say on macOS?" from a Linux CI runner — which is
/// the only way the macOS-only tools here get any test coverage at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// How to run a command, when commands are enabled.
pub trait CommandRunner: Send + Sync {
    /// Run `program` with `args` and return its stdout on success.
    ///
    /// Returns `Err` for anything that is not "exited 0 with stdout": the
    /// program missing, a non-zero exit, a signal. Providers treat all of
    /// those the same way — the live query did not work, fall back to files.
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

/// Runs commands with [`std::process::Command`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
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

/// A runner that answers from a fixed table. For tests.
#[derive(Debug, Default, Clone)]
pub struct StubRunner {
    responses: BTreeMap<String, Result<String, String>>,
}

impl StubRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `stdout` for this exact program-and-args invocation.
    #[must_use]
    pub fn returning(mut self, program: &str, args: &[&str], stdout: &str) -> Self {
        self.responses
            .insert(Self::key(program, args), Ok(stdout.to_string()));
        self
    }

    /// Fail this invocation, as a missing binary or a non-zero exit would.
    #[must_use]
    pub fn failing(mut self, program: &str, args: &[&str], error: &str) -> Self {
        self.responses
            .insert(Self::key(program, args), Err(error.to_string()));
        self
    }

    fn key(program: &str, args: &[&str]) -> String {
        std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join("\u{1}")
    }
}

impl CommandRunner for StubRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        self.responses
            .get(&Self::key(program, args))
            .cloned()
            .unwrap_or_else(|| Err(format!("{program}: no stub configured")))
    }
}

/// A runner that refuses everything, so a scan that has not opted into
/// commands cannot accidentally execute one.
#[derive(Debug, Default, Clone, Copy)]
struct NoCommands;

impl CommandRunner for NoCommands {
    fn run(&self, program: &str, _args: &[&str]) -> Result<String, String> {
        Err(format!("commands are disabled; not running {program}"))
    }
}

/// The environment a scan reads.
///
/// Holds the [`Host`] being scanned. By default that is this machine; give it
/// another with [`Env::with_host`] and the same providers scan an SSH host, a
/// container, or a fixture with no filesystem behind it at all.
pub struct Env {
    /// Configured by the builder methods, and used when `host` is unset. Kept
    /// concrete so `with_var`/`with_uid`/… stay available without needing a
    /// downcast.
    local: LocalHost,
    /// A caller-supplied host. When present it answers everything.
    host: Option<Arc<dyn Host>>,
    /// Overrides the host's own label. A caller scanning several machines
    /// names them the way its configuration does, which is what the user
    /// recognises in a list.
    machine: Option<String>,
    runner: Box<dyn CommandRunner>,
    commands_enabled: bool,
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("host", &self.host().label())
            .field("home", &self.home())
            .field("platform", &self.platform())
            .field("commands_enabled", &self.commands_enabled)
            .finish_non_exhaustive()
    }
}

impl Env {
    /// The real environment this process is running in.
    ///
    /// Commands are disabled; call [`Self::with_commands`] to enable them.
    pub fn from_process() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let vars = std::env::vars().collect::<BTreeMap<_, _>>();
        let path_dirs = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        Self {
            local: LocalHost {
                home,
                vars,
                platform: Platform::host(),
                uid: Some(current_uid()),
                path_dirs,
            },
            host: None,
            machine: None,
            runner: Box::new(NoCommands),
            commands_enabled: false,
        }
    }

    /// A synthetic environment rooted at `home`, with no variables set.
    pub fn fake(home: impl Into<PathBuf>) -> Self {
        Self {
            local: LocalHost {
                home: home.into(),
                vars: BTreeMap::new(),
                platform: Platform::Linux,
                uid: Some(1000),
                path_dirs: Vec::new(),
            },
            host: None,
            machine: None,
            runner: Box::new(NoCommands),
            commands_enabled: false,
        }
    }

    /// Name this machine, overriding the host's own label.
    ///
    /// Use the same name the caller's configuration uses. It ends up in every
    /// [`crate::SessionKey`], so it must be stable across scans — a label that
    /// changes turns every row into a new one.
    #[must_use]
    pub fn with_machine(mut self, machine: impl Into<String>) -> Self {
        self.machine = Some(machine.into());
        self
    }

    /// Scan `host` instead of this machine.
    ///
    /// This is the seam the editor wires an `Authority` onto — see
    /// [`crate::host`]. The host answers every filesystem question and may
    /// also answer [`Host::run`]; when it declines, and no [`CommandRunner`]
    /// was installed, providers report what files alone can prove.
    #[must_use]
    pub fn with_host(mut self, host: Arc<dyn Host>) -> Self {
        self.host = Some(host);
        self
    }

    /// The host being scanned.
    pub fn host(&self) -> &dyn Host {
        match &self.host {
            Some(host) => host.as_ref(),
            None => &self.local,
        }
    }

    /// The name of the scanned machine: the caller's, else the host's own.
    pub fn machine(&self) -> &str {
        self.machine
            .as_deref()
            .unwrap_or_else(|| self.host().label())
    }

    /// A short label for the scanned machine, for evidence and UI.
    pub fn host_label(&self) -> &str {
        self.host().label()
    }

    /// Whether the scan is looking at some machine other than this one.
    pub fn is_remote(&self) -> bool {
        self.host.is_some()
    }

    /// Allow providers to run their tools' own listing commands.
    ///
    /// This is what turns tmux discovery from "a socket file exists" into the
    /// actual live session list, and it is off by default because a library
    /// should not fork processes the caller did not ask for.
    #[must_use]
    pub fn with_commands(mut self, runner: impl CommandRunner + 'static) -> Self {
        self.runner = Box::new(runner);
        self.commands_enabled = true;
        self
    }

    /// Enable commands using [`ProcessRunner`].
    #[must_use]
    pub fn with_process_commands(self) -> Self {
        self.with_commands(ProcessRunner)
    }

    #[must_use]
    pub fn with_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.local.vars.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.local.platform = platform;
        self
    }

    #[must_use]
    pub fn with_uid(mut self, uid: u32) -> Self {
        self.local.uid = Some(uid);
        self
    }

    /// Directories to search for executables, in order.
    #[must_use]
    pub fn with_path_dirs(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.local.path_dirs = dirs.into_iter().collect();
        self
    }

    /// The scanned host's home directory.
    ///
    /// A host that cannot report one yields `/`, which makes every store path
    /// miss rather than silently resolving against this process's home and
    /// reporting local sessions as though they were the remote machine's.
    pub fn home(&self) -> PathBuf {
        self.host().home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }

    pub fn platform(&self) -> Platform {
        self.host().platform()
    }

    /// The scanned user's uid, when the host can say.
    pub fn uid(&self) -> Option<u32> {
        self.host().uid()
    }

    pub fn commands_enabled(&self) -> bool {
        self.commands_enabled
    }

    /// An environment variable's value, treating empty as unset.
    ///
    /// Why empty-is-unset: a wrapper that exports `CODEX_HOME=` has configured
    /// nothing, and resolving that to the process's cwd would make a scan
    /// report sessions from wherever it happened to be started.
    pub fn var(&self, name: &str) -> Option<String> {
        self.host()
            .var(name)
            .filter(|value| !value.trim().is_empty())
    }

    /// `$HOME` joined with `segments`.
    pub fn home_join<I, S>(&self, segments: I) -> PathBuf
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut path = self.home();
        for segment in segments {
            path.push(segment.as_ref());
        }
        path
    }

    /// The first of `names` that is set, as an absolute path, else `default`.
    ///
    /// A relative override is *rejected*, not resolved: tools spell these
    /// variables as absolute paths, and silently resolving a relative one
    /// against the scanning process's cwd would point the scan at a directory
    /// that has nothing to do with the user's install.
    pub fn dir_override(&self, names: &[&str], default: PathBuf) -> PathBuf {
        for name in names {
            if let Some(value) = self.var(name) {
                let path = PathBuf::from(&value);
                if path.is_absolute() {
                    return path;
                }
                tracing::debug!(
                    var = name,
                    %value,
                    "ignoring relative path override; expected an absolute path"
                );
            }
        }
        default
    }

    /// Run a command, if commands are enabled.
    ///
    /// An explicitly installed [`CommandRunner`] wins; otherwise the host is
    /// asked, which is how a remote adapter that *can* run commands gets used
    /// without the caller wiring a second thing.
    pub fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        if !self.commands_enabled {
            return Err(format!("commands are disabled; not running {program}"));
        }
        tracing::debug!(
            program,
            ?args,
            host = self.host_label(),
            "running listing command"
        );
        match self.runner.run(program, args) {
            Ok(output) => Ok(output),
            Err(err) if self.host.is_some() => self
                .host()
                .run(program, args)
                .map_err(|host_err| format!("{err}; host also declined: {host_err}")),
            Err(err) => Err(err),
        }
    }

    /// Find `program` on the scanned host's `PATH`.
    pub fn which(&self, program: &str) -> Option<PathBuf> {
        self.host().which(program)
    }

    /// Entries of `path`, sorted, from the scanned host.
    pub fn read_dir_sorted(&self, path: &Path) -> std::io::Result<Vec<HostEntry>> {
        self.host().read_dir_sorted(path)
    }

    /// A file's mtime on the scanned host.
    pub fn modified_unix(&self, path: &Path) -> Option<u64> {
        self.host()
            .metadata(path)
            .ok()
            .and_then(|meta| meta.modified_unix)
    }

    /// Now, as a unix timestamp.
    pub fn now_unix(&self) -> Option<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: `getuid` is documented as always succeeding and never touching
    // errno, and takes no arguments, so there is nothing to get wrong.
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
