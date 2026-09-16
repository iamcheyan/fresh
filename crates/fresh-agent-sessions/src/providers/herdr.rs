//! Herdr — a terminal multiplexer that knows what an agent is.
//!
//! # Layout, read off the source
//!
//! Herdr resolves its config directory as `$XDG_CONFIG_HOME/herdr` when that
//! variable is set, else `$HOME/.config/herdr`. A debug build uses `herdr-dev`
//! instead, so both are checked. Inside:
//!
//! ```text
//! <config>/session.json            # the default session's snapshot
//! <config>/herdr.sock              # JSON API socket
//! <config>/herdr-client.sock       # client protocol socket
//! <config>/sessions/<name>/…       # one directory per named session
//! ```
//!
//! The default session's data *is* the config directory — not
//! `sessions/default` — which is the one thing about this layout that a
//! reasonable guess gets wrong.
//!
//! # Why Herdr is the best source in this crate
//!
//! Every other multiplexer leaves you inferring which pane holds an agent from
//! its command name. Herdr writes it down. Each pane in `session.json` may
//! carry `agent_name`, `managed_agent_kind`, and an `agent_session` object:
//!
//! ```json
//! {"source": "claude", "agent": "claude", "kind": "id",
//!  "value": "3cc1b4b4-ea2d-5dac-9a45-aa08481b6b8f"}
//! ```
//!
//! `kind` is `id` or `path` — the agent's own session id, or a path to its
//! transcript. Either one links a pane to a row from
//! [`crate::providers::claude_code`] *exactly*, with no cwd heuristics, which
//! is what [`crate::model::Discovery::correlate`] reports as
//! [`crate::model::LinkReason::RecordedSessionId`].
//!
//! # Schema drift
//!
//! `session.json` carries a `version` field and Herdr has already migrated its
//! workspace shape once (a legacy snapshot put panes directly on the workspace;
//! the current one nests them under `tabs`). This reader handles both and
//! treats every field as optional, because a snapshot written by a newer Herdr
//! than we were built against is the normal case, not the exception.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::env::Env;
use crate::model::{
    AgentId, Confidence, Evidence, Installed, MuxSession, Pane, ToolId, ToolReport, Workspace,
};

/// The config directories to check, release build first.
fn config_dirs(env: &Env) -> Vec<PathBuf> {
    let base = env
        .var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| env.home_join([".config"]));
    vec![base.join("herdr"), base.join("herdr-dev")]
}

pub fn scan(env: &Env) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::Herdr, "Herdr");

    if let Some(path) = env.which("herdr") {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: env
                .run("herdr", &["--version"])
                .ok()
                .map(|out| out.trim().to_string()),
            executable: Some(path),
        };
    }

    for config in config_dirs(env) {
        if !config.is_dir() {
            continue;
        }
        report
            .evidence
            .push(Evidence::dir(&config).saying("Herdr config directory"));
        if matches!(report.installed, Installed::Absent) {
            report.installed = Installed::Present {
                version: None,
                executable: None,
            };
        }

        // The default session lives in the config directory itself.
        read_session_dir(&config, "default", &mut report);

        let named = config.join("sessions");
        if let Ok(entries) = std::fs::read_dir(&named) {
            let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            dirs.sort();
            for dir in dirs.into_iter().filter(|path| path.is_dir()) {
                let name = dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unnamed")
                    .to_string();
                read_session_dir(&dir, &name, &mut report);
            }
        }
    }

    report
}

/// Read one session directory: its snapshot and its sockets.
fn read_session_dir(dir: &Path, name: &str, report: &mut ToolReport) {
    let api_socket = dir.join("herdr.sock");
    let has_socket = api_socket.exists();
    if has_socket {
        report.evidence.push(
            Evidence::socket(&api_socket)
                .saying("Herdr JSON API socket; a live server can be queried here"),
        );
    }

    let snapshot_path = dir.join("session.json");
    if !snapshot_path.is_file() {
        return;
    }
    let text = match std::fs::read_to_string(&snapshot_path) {
        Ok(text) => text,
        Err(err) => {
            report.note_problem(format!("{}: {err}", snapshot_path.display()));
            return;
        }
    };
    let snapshot: SessionSnapshot = match serde_json::from_str(&text) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            report.note_problem(format!(
                "{}: not a Herdr session snapshot: {err}",
                snapshot_path.display()
            ));
            return;
        }
    };

    let evidence = Evidence::file(&snapshot_path).saying(format!(
        "Herdr session snapshot v{} for session {name}",
        snapshot.version
    ));
    report.evidence.push(evidence.clone());

    let mut panes = Vec::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        let id = workspace
            .id
            .clone()
            .unwrap_or_else(|| format!("{name}#{index}"));
        report.workspaces.push(Workspace {
            id: id.clone(),
            name: workspace.custom_name.clone(),
            path: workspace.identity_cwd.clone(),
            repo: None,
            branch: None,
            // Herdr's `worktree_space` marks a workspace as part of a worktree
            // group; absent means it was not created as one.
            is_worktree: Some(workspace.worktree_space.is_some()),
            confidence: Confidence::Certain,
            evidence: vec![evidence.clone()],
        });
        panes.extend(workspace.collect_panes(&id));
    }

    report.mux_sessions.push(MuxSession {
        id: name.to_string(),
        name: Some(name.to_string()),
        created_unix: None,
        // A snapshot is a file, not a live server; whether a client is
        // attached is not something it can tell us.
        attached: None,
        socket: has_socket.then_some(api_socket),
        panes,
        confidence: Confidence::Certain,
        evidence: vec![evidence],
    });
}

/// The subset of Herdr's `SessionSnapshot` this crate reads.
///
/// Every field is optional or defaulted: a snapshot from a newer Herdr will
/// carry keys we do not know, and must still parse.
#[derive(Debug, Deserialize)]
struct SessionSnapshot {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    workspaces: Vec<WorkspaceSnapshot>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceSnapshot {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    custom_name: Option<String>,
    #[serde(default)]
    identity_cwd: Option<PathBuf>,
    #[serde(default)]
    worktree_space: Option<serde_json::Value>,
    /// Current shape: panes hang off tabs.
    #[serde(default)]
    tabs: Vec<TabSnapshot>,
    /// Legacy shape: panes directly on the workspace. Still on disk for anyone
    /// who has not restarted Herdr since the migration.
    #[serde(default)]
    panes: std::collections::HashMap<String, PaneSnapshot>,
}

impl WorkspaceSnapshot {
    fn collect_panes(&self, workspace_id: &str) -> Vec<Pane> {
        let from_tabs = self.tabs.iter().enumerate().flat_map(|(tab_index, tab)| {
            let label = tab
                .custom_name
                .clone()
                .unwrap_or_else(|| format!("tab {tab_index}"));
            sorted_panes(&tab.panes)
                .into_iter()
                .map(move |(id, pane)| pane.to_model(workspace_id, &id, Some(label.clone())))
        });
        let legacy = sorted_panes(&self.panes)
            .into_iter()
            .map(|(id, pane)| pane.to_model(workspace_id, &id, None));
        from_tabs.chain(legacy).collect()
    }
}

/// Panes keyed by a numeric id in a map — sorted numerically, so pane 10 comes
/// after pane 9 rather than after pane 1.
fn sorted_panes(
    panes: &std::collections::HashMap<String, PaneSnapshot>,
) -> Vec<(String, &PaneSnapshot)> {
    let mut entries: Vec<(String, &PaneSnapshot)> =
        panes.iter().map(|(id, pane)| (id.clone(), pane)).collect();
    entries.sort_by_key(|(id, _)| (id.parse::<u64>().ok(), id.clone()));
    entries
}

#[derive(Debug, Deserialize)]
struct TabSnapshot {
    #[serde(default)]
    custom_name: Option<String>,
    #[serde(default)]
    panes: std::collections::HashMap<String, PaneSnapshot>,
}

#[derive(Debug, Deserialize)]
struct PaneSnapshot {
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    managed_agent_kind: Option<String>,
    #[serde(default)]
    agent_session: Option<PaneAgentSession>,
    #[serde(default)]
    launch_argv: Option<Vec<String>>,
}

impl PaneSnapshot {
    fn to_model(&self, workspace_id: &str, pane_id: &str, window: Option<String>) -> Pane {
        // Three signals for which agent this is, best first: the kind Herdr
        // manages it as, the agent named on the recorded session, and finally
        // argv[0] — the same guess tmux forces on us.
        let agent = self
            .managed_agent_kind
            .as_deref()
            .and_then(agent_from_herdr_kind)
            .or_else(|| {
                self.agent_session
                    .as_ref()
                    .and_then(|session| agent_from_herdr_kind(&session.agent))
            })
            .or_else(|| {
                self.launch_argv
                    .as_ref()
                    .and_then(|argv| argv.first())
                    .and_then(|program| AgentId::from_command(program))
            });

        Pane {
            id: format!("{workspace_id}/{pane_id}"),
            window,
            pid: None,
            cwd: self.cwd.clone(),
            command: self
                .launch_argv
                .as_ref()
                .map(|argv| argv.join(" "))
                .or_else(|| self.agent_name.clone()),
            title: self.label.clone(),
            agent,
            // Only an `id` reference names the agent's own session. A `path`
            // reference points at a transcript file, which is a different kind
            // of fact and would not match a session id if reported as one.
            agent_session_id: self
                .agent_session
                .as_ref()
                .filter(|session| session.kind == "id")
                .map(|session| session.value.clone()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PaneAgentSession {
    #[serde(default)]
    agent: String,
    /// `"id"` or `"path"`.
    #[serde(default)]
    kind: String,
    #[serde(default)]
    value: String,
}

/// Map Herdr's agent spelling onto ours.
fn agent_from_herdr_kind(kind: &str) -> Option<AgentId> {
    AgentId::ALL
        .iter()
        .copied()
        .find(|agent| agent.as_str() == kind)
        .or_else(|| AgentId::from_command(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot in the current shape, with the fields Herdr's own
    /// `PaneSnapshot`/`PaneAgentSessionSnapshot` structs define.
    const CURRENT: &str = r#"{
      "version": 3,
      "workspaces": [{
        "id": "ws-1",
        "custom_name": "fresh",
        "identity_cwd": "/home/user/fresh",
        "worktree_space": {"group": "g1"},
        "tabs": [{
          "custom_name": "agents",
          "panes": {
            "2": {"cwd": "/home/user/fresh", "label": "claude",
                  "managed_agent_kind": "claude",
                  "agent_session": {"source": "claude", "agent": "claude",
                                    "kind": "id", "value": "sess-abc"}},
            "10": {"cwd": "/home/user/fresh", "launch_argv": ["codex"]}
          }
        }]
      }]
    }"#;

    fn install(home: &Path, body: &str) -> PathBuf {
        let config = home.join(".config").join("herdr");
        std::fs::create_dir_all(&config).expect("create config");
        std::fs::write(config.join("session.json"), body).expect("write snapshot");
        config
    }

    #[test]
    fn reads_the_recorded_agent_session_id() {
        let home = tempfile::tempdir().expect("tempdir");
        install(home.path(), CURRENT);

        let report = scan(&Env::fake(home.path()));

        let panes = &report.mux_sessions[0].panes;
        assert_eq!(panes[0].agent, Some(AgentId::Claude));
        assert_eq!(
            panes[0].agent_session_id.as_deref(),
            Some("sess-abc"),
            "this is what makes an exact cross-tool link possible"
        );
    }

    #[test]
    fn a_path_reference_is_not_reported_as_a_session_id() {
        let body = CURRENT.replace(
            r#""kind": "id", "value": "sess-abc""#,
            r#""kind": "path", "value": "/t/x.jsonl""#,
        );
        let home = tempfile::tempdir().expect("tempdir");
        install(home.path(), &body);

        let report = scan(&Env::fake(home.path()));

        assert_eq!(
            report.mux_sessions[0].panes[0].agent_session_id, None,
            "a transcript path would never match a session id"
        );
    }

    #[test]
    fn panes_are_ordered_numerically_not_lexicographically() {
        let home = tempfile::tempdir().expect("tempdir");
        install(home.path(), CURRENT);

        let report = scan(&Env::fake(home.path()));

        let ids: Vec<&str> = report.mux_sessions[0]
            .panes
            .iter()
            .map(|pane| pane.id.as_str())
            .collect();
        assert_eq!(ids, vec!["ws-1/2", "ws-1/10"], "pane 10 sorts after pane 2");
    }

    #[test]
    fn the_default_session_lives_in_the_config_dir_not_under_sessions() {
        let home = tempfile::tempdir().expect("tempdir");
        install(home.path(), CURRENT);

        let report = scan(&Env::fake(home.path()));

        assert_eq!(report.mux_sessions.len(), 1);
        assert_eq!(report.mux_sessions[0].name.as_deref(), Some("default"));
    }

    #[test]
    fn named_sessions_are_found_under_sessions() {
        let home = tempfile::tempdir().expect("tempdir");
        let config = install(home.path(), CURRENT);
        let named = config.join("sessions").join("review");
        std::fs::create_dir_all(&named).expect("create named session");
        std::fs::write(named.join("session.json"), CURRENT).expect("write");

        let report = scan(&Env::fake(home.path()));

        let names: Vec<&str> = report
            .mux_sessions
            .iter()
            .filter_map(|session| session.name.as_deref())
            .collect();
        assert!(names.contains(&"default"), "got {names:?}");
        assert!(names.contains(&"review"), "got {names:?}");
    }

    #[test]
    fn the_legacy_workspace_shape_still_parses() {
        let legacy = r#"{
          "version": 1,
          "workspaces": [{
            "custom_name": "old",
            "identity_cwd": "/srv/old",
            "panes": {"0": {"cwd": "/srv/old", "launch_argv": ["claude"]}}
          }]
        }"#;
        let home = tempfile::tempdir().expect("tempdir");
        install(home.path(), legacy);

        let report = scan(&Env::fake(home.path()));

        assert_eq!(report.mux_sessions[0].panes.len(), 1);
        assert_eq!(report.mux_sessions[0].panes[0].agent, Some(AgentId::Claude));
    }

    #[test]
    fn unknown_fields_from_a_newer_herdr_do_not_break_the_read() {
        let future = r#"{
          "version": 99,
          "brand_new_top_level": {"anything": true},
          "workspaces": [{"id": "ws", "identity_cwd": "/x",
                          "tabs": [{"panes": {"0": {"cwd": "/x", "future_field": 1}}}]}]
        }"#;
        let home = tempfile::tempdir().expect("tempdir");
        install(home.path(), future);

        let report = scan(&Env::fake(home.path()));

        assert_eq!(report.mux_sessions.len(), 1);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
    }

    #[test]
    fn a_corrupt_snapshot_is_a_problem_not_a_panic() {
        let home = tempfile::tempdir().expect("tempdir");
        install(home.path(), "{ this is not json");

        let report = scan(&Env::fake(home.path()));

        assert!(report.mux_sessions.is_empty());
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    }

    #[test]
    fn xdg_config_home_relocates_the_store() {
        let root = tempfile::tempdir().expect("tempdir");
        let xdg = root.path().join("xdg");
        std::fs::create_dir_all(xdg.join("herdr")).expect("create");
        std::fs::write(xdg.join("herdr").join("session.json"), CURRENT).expect("write");

        let env = Env::fake(root.path()).with_var("XDG_CONFIG_HOME", xdg.to_string_lossy());
        let report = scan(&env);

        assert_eq!(report.mux_sessions.len(), 1);
    }
}
