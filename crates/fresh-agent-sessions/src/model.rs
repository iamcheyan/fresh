//! The vocabulary every provider reports in.
//!
//! Third-party tools disagree about almost everything — what a "session" is,
//! whether a workspace is a git worktree or a managed folder, whether a
//! transcript is one file or a directory — so this module fixes a small set of
//! shapes that each provider maps onto, and records *how* every field was
//! learned alongside it.
//!
//! # Why every record carries its evidence
//!
//! This crate reads formats nobody promised us. A path that held a session
//! yesterday can be a different shape after the tool updates, and a field we
//! infer (a working directory guessed from an encoded directory name, say) is
//! not the same kind of fact as one the tool wrote down. A bare list of
//! sessions cannot express that difference, so callers end up treating a guess
//! as gospel.
//!
//! Every record therefore carries [`Evidence`] — the concrete files, sockets,
//! processes and command outputs it came from — and a [`Confidence`]. A caller
//! rendering a picker can show only [`Confidence::Certain`] rows; a caller
//! debugging "why did it miss my session?" can print the evidence and go look.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A third-party tool this crate knows how to look for.
///
/// Not an exhaustive list of tools in the world — an exhaustive list of the
/// ones we have *reverse-engineered a storage format for*. Agents that only
/// appear as transcript directories live in [`crate::catalog`] under
/// [`AgentId`] instead; a `ToolId` is something that owns a session model of
/// its own (multiplexers and orchestrators), or an agent whose format needed a
/// bespoke reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ToolId {
    /// The terminal multiplexer. Sessions are live server state, not files.
    Tmux,
    /// GNU screen, for completeness: same "sockets in a directory" shape.
    Screen,
    /// zellij, likewise.
    Zellij,
    /// Herdr — tmux-shaped, but agent-aware and with a persisted session graph.
    Herdr,
    /// Orca — Electron ADE, one git worktree per task.
    Orca,
    /// Superset — desktop IDE plus CLI, workspaces are git worktrees.
    Superset,
    /// super.engineering — native macOS control plane, `sc` CLI.
    SuperEngineering,
    /// Claude Code, the CLI. Transcripts under `~/.claude/projects`.
    ClaudeCode,
    /// The Claude desktop application.
    ClaudeDesktop,
    /// The Codex CLI. Rollout transcripts under `$CODEX_HOME/sessions`.
    CodexCli,
    /// The Codex desktop application, which reads the same `CODEX_HOME`.
    CodexDesktop,
    /// A coding agent discovered purely from its transcript store.
    Agent(AgentId),
}

impl ToolId {
    /// A stable, lowercase identifier — the key callers should persist.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Screen => "screen",
            Self::Zellij => "zellij",
            Self::Herdr => "herdr",
            Self::Orca => "orca",
            Self::Superset => "superset",
            Self::SuperEngineering => "super-engineering",
            Self::ClaudeCode => "claude-code",
            Self::ClaudeDesktop => "claude-desktop",
            Self::CodexCli => "codex-cli",
            Self::CodexDesktop => "codex-desktop",
            Self::Agent(agent) => agent.as_str(),
        }
    }
}

/// A coding agent, identified independently of whoever is hosting it.
///
/// The same agent shows up under several tools at once: a Claude Code session
/// can be a row in `~/.claude/projects`, a pane in tmux, a pane in Herdr with
/// its session id recorded, and a worktree in Orca. Keeping the agent's
/// identity separate from the host's is what lets [`Discovery::correlate`]
/// notice those are one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AgentId {
    Claude,
    Codex,
    Cursor,
    Gemini,
    Copilot,
    Grok,
    OpenCode,
    Droid,
    Cline,
    Kimi,
    Devin,
    Hermes,
    Rovo,
    Pi,
    Omp,
    PrimeAgent,
    OpenClaw,
    Amp,
    Aider,
    Goose,
    Qwen,
}

impl AgentId {
    /// A stable, lowercase identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::Copilot => "copilot",
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
            Self::Droid => "droid",
            Self::Cline => "cline",
            Self::Kimi => "kimi",
            Self::Devin => "devin",
            Self::Hermes => "hermes",
            Self::Rovo => "rovo",
            Self::Pi => "pi",
            Self::Omp => "omp",
            Self::PrimeAgent => "prime-agent",
            Self::OpenClaw => "openclaw",
            Self::Amp => "amp",
            Self::Aider => "aider",
            Self::Goose => "goose",
            Self::Qwen => "qwen",
        }
    }

    /// The argv[0] spellings that identify this agent running in a pane.
    ///
    /// Deliberately the *bare* command names: a pane's `pane_current_command`
    /// is a basename, and matching on a full path would miss every install
    /// that is not the one we happened to test against.
    pub fn command_names(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude"],
            Self::Codex => &["codex", "codex-cli"],
            Self::Cursor => &["cursor-agent", "cursor"],
            Self::Gemini => &["gemini"],
            Self::Copilot => &["copilot"],
            Self::Grok => &["grok"],
            Self::OpenCode => &["opencode"],
            Self::Droid => &["droid"],
            Self::Cline => &["cline"],
            Self::Kimi => &["kimi", "kimi-code"],
            Self::Devin => &["devin"],
            Self::Hermes => &["hermes"],
            Self::Rovo => &["rovodev", "acli"],
            Self::Pi => &["pi"],
            Self::Omp => &["omp"],
            Self::PrimeAgent => &["prime", "prime-agent"],
            Self::OpenClaw => &["openclaw", "clawdbot"],
            Self::Amp => &["amp"],
            Self::Aider => &["aider"],
            Self::Goose => &["goose"],
            Self::Qwen => &["qwen"],
        }
    }

    /// Every agent this crate can recognise from a command name.
    pub const ALL: &'static [AgentId] = &[
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Gemini,
        Self::Copilot,
        Self::Grok,
        Self::OpenCode,
        Self::Droid,
        Self::Cline,
        Self::Kimi,
        Self::Devin,
        Self::Hermes,
        Self::Rovo,
        Self::Pi,
        Self::Omp,
        Self::PrimeAgent,
        Self::OpenClaw,
        Self::Amp,
        Self::Aider,
        Self::Goose,
        Self::Qwen,
    ];

    /// The agent whose CLI is spelled `command`, if any.
    ///
    /// `command` may be a full path or a bare name; only the basename is
    /// compared, and a `.exe` suffix is ignored so Windows panes match too.
    pub fn from_command(command: &str) -> Option<Self> {
        let base = command
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(command)
            .trim_end_matches(".exe");
        Self::ALL
            .iter()
            .copied()
            .find(|agent| agent.command_names().contains(&base))
    }
}

/// How a fact was learned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceKind {
    /// A file we read (a transcript, a state snapshot, a manifest).
    File,
    /// A directory whose *existence or name* carried the information.
    Directory,
    /// A unix socket. Present means "a server was here", not "it is alive" —
    /// a crashed server leaves its socket behind.
    Socket,
    /// An environment variable that relocated a store.
    EnvVar,
    /// Output of a command we ran, which only happens when the caller opted in
    /// via [`crate::env::Env::with_commands`].
    Command,
    /// An executable found on `PATH`.
    Executable,
}

/// One concrete thing we looked at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    /// The path, variable name or command line. Printable, and enough for a
    /// human to go and check the claim by hand.
    pub locator: String,
    /// What this particular thing told us, when that is not obvious.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Evidence {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::at(EvidenceKind::File, path)
    }

    pub fn dir(path: impl Into<PathBuf>) -> Self {
        Self::at(EvidenceKind::Directory, path)
    }

    pub fn socket(path: impl Into<PathBuf>) -> Self {
        Self::at(EvidenceKind::Socket, path)
    }

    pub fn executable(path: impl Into<PathBuf>) -> Self {
        Self::at(EvidenceKind::Executable, path)
    }

    pub fn env_var(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            kind: EvidenceKind::EnvVar,
            locator: name.into(),
            detail: Some(value.into()),
        }
    }

    pub fn command(line: impl Into<String>) -> Self {
        Self {
            kind: EvidenceKind::Command,
            locator: line.into(),
            detail: None,
        }
    }

    fn at(kind: EvidenceKind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            locator: path.into().to_string_lossy().into_owned(),
            detail: None,
        }
    }

    /// Attach what this evidence told us.
    #[must_use]
    pub fn saying(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// How much to trust a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    /// The tool wrote this down and we read it back: a `cwd` field in a
    /// transcript, a session name from `tmux list-sessions`.
    Certain,
    /// Derived from something the tool wrote, by a rule we verified against a
    /// real install — a session id parsed out of a `rollout-<ts>-<uuid>.jsonl`
    /// filename, say.
    Probable,
    /// Inferred from a lossy or undocumented signal: a working directory
    /// reconstructed from Claude's dash-encoded project directory, a pane
    /// assumed to host an agent because of its command name.
    Possible,
}

/// Whether the tool is on this machine at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Installed {
    /// We found state, an executable, or both.
    Present {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable: Option<PathBuf>,
    },
    /// Nothing of this tool is here.
    Absent,
    /// The tool exists but cannot be inspected on this platform — a macOS-only
    /// app seen from Linux. Distinguished from `Absent` so callers do not
    /// report "you don't have it" when the honest answer is "not from here".
    Unsupported { reason: String },
}

/// A unit of work with a directory: an Orca worktree, a Superset workspace, a
/// Herdr workspace, a plain git worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Unique within a [`ToolReport`]. The tool's own id where it has one.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The directory the workspace is checked out at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The repository this was created from, when the tool records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Whether the tool models this as a git worktree rather than a plain
    /// directory. `None` when the tool does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_worktree: Option<bool>,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// One coding-agent conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// The agent's own session id where we can read one, else a path-derived
    /// stand-in. Check `confidence` before treating it as resumable.
    pub id: String,
    pub agent: AgentId,
    /// The directory the agent was working in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The transcript file or directory, for callers that want to read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<PathBuf>,
    /// Transcript mtime as a unix timestamp — the only "last activity" signal
    /// every one of these formats agrees on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix: Option<u64>,
    /// Agent version, when the transcript records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    /// True when the transcript is a sub-agent's, not a top-level conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_subagent: bool,
    /// For a sub-agent, the id of the conversation that spawned it.
    ///
    /// Sub-agent transcripts record the *parent's* session id, so it cannot
    /// also serve as the row's own `id` without making `id` non-unique — and a
    /// non-unique id turns one host pane into N identical correlation links.
    /// The parent is kept here and `id` is made distinct instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// A multiplexer session: tmux, screen, zellij, Herdr.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxSession {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Creation time as a unix timestamp, where the tool reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_unix: Option<u64>,
    /// Whether a client is attached. `None` when we read a file rather than
    /// asking a running server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<bool>,
    /// The socket this session lives on, for multiplexers that have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<Pane>,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// One terminal inside a multiplexer session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// The foreground command, as the multiplexer spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The agent we believe is running here, from `command` or from the
    /// multiplexer's own agent tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// The agent session id this pane is attached to, when the multiplexer
    /// records one. Herdr does; tmux does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
}

/// Everything one tool told us.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReport {
    pub tool: ToolId,
    /// Human-facing name, for UI that should say "super.engineering" rather
    /// than `super-engineering`.
    pub display_name: String,
    pub installed: Installed,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<Workspace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_sessions: Vec<AgentSession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mux_sessions: Vec<MuxSession>,
    /// Where we looked, including the places that turned out to be empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    /// Non-fatal problems: an unreadable directory, a state file that did not
    /// parse. A provider never fails the whole scan; it reports and continues.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

impl ToolReport {
    /// An empty report for a tool that is not installed.
    pub fn absent(tool: ToolId, display_name: impl Into<String>) -> Self {
        Self {
            tool,
            display_name: display_name.into(),
            installed: Installed::Absent,
            workspaces: Vec::new(),
            agent_sessions: Vec::new(),
            mux_sessions: Vec::new(),
            evidence: Vec::new(),
            problems: Vec::new(),
        }
    }

    /// True when the tool left nothing behind at all. Callers rendering a
    /// summary usually want to drop these.
    pub fn is_empty(&self) -> bool {
        matches!(self.installed, Installed::Absent)
            && self.workspaces.is_empty()
            && self.agent_sessions.is_empty()
            && self.mux_sessions.is_empty()
    }

    pub fn note_problem(&mut self, problem: impl Into<String>) {
        self.problems.push(problem.into());
    }
}

/// A claim that two records from different tools describe the same thing.
///
/// Correlation is a *hypothesis*, never a merge: we never rewrite a provider's
/// record with another provider's belief, because when the two disagree the
/// disagreement is the interesting part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// `"<tool>/<id>"` of the hosting record — a multiplexer pane or workspace.
    pub host: String,
    /// `"<tool>/<id>"` of the hosted record — usually an agent session.
    pub hosted: String,
    pub reason: LinkReason,
    pub confidence: Confidence,
}

/// Why we think two records are the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkReason {
    /// A multiplexer recorded the agent's own session id. Unambiguous.
    RecordedSessionId,
    /// Both records name the same directory, and the agent matches.
    SameCwdAndAgent,
    /// The agent's cwd is inside the workspace's directory.
    CwdInsideWorkspace,
}

/// The result of a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    /// When the scan ran, as a unix timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_at_unix: Option<u64>,
    pub tools: Vec<ToolReport>,
    /// Cross-tool hypotheses, populated by [`Discovery::correlate`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

impl Discovery {
    /// Total agent sessions across every tool, counting duplicates.
    ///
    /// Duplicates are expected and are not a bug: one Claude Code conversation
    /// legitimately appears once as a transcript and once as a Herdr pane.
    /// [`Self::correlate`] is what tells you which rows are the same session.
    pub fn agent_session_count(&self) -> usize {
        self.tools.iter().map(|t| t.agent_sessions.len()).sum()
    }

    /// Reports for tools that left something behind.
    pub fn present_tools(&self) -> impl Iterator<Item = &ToolReport> {
        self.tools.iter().filter(|report| !report.is_empty())
    }

    /// Build the cross-tool [`Link`]s.
    ///
    /// Three rules, strongest first, and a pair is linked by the first rule
    /// that fires:
    ///
    /// 1. A multiplexer pane that recorded an agent session id matching a
    ///    transcript we found: [`LinkReason::RecordedSessionId`], certain.
    /// 2. A pane and a session sharing both cwd and agent:
    ///    [`LinkReason::SameCwdAndAgent`], probable — two agents of the same
    ///    kind in one directory would collide, which does happen.
    /// 3. A session whose cwd is under a workspace path:
    ///    [`LinkReason::CwdInsideWorkspace`], probable.
    ///
    /// Nested workspaces would let rule 3 fire more than once for a session,
    /// so it keeps only the longest (most specific) workspace path.
    pub fn correlate(&mut self) {
        let mut links = Vec::new();
        let sessions: Vec<(String, &AgentSession)> = self
            .tools
            .iter()
            .flat_map(|tool| {
                tool.agent_sessions
                    .iter()
                    .map(move |s| (format!("{}/{}", tool.tool.as_str(), s.id), s))
            })
            .collect();

        for tool in &self.tools {
            for mux in &tool.mux_sessions {
                for pane in &mux.panes {
                    let host = format!("{}/{}:{}", tool.tool.as_str(), mux.id, pane.id);
                    links.extend(pane_links(&host, pane, &sessions));
                }
            }
        }

        for tool in &self.tools {
            for workspace in &tool.workspaces {
                let Some(path) = workspace.path.as_deref() else {
                    continue;
                };
                let host = format!("{}/{}", tool.tool.as_str(), workspace.id);
                for (key, session) in &sessions {
                    if session
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| cwd.starts_with(path))
                    {
                        links.push(Link {
                            host: host.clone(),
                            hosted: key.clone(),
                            reason: LinkReason::CwdInsideWorkspace,
                            confidence: Confidence::Probable,
                        });
                    }
                }
            }
        }

        keep_most_specific_workspace_links(&mut links, self);
        self.links = links;
    }
}

/// Links from one pane, strongest rule first; at most one link per session.
fn pane_links(host: &str, pane: &Pane, sessions: &[(String, &AgentSession)]) -> Vec<Link> {
    if let Some(session_id) = pane.agent_session_id.as_deref() {
        let exact: Vec<Link> = sessions
            .iter()
            .filter(|(_, session)| session.id == session_id)
            .map(|(key, _)| Link {
                host: host.to_string(),
                hosted: key.clone(),
                reason: LinkReason::RecordedSessionId,
                confidence: Confidence::Certain,
            })
            .collect();
        if !exact.is_empty() {
            return exact;
        }
    }

    let (Some(cwd), Some(agent)) = (pane.cwd.as_deref(), pane.agent) else {
        return Vec::new();
    };
    sessions
        .iter()
        .filter(|(_, session)| session.agent == agent && session.cwd.as_deref() == Some(cwd))
        .map(|(key, _)| Link {
            host: host.to_string(),
            hosted: key.clone(),
            reason: LinkReason::SameCwdAndAgent,
            confidence: Confidence::Probable,
        })
        .collect()
}

/// Drop all but the deepest workspace claiming each session.
///
/// Why: `~/code/repo` and `~/code/repo/.worktrees/feature` are both prefixes of
/// a session running in the latter, and reporting both makes the session look
/// like it lives in two workspaces at once. The longer path is the real one.
fn keep_most_specific_workspace_links(links: &mut Vec<Link>, discovery: &Discovery) {
    // Keyed by the same `"<tool>/<id>"` string the links carry. Workspace ids
    // are only unique within a tool — "default" and "main" are not rare — so
    // keying on the bare id lets one tool's workspace supply another tool's
    // depth, and the session gets attributed to the wrong one.
    let mut depth_by_host: BTreeMap<String, usize> = BTreeMap::new();
    for tool in &discovery.tools {
        for workspace in &tool.workspaces {
            if let Some(path) = workspace.path.as_deref() {
                depth_by_host.insert(
                    format!("{}/{}", tool.tool.as_str(), workspace.id),
                    path.components().count(),
                );
            }
        }
    }

    let mut best: BTreeMap<String, usize> = BTreeMap::new();
    for link in links.iter() {
        if link.reason != LinkReason::CwdInsideWorkspace {
            continue;
        }
        let depth = host_depth(&link.host, &depth_by_host);
        best.entry(link.hosted.clone())
            .and_modify(|d| *d = (*d).max(depth))
            .or_insert(depth);
    }

    links.retain(|link| {
        link.reason != LinkReason::CwdInsideWorkspace
            || best
                .get(&link.hosted)
                .is_some_and(|best| *best == host_depth(&link.host, &depth_by_host))
    });
}

fn host_depth(host: &str, depth_by_host: &BTreeMap<String, usize>) -> usize {
    depth_by_host.get(host).copied().unwrap_or(0)
}
