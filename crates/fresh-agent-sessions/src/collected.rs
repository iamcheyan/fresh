//! The flat view: one self-contained row per session.
//!
//! [`crate::Discovery`] is shaped like the scan that produced it — grouped by
//! tool, with cross-tool hypotheses kept to one side. That shape is right for
//! reporting *what each tool said*, and wrong for the thing that actually
//! consumes this crate: a dock panel listing every session on every configured
//! machine, sorted by recency, filterable, one row at a time.
//!
//! Walking a tree and re-joining it against a link list, in a UI, for every
//! repaint, is work the caller should never have had to do. [`collect`] does
//! it once and hands back [`CollectedSession`] rows that answer every question
//! a row can be asked without referring to anything else: which machine, which
//! agent, which multiplexer probably owns it, where it is on disk, when it was
//! last touched, and how much of that to believe.
//!
//! # Rows are machine-qualified and safe to concatenate
//!
//! The intended use is one scan per configured machine, with the results
//! appended into a single list:
//!
//! ```no_run
//! use fresh_agent_sessions::{collect_sessions, Target};
//! # use std::sync::atomic::AtomicBool;
//! # fn configured() -> Vec<Box<dyn Target>> { Vec::new() }
//! let cancel = AtomicBool::new(false);
//! let mut rows = Vec::new();
//! for target in configured() {
//!     rows.extend(collect_sessions(target.as_ref(), &cancel).sessions);
//! }
//! rows.sort_by_key(|row| std::cmp::Reverse(row.last_active_unix));
//! ```
//!
//! That is only sound because every [`SessionKey`] carries its machine. Two
//! machines really can hold the same session id — the same repo cloned twice,
//! or a home directory on shared storage — and a key without a machine in it
//! would silently merge two different conversations into one row.
//!
//! # One session, one row
//!
//! A conversation frequently appears twice in a `Discovery`: once as the
//! agent's transcript, once as the multiplexer pane running it. Those are not
//! two sessions and must not be two rows. [`collect`] emits the transcript row
//! — the one with the resumable id, the working directory and the history —
//! and records the pane as its [`Owner`].
//!
//! A pane with *no* matching transcript is the opposite case and still gets a
//! row: an agent running in a tmux pane that has not written a transcript yet,
//! or whose transcript this scan could not read, is a real session someone
//! wants to jump to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{
    AgentId, Confidence, Discovery, Evidence, LinkReason, ToolId, ToolReport, Workspace,
};

/// The machine a row was collected from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MachineId(pub String);

impl MachineId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A session's identity, unique across machines, tools and scans.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    pub machine: MachineId,
    /// The tool whose store this row came from.
    pub tool: ToolId,
    /// Unique within that tool on that machine.
    pub id: String,
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.machine, self.tool.as_str(), self.id)
    }
}

/// What kind of thing a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    /// An agent conversation with a transcript behind it. Usually resumable.
    AgentConversation,
    /// A live terminal believed to be running an agent, with no transcript
    /// found — typically a multiplexer pane whose agent has not written one
    /// yet, or whose store this scan could not read.
    AgentTerminal,
    /// A terminal with no agent in it. Reported because a dock listing a
    /// machine's sessions wants the plain shells too; filter on `agent` to
    /// drop them.
    Terminal,
}

/// What kind of thing hosts a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OwnerKind {
    /// A terminal multiplexer: tmux, screen, zellij, Herdr.
    Multiplexer,
    /// A workspace/worktree orchestrator: Orca, Superset, super.engineering.
    Orchestrator,
    /// Nothing hosts it that we can see — the agent was run directly, or its
    /// host is not something this crate knows about.
    Unhosted,
}

/// The application that probably owns a session, and how sure we are.
///
/// "Probably" is the operative word and is why [`Self::confidence`] is not
/// optional. A multiplexer that recorded the agent's own session id owns it
/// beyond doubt; one that merely has a pane in the same directory running the
/// same agent is a good guess that a second agent in that directory would
/// break. A dock can show the first as a definite parent and the second as a
/// hint, but only if the difference survives into the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub tool: ToolId,
    pub kind: OwnerKind,
    /// Display name of the owning tool, e.g. `"tmux"`.
    pub display_name: String,
    /// The owning multiplexer session, when there is one (`"work"`, `"$0"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The owning pane, when there is one (`"%0"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// The owning workspace or worktree, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub confidence: Confidence,
    /// Why we believe this owns the session. `None` for a row that *is* the
    /// owner's own record rather than one matched to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<LinkReason>,
}

/// Everywhere on disk this session is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPaths {
    /// The directory the session is working in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// The agent's transcript, for a caller that wants to read or delete it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<PathBuf>,
    /// The owning workspace's checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
    /// The repository the workspace came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    /// The owning multiplexer's socket, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<PathBuf>,
}

/// One session, with everything this crate could find out about it.
///
/// Self-contained on purpose: a caller renders a row, filters a list or opens
/// a session from this value alone, without holding the [`Discovery`] it came
/// from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectedSession {
    pub key: SessionKey,
    pub kind: SessionKind,
    /// The coding agent, where one was identified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// The agent's own session id, as the agent would accept it for `--resume`.
    ///
    /// Distinct from `key.id`, which is unique but may be a path-derived
    /// stand-in. Present only when we read an id the agent actually minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_id: Option<String>,
    /// For a sub-agent, the conversation that spawned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_subagent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    /// The process id, when the owner reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The command line, when the owner reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Whether a client is attached to the owning multiplexer session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<bool>,
    pub owner: Owner,
    pub paths: SessionPaths,
    /// Last known activity as a unix timestamp — the transcript's mtime, or
    /// the owning session's creation time. The field a dock sorts on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_unix: Option<u64>,
    /// How much of this row to believe. The weakest part of it governs.
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

impl CollectedSession {
    /// A one-line label for a list row.
    pub fn display_title(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        match (&self.paths.cwd, self.agent) {
            (Some(cwd), Some(agent)) => format!(
                "{} in {}",
                agent.as_str(),
                cwd.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| cwd.display().to_string())
            ),
            (Some(cwd), None) => cwd.display().to_string(),
            (None, Some(agent)) => agent.as_str().to_string(),
            (None, None) => self.key.id.clone(),
        }
    }

    /// Whether the agent's own id was read, which is what `--resume` needs.
    pub fn is_resumable(&self) -> bool {
        self.resume_id.is_some() && !self.is_subagent
    }
}

/// Flatten a scan into one row per session.
///
/// Pure: the same [`Discovery`] always yields the same rows, in the same
/// order (most recently active first, then by key), so a dock can diff two
/// scans without sorting them itself.
pub fn collect(discovery: &Discovery) -> Vec<CollectedSession> {
    let machine = MachineId(discovery.machine.clone());
    let owners = OwnerIndex::build(discovery);
    let mut rows = Vec::new();

    for tool in &discovery.tools {
        for session in &tool.agent_sessions {
            let key = SessionKey {
                machine: machine.clone(),
                tool: tool.tool,
                id: session.id.clone(),
            };
            let owner = owners.for_session(&key_without_machine(&key), discovery, tool);
            let workspace = owners.workspace_for(&owner, discovery);
            let pane = owners.pane_for(&owner, discovery);

            rows.push(CollectedSession {
                kind: SessionKind::AgentConversation,
                agent: Some(session.agent),
                resume_id: (session.confidence != Confidence::Possible).then(|| session.id.clone()),
                parent: session.parent_session_id.clone(),
                is_subagent: session.is_subagent,
                title: session.title.clone(),
                git_branch: session.git_branch.clone(),
                agent_version: session.agent_version.clone(),
                pid: pane.as_ref().and_then(|pane| pane.pid),
                command: pane.as_ref().and_then(|pane| pane.command.clone()),
                attached: owners.attached_for(&owner, discovery),
                paths: SessionPaths {
                    cwd: session.cwd.clone(),
                    transcript: session.transcript.clone(),
                    workspace_root: workspace.as_ref().and_then(|w| w.path.clone()),
                    repo: workspace.as_ref().and_then(|w| w.repo.clone()),
                    socket: owners.socket_for(&owner, discovery),
                },
                last_active_unix: session.modified_unix,
                confidence: session.confidence,
                evidence: session.evidence.clone(),
                owner,
                key,
            });
        }
    }

    // Panes that were never matched to a transcript. These are real sessions —
    // an agent that has not written a transcript yet, or one whose store this
    // scan could not read — and dropping them would make a dock claim a machine
    // is idle while an agent is visibly running on it.
    let claimed = owners.claimed_panes();
    for tool in &discovery.tools {
        for mux in &tool.mux_sessions {
            for pane in &mux.panes {
                let pane_key = format!("{}/{}:{}", tool.tool.as_str(), mux.id, pane.id);
                if claimed.contains(&pane_key) {
                    continue;
                }
                rows.push(CollectedSession {
                    key: SessionKey {
                        machine: machine.clone(),
                        tool: tool.tool,
                        id: format!("{}:{}", mux.id, pane.id),
                    },
                    kind: if pane.agent.is_some() {
                        SessionKind::AgentTerminal
                    } else {
                        SessionKind::Terminal
                    },
                    agent: pane.agent,
                    // A pane's recorded agent session id is the agent's own;
                    // without one there is nothing to resume by.
                    resume_id: pane.agent_session_id.clone(),
                    parent: None,
                    is_subagent: false,
                    title: pane.title.clone(),
                    git_branch: None,
                    agent_version: None,
                    pid: pane.pid,
                    command: pane.command.clone(),
                    attached: mux.attached,
                    owner: Owner {
                        tool: tool.tool,
                        kind: owner_kind(tool.tool),
                        display_name: tool.display_name.clone(),
                        session: mux.name.clone().or_else(|| Some(mux.id.clone())),
                        pane: Some(pane.id.clone()),
                        workspace: None,
                        confidence: mux.confidence,
                        // This row *is* the multiplexer's own record, not a
                        // guess matching two records together.
                        reason: None,
                    },
                    paths: SessionPaths {
                        cwd: pane.cwd.clone(),
                        socket: mux.socket.clone(),
                        ..SessionPaths::default()
                    },
                    last_active_unix: mux.created_unix,
                    // A pane's agent is read off its foreground command, which
                    // a wrapper script or a shell of the same name defeats.
                    confidence: if pane.agent_session_id.is_some() {
                        Confidence::Certain
                    } else {
                        Confidence::Possible
                    },
                    evidence: mux.evidence.clone(),
                });
            }
        }
    }

    // Newest first, then by key so equal timestamps do not reorder between
    // scans and make a dock flicker.
    rows.sort_by(|a, b| {
        b.last_active_unix
            .cmp(&a.last_active_unix)
            .then_with(|| a.key.cmp(&b.key))
    });
    rows
}

fn key_without_machine(key: &SessionKey) -> String {
    format!("{}/{}", key.tool.as_str(), key.id)
}

fn owner_kind(tool: ToolId) -> OwnerKind {
    match tool {
        ToolId::Tmux | ToolId::Screen | ToolId::Zellij | ToolId::Herdr => OwnerKind::Multiplexer,
        ToolId::Orca | ToolId::Superset | ToolId::SuperEngineering => OwnerKind::Orchestrator,
        _ => OwnerKind::Unhosted,
    }
}

/// The links, indexed for lookup by hosted session.
struct OwnerIndex {
    /// hosted key -> the strongest link claiming it.
    best: BTreeMap<String, crate::model::Link>,
    /// Host keys that were matched to a transcript.
    claimed: BTreeSet<String>,
}

impl OwnerIndex {
    fn build(discovery: &Discovery) -> Self {
        let mut best: BTreeMap<String, crate::model::Link> = BTreeMap::new();
        let mut claimed = BTreeSet::new();
        for link in &discovery.links {
            claimed.insert(link.host.clone());
            match best.get(&link.hosted) {
                // Lower `Confidence` sorts stronger (Certain is first).
                Some(existing) if existing.confidence <= link.confidence => {}
                _ => {
                    best.insert(link.hosted.clone(), link.clone());
                }
            }
        }
        Self { best, claimed }
    }

    fn claimed_panes(&self) -> &BTreeSet<String> {
        &self.claimed
    }

    fn for_session(&self, hosted: &str, discovery: &Discovery, own: &ToolReport) -> Owner {
        let Some(link) = self.best.get(hosted) else {
            // Nothing hosts it that we can see. The agent's own tool is still
            // the most useful thing to name, so the row is never ownerless.
            return Owner {
                tool: own.tool,
                kind: OwnerKind::Unhosted,
                display_name: own.display_name.clone(),
                session: None,
                pane: None,
                workspace: None,
                confidence: Confidence::Certain,
                reason: None,
            };
        };

        let (tool, rest) = split_host(&link.host);
        let display_name = discovery
            .tools
            .iter()
            .find(|report| report.tool.as_str() == tool)
            .map(|report| report.display_name.clone())
            .unwrap_or_else(|| tool.to_string());
        let tool_id = discovery
            .tools
            .iter()
            .find(|report| report.tool.as_str() == tool)
            .map(|report| report.tool)
            .unwrap_or(own.tool);

        // `<session>:<pane>` for a multiplexer, a bare id for a workspace.
        let (session, pane, workspace) = match rest.split_once(':') {
            Some((session, pane)) => (Some(session.to_string()), Some(pane.to_string()), None),
            None => (None, None, Some(rest.to_string())),
        };

        Owner {
            tool: tool_id,
            kind: owner_kind(tool_id),
            display_name,
            session,
            pane,
            workspace,
            confidence: link.confidence,
            reason: Some(link.reason),
        }
    }

    fn workspace_for<'a>(&self, owner: &Owner, discovery: &'a Discovery) -> Option<&'a Workspace> {
        let wanted = owner.workspace.as_deref()?;
        discovery
            .tools
            .iter()
            .filter(|report| report.tool == owner.tool)
            .flat_map(|report| &report.workspaces)
            .find(|workspace| workspace.id == wanted)
    }

    fn pane_for<'a>(
        &self,
        owner: &Owner,
        discovery: &'a Discovery,
    ) -> Option<&'a crate::model::Pane> {
        let (session, pane) = (owner.session.as_deref()?, owner.pane.as_deref()?);
        discovery
            .tools
            .iter()
            .filter(|report| report.tool == owner.tool)
            .flat_map(|report| &report.mux_sessions)
            .filter(|mux| mux.id == session || mux.name.as_deref() == Some(session))
            .flat_map(|mux| &mux.panes)
            .find(|candidate| candidate.id == pane)
    }

    fn mux_for<'a>(
        &self,
        owner: &Owner,
        discovery: &'a Discovery,
    ) -> Option<&'a crate::model::MuxSession> {
        let session = owner.session.as_deref()?;
        discovery
            .tools
            .iter()
            .filter(|report| report.tool == owner.tool)
            .flat_map(|report| &report.mux_sessions)
            .find(|mux| mux.id == session || mux.name.as_deref() == Some(session))
    }

    fn socket_for(&self, owner: &Owner, discovery: &Discovery) -> Option<PathBuf> {
        self.mux_for(owner, discovery)
            .and_then(|mux| mux.socket.clone())
    }

    fn attached_for(&self, owner: &Owner, discovery: &Discovery) -> Option<bool> {
        self.mux_for(owner, discovery).and_then(|mux| mux.attached)
    }
}

/// `"tool/rest"` — the tool id is everything before the first slash.
fn split_host(host: &str) -> (&str, &str) {
    host.split_once('/').unwrap_or((host, ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Machine;
    use crate::scan;
    use crate::target::MapTarget;

    /// A machine with a Claude session, a Herdr pane naming it, and a second
    /// tmux-shaped pane with no transcript behind it.
    fn machine(name: &str) -> Machine<'static> {
        let host = MapTarget::new(name, "/home/remote")
            .with_file(
                "/home/remote/.claude/projects/-srv-api/sess-1.jsonl",
                r#"{"sessionId":"sess-1","cwd":"/srv/api","gitBranch":"deploy","version":"2.1.273"}"#,
            )
            .with_file(
                "/home/remote/.config/herdr/session.json",
                r#"{"version":3,"workspaces":[{"id":"ws","identity_cwd":"/srv/api",
                    "tabs":[{"panes":{
                      "0":{"cwd":"/srv/api","managed_agent_kind":"claude",
                           "agent_session":{"source":"claude","agent":"claude","kind":"id","value":"sess-1"}},
                      "1":{"cwd":"/srv/other","managed_agent_kind":"codex"}
                    }}]}]}"#,
            );
        Machine::over(host)
    }

    #[test]
    fn a_conversation_and_the_pane_running_it_are_one_row() {
        let rows = collect(&scan::scan(&machine("build-box")));

        let claude: Vec<_> = rows
            .iter()
            .filter(|row| row.agent == Some(AgentId::Claude))
            .collect();
        assert_eq!(
            claude.len(),
            1,
            "the transcript and its pane are one session, not two: {claude:#?}"
        );

        let row = claude[0];
        // The transcript wins as the row, because it carries the resumable id,
        // the working directory and the history.
        assert_eq!(row.kind, SessionKind::AgentConversation);
        assert_eq!(row.resume_id.as_deref(), Some("sess-1"));
        assert_eq!(row.git_branch.as_deref(), Some("deploy"));
        // …and the pane becomes its owner.
        assert_eq!(row.owner.tool, ToolId::Herdr);
        assert_eq!(row.owner.kind, OwnerKind::Multiplexer);
        // Session-qualified, because two Herdr installs can both call a
        // workspace "ws" — see `providers::herdr`.
        assert_eq!(row.owner.pane.as_deref(), Some("default/ws/0"));
        assert_eq!(row.owner.session.as_deref(), Some("default"));
        assert_eq!(row.owner.confidence, Confidence::Certain);
        assert_eq!(row.owner.reason, Some(LinkReason::RecordedSessionId));
    }

    #[test]
    fn a_pane_with_no_transcript_still_gets_a_row() {
        // Otherwise a dock reports a machine as idle while an agent is
        // visibly running on it.
        let rows = collect(&scan::scan(&machine("build-box")));

        let codex: Vec<_> = rows
            .iter()
            .filter(|row| row.agent == Some(AgentId::Codex))
            .collect();
        assert_eq!(codex.len(), 1, "{rows:#?}");
        assert_eq!(codex[0].kind, SessionKind::AgentTerminal);
        assert_eq!(codex[0].resume_id, None, "nothing to resume by");
        assert_eq!(
            codex[0].paths.cwd.as_deref(),
            Some(std::path::Path::new("/srv/other"))
        );
    }

    #[test]
    fn rows_from_two_machines_concatenate_without_colliding() {
        // The same session id on two machines is ordinary: a repo cloned
        // twice, or a home directory on shared storage.
        let mut rows = collect(&scan::scan(&machine("build-box")));
        rows.extend(collect(&scan::scan(&machine("laptop"))));

        let keys: BTreeSet<String> = rows.iter().map(|row| row.key.to_string()).collect();
        assert_eq!(keys.len(), rows.len(), "every key must stay unique");

        let sess_1: Vec<_> = rows
            .iter()
            .filter(|row| row.resume_id.as_deref() == Some("sess-1"))
            .collect();
        assert_eq!(
            sess_1.len(),
            2,
            "the same id on two machines is two sessions"
        );
        assert_eq!(sess_1[0].key.machine.as_str(), "build-box");
        assert_eq!(sess_1[1].key.machine.as_str(), "laptop");
    }

    #[test]
    fn a_session_nothing_hosts_is_still_attributed_to_its_own_tool() {
        let host = MapTarget::new("m", "/home/remote").with_file(
            "/home/remote/.claude/projects/-x/lonely.jsonl",
            r#"{"sessionId":"lonely","cwd":"/x"}"#,
        );
        let env = Machine::over(host);

        let rows = collect(&scan::scan(&env));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].owner.kind, OwnerKind::Unhosted);
        assert_eq!(rows[0].owner.tool, ToolId::ClaudeCode);
        assert_eq!(rows[0].owner.display_name, "Claude Code");
    }

    #[test]
    fn rows_are_newest_first_and_stable_between_scans() {
        let env = machine("m");
        let first = collect(&scan::scan(&env));
        let second = collect(&scan::scan(&env));
        let keys = |rows: &[CollectedSession]| {
            rows.iter()
                .map(|row| row.key.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(&first),
            keys(&second),
            "an unchanged machine must not reorder, or a dock flickers"
        );

        let times: Vec<Option<u64>> = first.iter().map(|row| row.last_active_unix).collect();
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted, "newest first");
    }

    #[test]
    fn a_row_answers_on_its_own_without_the_discovery() {
        let rows = collect(&scan::scan(&machine("build-box")));
        let row = rows
            .iter()
            .find(|row| row.agent == Some(AgentId::Claude))
            .expect("claude row");

        // Everything a dock needs, from the row alone.
        assert_eq!(row.key.machine.as_str(), "build-box");
        assert!(row.is_resumable());
        assert!(
            row.display_title().contains("api"),
            "{}",
            row.display_title()
        );
        assert!(row.paths.transcript.is_some());
        assert!(
            row.paths.workspace_root.is_none(),
            "herdr pane, not a worktree"
        );
        assert_eq!(row.owner.display_name, "Herdr");
    }

    #[test]
    fn a_worktree_owner_carries_its_checkout_and_repo() {
        let host = MapTarget::new("m", "/home/remote")
            .with_file(
                "/home/remote/.orca/state.json",
                r#"{"schemaVersion":1,"repos":[{"id":"r","path":"/src/repo"}],
                    "worktreeMeta":{"wt":{"path":"/src/repo/.worktrees/feat","branch":"feat","repoId":"r"}}}"#,
            )
            .with_file(
                "/home/remote/.claude/projects/-w/s.jsonl",
                r#"{"sessionId":"s","cwd":"/src/repo/.worktrees/feat"}"#,
            );
        let env = Machine::over(host);

        let rows = collect(&scan::scan(&env));
        let row = &rows[0];

        assert_eq!(row.owner.kind, OwnerKind::Orchestrator);
        assert_eq!(row.owner.tool, ToolId::Orca);
        assert_eq!(row.owner.workspace.as_deref(), Some("wt"));
        assert_eq!(
            row.paths.workspace_root.as_deref(),
            Some(std::path::Path::new("/src/repo/.worktrees/feat"))
        );
        assert_eq!(
            row.paths.repo.as_deref(),
            Some(std::path::Path::new("/src/repo"))
        );
    }
}
