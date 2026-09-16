//! Discover the agent sessions, workspaces and worktrees that *other* tools
//! are managing on this machine.
//!
//! A developer's machine now routinely runs several coding agents at once,
//! each under a different host: a Claude Code session in a tmux pane, two
//! Codex sessions in Orca worktrees, a Herdr workspace with four agents in it.
//! Every one of those hosts knows about its own sessions and nothing about
//! anyone else's. This crate reads all of their state and reports one list.
//!
//! ```no_run
//! use fresh_agent_sessions::{scan, Env};
//!
//! // Reading files only. Commands are opt-in; see below.
//! let discovery = scan(&Env::from_process());
//!
//! for tool in discovery.present_tools() {
//!     println!("{}: {} session(s)", tool.display_name, tool.agent_sessions.len());
//! }
//! ```
//!
//! # What "reverse-engineering" means here
//!
//! None of these formats are documented as interfaces, and none of them are
//! promised to stay put. The layouts in this crate were obtained three ways,
//! and each record says which:
//!
//! 1. **Checked against the tool's own source**, where it is published:
//!    Herdr's session snapshot handling (Apache-2.0), Orca's persisted state
//!    and its `ai-vault/session-scanner-agent-sources.ts` (MIT) — the latter a
//!    catalogue of ~18 agents' stores, and the single most useful artefact
//!    found while building this. Only format facts were taken from either; see
//!    `THIRD-PARTY-NOTES.md`.
//! 2. **Confirmed against a live install**: tmux 3.4, Claude Code 2.1.273 and
//!    Codex CLI 0.154.0 were installed and inspected while writing this, and
//!    their entries are marked as verified.
//! 3. **Taken on another tool's authority**, for the agents and platforms not
//!    reachable from here. These are the entries most likely to have drifted,
//!    and [`catalog::AgentStore::verified_against_install`] is `false` for
//!    every one of them.
//!
//! # Design
//!
//! **Read-only, and it stays that way.** Nothing here writes, moves or deletes
//! a file. The one capability that reaches outside the filesystem — running a
//! tool's own listing command, which is how tmux's live session list is
//! obtained — is off unless the caller calls [`Env::with_commands`].
//!
//! **Nothing is assumed about the machine.** Home directory, environment
//! variables, platform, uid and `PATH` all come from [`Env`], so a test can
//! describe an install in a `TempDir` and a Linux CI runner can exercise the
//! macOS-only paths.
//!
//! **Every fact carries its provenance.** Each record holds the [`Evidence`]
//! it came from and a [`Confidence`]. A working directory the agent wrote into
//! its own transcript is [`Confidence::Certain`]; one inferred from a pane's
//! foreground command is [`Confidence::Possible`]. Callers that need to be
//! right — a "resume this session" picker — filter on it; callers debugging a
//! miss print the evidence and go look at the path.
//!
//! **Duplicates are expected, and resolved by linking rather than merging.**
//! One conversation legitimately appears as a Claude Code transcript *and* a
//! Herdr pane. [`Discovery::correlate`] emits a [`Link`] saying so — exactly,
//! when the host recorded the agent's session id; as a hypothesis, when all we
//! share is a working directory. It never rewrites one provider's record with
//! another's belief, because when two tools disagree that disagreement is the
//! interesting part.
//!
//! # Privacy
//!
//! These stores hold the user's prompts and their agents' output. This crate
//! reads only enough of a transcript to identify it — the header records, a
//! bounded prefix — and never puts transcript *content* in a report. Files
//! holding credentials (Superset's `config.json`, its host manifests) are
//! reported as existing and are not read for their secrets: see
//! [`providers::orchestrators`].
//!
//! # The flat view a UI wants
//!
//! [`Discovery`] is shaped like the scan that produced it: grouped by tool,
//! with cross-tool hypotheses to one side. [`collect`] turns it into
//! [`CollectedSession`] rows — one per session, each answering every question
//! a list row can be asked without referring back to anything:
//!
//! ```no_run
//! # use std::sync::Arc;
//! use fresh_agent_sessions::{collect, scan, Env, Host};
//! # fn configured_machines() -> Vec<(String, Arc<dyn Host>)> { Vec::new() }
//! let mut rows = Vec::new();
//! for (name, host) in configured_machines() {
//!     let env = Env::from_process().with_host(host).with_machine(name);
//!     rows.extend(collect(&scan(&env)));
//! }
//! rows.sort_by_key(|row| std::cmp::Reverse(row.last_active_unix));
//! ```
//!
//! Each row names the machine it came from, the agent, the multiplexer or
//! orchestrator that probably owns it, every path involved, and how much of
//! that to believe. Scans are independent and stateless, so a caller drives
//! one per machine, on whatever schedule it likes, and concatenates.
//!
//! # Scanning another machine
//!
//! A scan is not tied to this machine. Everything it reads goes through the
//! [`Host`] port, so pointing it somewhere else scans somewhere else:
//!
//! ```no_run
//! # use std::sync::Arc;
//! use fresh_agent_sessions::{scan, Env, Host};
//! # fn build(_: ()) -> Arc<dyn Host> { unimplemented!() }
//! # let my_ssh_host = build(());
//! let discovery = scan(&Env::from_process().with_host(my_ssh_host));
//! ```
//!
//! `fresh-editor` already owns this concept — an `Authority` is its one slot
//! for "where does the editor act?", with local, SSH, container and
//! plugin-supplied backends behind it. The editor supplies a small adapter from
//! `Authority` to [`Host`]; this crate does not depend on the editor, which
//! would be a cycle, and holds a read-only interface so it cannot write even by
//! mistake. [`crate::host`] has the adapter sketch and the reasoning.
//!
//! What stays out of scope is *aggregation*: a scan reports one machine. A
//! caller that wants several runs one scan per host and keeps the results
//! apart, because two machines can hold the same session id and merging them
//! would invent a session that exists on neither.

pub mod catalog;
pub mod collected;
pub mod env;
pub mod host;
pub mod model;
pub mod providers;
pub mod scan;

pub use collected::{
    collect, CollectedSession, MachineId, Owner, OwnerKind, SessionKey, SessionKind, SessionPaths,
};
pub use env::{CommandRunner, Env, Platform, ProcessRunner, StubRunner};
pub use host::{EntryKind, Host, HostEntry, HostMetadata, LocalHost};
pub use model::{
    AgentId, AgentSession, Confidence, Discovery, Evidence, EvidenceKind, Installed, Link,
    LinkReason, MuxSession, Pane, ToolId, ToolReport, Workspace,
};
pub use scan::{scan, scan_with, ScanOptions};
