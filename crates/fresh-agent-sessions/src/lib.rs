//! Discover the agent sessions, workspaces and worktrees that *other* tools
//! are managing — on this machine or any machine the caller can reach.
//!
//! A developer's machine now routinely runs several coding agents at once,
//! each under a different host: a Claude Code session in a tmux pane, two
//! Codex sessions in Orca worktrees, a Herdr workspace with four agents in it.
//! Every one of those hosts knows about its own sessions and nothing about
//! anyone else's. This crate reads all of their state and returns one list.
//!
//! # The whole API
//!
//! Construct a [`Target`], call [`collect_sessions`], get data back:
//!
//! ```no_run
//! use fresh_agent_sessions::{collect_sessions, LocalTarget};
//! use std::sync::atomic::AtomicBool;
//!
//! let target = LocalTarget::from_process().with_commands();
//! let result = collect_sessions(&target, &AtomicBool::new(false));
//!
//! for session in &result.sessions {
//!     println!("{} — {}", session.key, session.display_title());
//! }
//! ```
//!
//! That is the surface. One trait in, one struct out. The crate holds no
//! state between calls, opens no connections of its own, and decides nothing
//! about scheduling or concurrency.
//!
//! # Several machines
//!
//! Run it once per machine and concatenate. Every [`SessionKey`] carries its
//! machine, which is what makes that safe: two machines can hold the same
//! session id — the same repo cloned twice, or a home directory on shared
//! storage — and a key without a machine in it would merge two different
//! conversations into one row.
//!
//! ```no_run
//! # use fresh_agent_sessions::{collect_sessions, Target};
//! # use std::sync::atomic::AtomicBool;
//! # fn configured() -> Vec<Box<dyn Target>> { Vec::new() }
//! let cancel = AtomicBool::new(false);
//! let mut rows = Vec::new();
//! for target in configured() {
//!     let result = collect_sessions(target.as_ref(), &cancel);
//!     if result.status.is_reachable() {
//!         rows.extend(result.sessions);
//!     }
//! }
//! rows.sort_by_key(|row| std::cmp::Reverse(row.last_active_unix));
//! ```
//!
//! # Blocking, on purpose
//!
//! [`collect_sessions`] is a blocking call, and [`Target`] is a synchronous
//! trait. That matches `fresh-editor`'s own `FileSystem`, which is sync and
//! whose remote implementation blocks on its transport — so a remote target
//! here is the pattern that subsystem already uses, not a new one.
//!
//! Callers on an async runtime run it on a blocking worker and deliver the
//! result back over whatever channel they already have. Cancellation is the
//! `AtomicBool`, checked throughout, so a scan against a wedged connection is
//! abandoned rather than waited on.
//!
//! Making the trait async would need `async_trait` for dyn-compatibility, turn
//! every provider and test async, and leave this crate async on top of a sync
//! `FileSystem` on top of an async transport. The only thing it would buy —
//! not occupying a thread — is worth nothing for a scan that runs a few times
//! a minute.
//!
//! # What it reads
//!
//! Terminal multiplexers (tmux, screen, zellij), agent orchestrators (Herdr,
//! Orca, Superset, super.engineering), the agents with bespoke formats (Claude
//! Code, Codex) and ~19 more through a declarative table in [`catalog`].
//!
//! # Design
//!
//! **Read-only, and it stays that way.** [`Target`] has no method that writes,
//! creates, moves or deletes. The promise is kept by the type, not by care.
//!
//! **Bulk, not chatty.** Listings go out as one [`Target::walk`] per store and
//! file reads batch through [`Target::read_prefixes`]. A scan that stats each
//! file individually is hundreds of round trips over SSH.
//!
//! **Bounded reads.** There is no "read this whole file". Transcripts reach
//! megabytes and everything identifying is in the first records.
//!
//! **Nothing is probed, executed or guessed that the caller did not provide.**
//! A target that cannot report the remote environment says so, and the scan
//! narrows rather than substituting this process's values and mislabelling the
//! result.
//!
//! **Every fact carries its provenance.** Each row holds the [`Evidence`] it
//! came from and a [`Confidence`]. A working directory the agent wrote into
//! its own transcript is [`Confidence::Certain`]; one inferred from a pane's
//! foreground command is [`Confidence::Possible`].
//!
//! **Duplicates are linked, not merged.** One conversation appears as a
//! transcript *and* as the pane running it. The transcript wins the row and
//! the pane becomes its [`Owner`], so the dock shows one session, not two.
//!
//! # Privacy
//!
//! These stores hold the user's prompts and their agents' output. This crate
//! reads only enough of a transcript to identify it and never puts transcript
//! *content* in a result. Files holding credentials are reported as existing
//! and are not read for their secrets.

pub mod catalog;
pub mod collected;
pub mod local;
pub mod machine;
pub mod model;
pub mod providers;
pub mod scan;
pub mod target;

use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

pub use collected::{
    CollectedSession, MachineId, Owner, OwnerKind, SessionKey, SessionKind, SessionPaths,
};
pub use local::LocalTarget;
pub use machine::Machine;
pub use model::{
    AgentId, AgentSession, Confidence, Discovery, Evidence, EvidenceKind, Installed, Link,
    LinkReason, MuxSession, Pane, ToolId, ToolReport, Workspace,
};
pub use scan::ScanOptions;
pub use target::{Entry, EntryKind, Platform, Target, WalkOpts};

/// Whether the machine could be read at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Status {
    Reachable,
    /// The machine could not be read. Distinguished from "no sessions" so a
    /// dead SSH host does not render as an idle machine.
    Unreachable {
        error: String,
    },
    /// The caller cancelled the scan; `sessions` holds whatever was collected
    /// before that.
    Cancelled,
}

impl Status {
    pub fn is_reachable(&self) -> bool {
        matches!(self, Self::Reachable)
    }
}

/// One machine's sessions, and whether the scan could see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    /// The machine's name, as [`Target::name`] gave it.
    pub machine: String,
    pub status: Status,
    /// Every session found, newest first, then by key — so an unchanged
    /// machine returns the same order twice and a UI does not flicker.
    pub sessions: Vec<CollectedSession>,
    /// Non-fatal problems: an unreadable directory, a state file that did not
    /// parse, a truncated listing. A scan never fails as a whole.
    ///
    /// Always serialised, even when empty. A consumer reads
    /// `result.problems.length`, and a field that silently disappears when it
    /// has nothing in it is a field that throws on the happy path.
    #[serde(default)]
    pub problems: Vec<String>,
    /// The grouped, per-tool view, for callers that want the raw report rather
    /// than the flat rows: which tools are installed, where their state is,
    /// and the cross-tool links behind each row's owner.
    pub discovery: Discovery,
}

/// Scan one machine.
///
/// Blocking; see the module docs. Never panics and never returns `Err`: a
/// machine that cannot be read comes back as [`Status::Unreachable`], and
/// anything smaller lands in [`ScanResult::problems`].
pub fn collect_sessions(target: &dyn Target, cancel: &AtomicBool) -> ScanResult {
    collect_sessions_with(target, cancel, &ScanOptions::default())
}

/// [`collect_sessions`] with explicit options.
pub fn collect_sessions_with(
    target: &dyn Target,
    cancel: &AtomicBool,
    options: &ScanOptions,
) -> ScanResult {
    let machine = Machine::new(target, cancel);
    let name = target.name().to_string();

    // The home directory is the root of every store path, so a target that
    // cannot produce one cannot be scanned at all — and that is exactly the
    // "host is down" case a caller needs to tell apart from "no sessions".
    let home = target.home();
    if home.as_os_str().is_empty() || !machine.is_dir(&home) {
        return ScanResult {
            machine: name,
            status: Status::Unreachable {
                error: format!("home directory {} is not readable", home.display()),
            },
            sessions: Vec::new(),
            problems: Vec::new(),
            discovery: Discovery::default(),
        };
    }

    let discovery = scan::run(&machine, options);
    let sessions = collected::collect(&discovery);
    let problems = discovery
        .tools
        .iter()
        .flat_map(|tool| {
            tool.problems
                .iter()
                .map(move |problem| format!("{}: {problem}", tool.display_name))
        })
        .collect();

    ScanResult {
        machine: name,
        status: if machine.cancelled() {
            Status::Cancelled
        } else {
            Status::Reachable
        },
        sessions,
        problems,
        discovery,
    }
}
