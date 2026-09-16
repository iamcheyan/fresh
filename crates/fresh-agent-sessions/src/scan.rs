//! Running every provider and assembling the result.

use crate::catalog::{scan_store, AGENT_STORES};
use crate::env::Env;
use crate::model::{Discovery, Installed, ToolId, ToolReport};
use crate::providers;

/// What to scan, and how hard.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Cap on sessions reported per tool.
    ///
    /// Present because these stores are unbounded: a year of daily Claude Code
    /// use is tens of thousands of transcripts, and a caller filling a picker
    /// wants the newest twenty, not all of them. Providers that can walk
    /// newest-first (Codex's date shards) honour it without reading the rest.
    pub max_sessions_per_tool: usize,
    /// Report tools that left nothing behind.
    ///
    /// Off by default: a list of thirty "not installed" rows buries the three
    /// that matter. On, it answers "did you even look for X?".
    pub include_absent: bool,
    /// Build the cross-tool links. On by default — it is the only thing that
    /// tells a caller which rows are the same session seen twice.
    pub correlate: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_sessions_per_tool: 200,
            include_absent: false,
            correlate: true,
        }
    }
}

/// Scan with the default options.
pub fn scan(env: &Env) -> Discovery {
    scan_with(env, &ScanOptions::default())
}

/// Scan every tool this crate knows about.
///
/// Never fails. A provider that cannot read something records it in that
/// tool's `problems` and the scan continues — see [`crate::providers`].
pub fn scan_with(env: &Env, options: &ScanOptions) -> Discovery {
    let limit = options.max_sessions_per_tool;
    let mut tools = vec![
        providers::tmux::scan(env),
        providers::multiplexers::scan_screen(env),
        providers::multiplexers::scan_zellij(env),
        providers::herdr::scan(env),
        providers::orchestrators::scan_orca(env),
        providers::orchestrators::scan_superset(env),
        providers::orchestrators::scan_super_engineering(env),
        providers::claude_code::scan(env, limit),
        providers::claude_desktop::scan(env),
        providers::codex::scan(env, limit),
        providers::codex::desktop_report(env),
    ];

    // Everything else is table-driven; see `crate::catalog`.
    for store in AGENT_STORES {
        let mut report = ToolReport::absent(ToolId::Agent(store.agent), store.display_name);
        let scan = scan_store(store, env, limit, &mut report.problems);
        // A store directory that exists, or the agent's binary on PATH. An
        // environment variable on its own proves neither: `GOOSE_DATA_DIR`
        // exported by a shell profile says where goose *would* keep its
        // sessions, not that goose is here.
        let executable = env.which(store.agent.command_names()[0]);
        if scan.root_exists || executable.is_some() {
            report.installed = Installed::Present {
                version: None,
                executable,
            };
        }
        report.evidence = scan.evidence;
        report.agent_sessions = scan.sessions;
        tools.push(report);
    }

    if !options.include_absent {
        tools.retain(|report| !report.is_empty());
    }
    tools.sort_by_key(|report| report.tool.as_str());

    let mut discovery = Discovery {
        scanned_at_unix: env.now_unix(),
        tools,
        links: Vec::new(),
    };
    if options.correlate {
        discovery.correlate();
    }
    discovery
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentId, LinkReason};

    #[test]
    fn a_bare_machine_yields_an_empty_but_valid_discovery() {
        let home = tempfile::tempdir().expect("tempdir");
        let discovery = scan(&Env::fake(home.path()));
        assert_eq!(discovery.agent_session_count(), 0);
        assert!(discovery.links.is_empty());
    }

    #[test]
    fn include_absent_lists_every_tool_we_looked_for() {
        let home = tempfile::tempdir().expect("tempdir");
        let options = ScanOptions {
            include_absent: true,
            ..ScanOptions::default()
        };
        let discovery = scan_with(&Env::fake(home.path()), &options);
        assert!(
            discovery.tools.len() > 20,
            "expected the full tool list, got {}",
            discovery.tools.len()
        );
    }

    /// Herdr records a Claude session id on a pane; Claude Code has the
    /// transcript for that same id. The two must come back as one exact link.
    #[test]
    fn a_herdr_pane_links_exactly_to_the_claude_transcript_it_names() {
        let home = tempfile::tempdir().expect("tempdir");
        let session_id = "11111111-2222-3333-4444-555555555555";

        let bucket = home.path().join(".claude").join("projects").join("-w");
        std::fs::create_dir_all(&bucket).expect("create bucket");
        std::fs::write(
            bucket.join(format!("{session_id}.jsonl")),
            format!(r#"{{"sessionId":"{session_id}","cwd":"/w"}}"#),
        )
        .expect("write transcript");

        let config = home.path().join(".config").join("herdr");
        std::fs::create_dir_all(&config).expect("create config");
        std::fs::write(
            config.join("session.json"),
            format!(
                r#"{{"version":3,"workspaces":[{{"id":"ws","identity_cwd":"/w","tabs":[{{"panes":{{
                   "0":{{"cwd":"/w","managed_agent_kind":"claude",
                        "agent_session":{{"source":"claude","agent":"claude","kind":"id","value":"{session_id}"}}}}}}}}]}}]}}"#
            ),
        )
        .expect("write snapshot");

        let discovery = scan(&Env::fake(home.path()));

        let exact: Vec<_> = discovery
            .links
            .iter()
            .filter(|link| link.reason == LinkReason::RecordedSessionId)
            .collect();
        assert_eq!(exact.len(), 1, "links: {:?}", discovery.links);
        assert!(exact[0].hosted.ends_with(session_id), "{:?}", exact[0]);
        assert!(exact[0].host.starts_with("herdr/"), "{:?}", exact[0]);
    }

    #[test]
    fn a_catalog_agent_is_discovered_from_its_transcript_store_alone() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join(".factory").join("sessions");
        std::fs::create_dir_all(&dir).expect("create");
        std::fs::write(dir.join("abc123.jsonl"), r#"{"cwd":"/srv/app"}"#).expect("write");

        let discovery = scan(&Env::fake(home.path()));

        let droid = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::Agent(AgentId::Droid))
            .expect("droid report");
        assert_eq!(droid.agent_sessions.len(), 1);
        assert_eq!(droid.agent_sessions[0].id, "abc123");
        assert_eq!(
            droid.agent_sessions[0].cwd.as_deref(),
            Some(std::path::Path::new("/srv/app"))
        );
    }

    #[test]
    fn a_session_is_attributed_to_the_innermost_workspace_containing_it() {
        // Orca knows about a repo and a worktree inside it. A session in the
        // worktree belongs to the worktree, not to both.
        let home = tempfile::tempdir().expect("tempdir");
        let data = home.path().join(".orca");
        std::fs::create_dir_all(&data).expect("create");
        std::fs::write(
            data.join("state.json"),
            r#"{"schemaVersion":1,"repos":[{"id":"r","path":"/src/repo"}],
                "worktreeMeta":{
                  "outer":{"path":"/src/repo","repoId":"r"},
                  "inner":{"path":"/src/repo/.worktrees/feat","repoId":"r"}}}"#,
        )
        .expect("write state");

        let bucket = home.path().join(".claude").join("projects").join("-x");
        std::fs::create_dir_all(&bucket).expect("create bucket");
        std::fs::write(
            bucket.join("sess.jsonl"),
            r#"{"sessionId":"sess","cwd":"/src/repo/.worktrees/feat"}"#,
        )
        .expect("write transcript");

        let discovery = scan(&Env::fake(home.path()));

        let workspace_links: Vec<_> = discovery
            .links
            .iter()
            .filter(|link| link.reason == LinkReason::CwdInsideWorkspace)
            .collect();
        assert_eq!(workspace_links.len(), 1, "links: {workspace_links:?}");
        assert!(
            workspace_links[0].host.ends_with("/inner"),
            "expected the deeper worktree, got {:?}",
            workspace_links[0]
        );
    }

    #[test]
    fn the_session_limit_is_honoured_per_tool() {
        let home = tempfile::tempdir().expect("tempdir");
        let bucket = home.path().join(".claude").join("projects").join("-w");
        std::fs::create_dir_all(&bucket).expect("create bucket");
        for i in 0..10 {
            std::fs::write(
                bucket.join(format!("s{i}.jsonl")),
                format!(r#"{{"sessionId":"s{i}","cwd":"/w"}}"#),
            )
            .expect("write");
        }

        let options = ScanOptions {
            max_sessions_per_tool: 3,
            ..ScanOptions::default()
        };
        let discovery = scan_with(&Env::fake(home.path()), &options);

        let claude = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::ClaudeCode)
            .expect("claude report");
        assert_eq!(claude.agent_sessions.len(), 3);
        assert!(
            claude.problems.iter().any(|p| p.contains("limit")),
            "truncation must be reported: {:?}",
            claude.problems
        );
    }

    #[test]
    fn an_env_var_alone_does_not_make_an_agent_installed() {
        // A shell profile that exports GOOSE_DATA_DIR says where goose would
        // keep sessions, not that goose is here.
        let home = tempfile::tempdir().expect("tempdir");
        let options = ScanOptions {
            include_absent: true,
            ..ScanOptions::default()
        };
        let env = Env::fake(home.path()).with_var("GOOSE_DATA_DIR", "/definitely/not/here");

        let discovery = scan_with(&env, &options);

        let goose = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::Agent(AgentId::Goose))
            .expect("goose report");
        assert!(
            matches!(goose.installed, crate::model::Installed::Absent),
            "got {:?}",
            goose.installed
        );
        assert!(
            goose.evidence.iter().any(|e| e.locator == "GOOSE_DATA_DIR"),
            "the override is still worth reporting to whoever is debugging"
        );
    }

    #[test]
    fn a_workspace_id_shared_across_tools_does_not_steal_the_attribution() {
        // Orca has the repo root and its worktree; Herdr independently has a
        // workspace whose id happens to match. Depths keyed by bare id let
        // Herdr's shallow path stand in for Orca's deep one, and the session
        // gets attributed to the outer repo.
        let home = tempfile::tempdir().expect("tempdir");
        let data = home.path().join(".orca");
        std::fs::create_dir_all(&data).expect("create");
        std::fs::write(
            data.join("state.json"),
            r#"{"schemaVersion":1,"repos":[{"id":"r","path":"/src/repo"}],
                "worktreeMeta":{
                  "outer":{"path":"/src/repo","repoId":"r"},
                  "inner":{"path":"/src/repo/.worktrees/feat","repoId":"r"}}}"#,
        )
        .expect("write state");

        // A Herdr workspace whose id collides with Orca's deep one, at a
        // shallow path.
        let config = home.path().join(".config").join("herdr");
        std::fs::create_dir_all(&config).expect("create config");
        std::fs::write(
            config.join("session.json"),
            r#"{"version":3,"workspaces":[{"id":"inner","identity_cwd":"/elsewhere",
                "tabs":[{"panes":{}}]}]}"#,
        )
        .expect("write snapshot");

        let bucket = home.path().join(".claude").join("projects").join("-x");
        std::fs::create_dir_all(&bucket).expect("create bucket");
        std::fs::write(
            bucket.join("sess.jsonl"),
            r#"{"sessionId":"sess","cwd":"/src/repo/.worktrees/feat"}"#,
        )
        .expect("write transcript");

        let discovery = scan(&Env::fake(home.path()));

        let workspace_links: Vec<_> = discovery
            .links
            .iter()
            .filter(|link| link.reason == LinkReason::CwdInsideWorkspace)
            .collect();
        assert_eq!(workspace_links.len(), 1, "links: {workspace_links:?}");
        assert_eq!(
            workspace_links[0].host, "orca/inner",
            "the deepest *Orca* workspace owns it, not whatever shares the name"
        );
    }

    #[test]
    fn an_unparsed_store_is_reported_as_present_without_inventing_sessions() {
        // opencode keeps state in SQLite. Emitting a row per .db file would
        // claim sessions we cannot identify.
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join(".local").join("share").join("opencode");
        std::fs::create_dir_all(&dir).expect("create");
        std::fs::write(dir.join("storage.db"), b"SQLite format 3\0").expect("write");

        let discovery = scan(&Env::fake(home.path()));

        let opencode = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::Agent(AgentId::OpenCode))
            .expect("opencode report");
        assert!(matches!(
            opencode.installed,
            crate::model::Installed::Present { .. }
        ));
        assert!(
            opencode.agent_sessions.is_empty(),
            "the format is not parsed, so no session may be claimed"
        );
        assert!(
            opencode
                .evidence
                .iter()
                .any(|e| e.locator.ends_with("storage.db")),
            "but the file itself is still reported"
        );
    }

    #[test]
    fn a_truncated_catalog_walk_says_so() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join(".factory").join("sessions");
        std::fs::create_dir_all(&dir).expect("create");
        for i in 0..5 {
            std::fs::write(dir.join(format!("s{i}.jsonl")), r#"{"cwd":"/w"}"#).expect("write");
        }

        let options = ScanOptions {
            max_sessions_per_tool: 2,
            ..ScanOptions::default()
        };
        let discovery = scan_with(&Env::fake(home.path()), &options);

        let droid = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::Agent(AgentId::Droid))
            .expect("droid report");
        assert_eq!(droid.agent_sessions.len(), 2);
        assert!(
            droid.problems.iter().any(|p| p.contains("limit")),
            "silent truncation is how a picker quietly loses sessions: {:?}",
            droid.problems
        );
    }

    /// The whole point of the [`crate::host`] port: a scan of a machine this
    /// process has no filesystem access to.
    ///
    /// Nothing in this test exists on any disk. A provider that still reached
    /// for `std::fs` would come back empty, so this is what keeps the port
    /// honest as providers are added.
    #[test]
    fn a_scan_runs_against_a_host_with_no_filesystem_behind_it() {
        use crate::host::testing::MapHost;
        use std::sync::Arc;

        let host = MapHost::default()
            // Claude Code on the remote machine.
            .with_file(
                "/home/remote/.claude/projects/-srv-api/remote-sess.jsonl",
                r#"{"sessionId":"remote-sess","cwd":"/srv/api","gitBranch":"deploy","version":"2.1.273"}"#,
            )
            // Codex, in its date-sharded layout.
            .with_file(
                "/home/remote/.codex/sessions/2026/05/01/rollout-2026-05-01T10-00-00-0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b.jsonl",
                r#"{"cwd":"/srv/api"}"#,
            )
            // And a Herdr workspace whose pane names the Claude session, so
            // correlation has to work over the port too.
            .with_file(
                "/home/remote/.config/herdr/session.json",
                r#"{"version":3,"workspaces":[{"id":"ws","identity_cwd":"/srv/api",
                    "tabs":[{"panes":{"0":{"cwd":"/srv/api","managed_agent_kind":"claude",
                    "agent_session":{"source":"claude","agent":"claude","kind":"id",
                    "value":"remote-sess"}}}}]}]}"#,
            );

        let env = Env::fake("/unused-local-home").with_host(Arc::new(host));
        assert!(env.is_remote());
        assert_eq!(env.host_label(), "ssh://test-host");

        let discovery = scan(&env);

        let claude = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::ClaudeCode)
            .expect("claude report from the remote host");
        assert_eq!(claude.agent_sessions.len(), 1);
        assert_eq!(
            claude.agent_sessions[0].cwd.as_deref(),
            Some(std::path::Path::new("/srv/api"))
        );
        assert_eq!(
            claude.agent_sessions[0].git_branch.as_deref(),
            Some("deploy")
        );

        let codex = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::CodexCli)
            .expect("codex report from the remote host");
        assert_eq!(codex.agent_sessions.len(), 1);

        let exact: Vec<_> = discovery
            .links
            .iter()
            .filter(|link| link.reason == LinkReason::RecordedSessionId)
            .collect();
        assert_eq!(
            exact.len(),
            1,
            "correlation works over the port too: {:?}",
            discovery.links
        );
    }

    #[test]
    fn a_remote_host_does_not_leak_this_processs_environment() {
        use crate::host::testing::MapHost;
        use std::sync::Arc;

        // The local env says CODEX_HOME is somewhere; the remote host knows
        // nothing about it. Honouring the local value would send the scan to a
        // path that does not exist there and label the result as if it did.
        let host = MapHost::default().with_dir("/home/remote/.codex");
        let env = Env::fake("/local")
            .with_var("CODEX_HOME", "/local/elsewhere")
            .with_host(Arc::new(host));

        assert_eq!(env.var("CODEX_HOME"), None);
        assert_eq!(
            crate::providers::codex::codex_home(&env),
            std::path::Path::new("/home/remote/.codex"),
            "the remote home is what a remote store resolves against"
        );
    }

    #[test]
    fn a_host_that_cannot_run_commands_still_reports_what_files_prove() {
        use crate::host::testing::MapHost;
        use std::sync::Arc;

        // MapHost declines `run`. tmux discovery must degrade to "a socket is
        // here" rather than failing the scan.
        let host = MapHost::default().with_file("/tmp/tmux-1000/default", "");
        let env = Env::fake("/local")
            .with_host(Arc::new(host))
            .with_commands(crate::env::StubRunner::new());

        let discovery = scan(&env);

        let tmux = discovery
            .tools
            .iter()
            .find(|report| report.tool == ToolId::Tmux)
            .expect("tmux report");
        assert_eq!(tmux.mux_sessions.len(), 1);
        assert_eq!(
            tmux.mux_sessions[0].confidence,
            crate::model::Confidence::Possible,
            "a socket without a live query is a weak claim, and says so"
        );
    }

    #[test]
    fn the_whole_discovery_round_trips_through_json() {
        let home = tempfile::tempdir().expect("tempdir");
        let bucket = home.path().join(".claude").join("projects").join("-w");
        std::fs::create_dir_all(&bucket).expect("create bucket");
        std::fs::write(bucket.join("s.jsonl"), r#"{"sessionId":"s","cwd":"/w"}"#).expect("write");

        let discovery = scan(&Env::fake(home.path()));
        let json = serde_json::to_string(&discovery).expect("serialize");
        let back: Discovery = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(discovery, back);
    }
}
