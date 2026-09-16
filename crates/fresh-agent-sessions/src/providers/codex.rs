//! The Codex CLI, and the Codex desktop app that shares its store.
//!
//! # Layout
//!
//! ```text
//! $CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl
//! $CODEX_HOME/session_index.jsonl      # id -> title index
//! $CODEX_HOME/state_5.sqlite           # desktop-app metadata
//! $CODEX_HOME/history.jsonl            # cross-session prompt history
//! ```
//!
//! `CODEX_HOME` defaults to `~/.codex`, confirmed against Codex CLI 0.154.0
//! installed on this machine.
//!
//! # Why the date shards matter
//!
//! Sessions are bucketed by date, so the newest ones are reachable without
//! walking the whole tree: sort the `YYYY`, `MM` and `DD` levels descending and
//! stop once the limit is met. A flat recursive walk of a year of daily use
//! would stat thousands of files to return twenty rows, which is the
//! difference between a picker that opens instantly and one that hitches.
//!
//! # The desktop app is not a separate store
//!
//! Codex Desktop reads the same `CODEX_HOME` the CLI writes — it adds
//! `state_5.sqlite` beside it for its own metadata, but the transcripts are
//! shared. So [`scan`] reports the sessions once, under
//! [`ToolId::CodexCli`], and [`desktop_report`] reports the app's presence
//! without duplicating them. Reporting the same conversation twice under two
//! tool names is exactly the kind of double-count this crate exists to avoid.

use std::path::{Path, PathBuf};

use crate::machine::Machine;
use crate::model::{AgentId, AgentSession, Confidence, Evidence, Installed, ToolId, ToolReport};

/// `$CODEX_HOME`, or `~/.codex`.
pub fn codex_home(env: &Machine<'_>) -> PathBuf {
    env.dir_override(&["CODEX_HOME"], env.home_join([".codex"]))
        .0
}

pub fn scan(env: &Machine<'_>, limit: usize) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::CodexCli, "Codex CLI");
    let home = codex_home(env);

    // Only when the variable actually moved the store. `dir_override` ignores a
    // relative value, and reporting the *resolved default* under the variable's
    // name would claim provenance the variable never had — the one thing a
    // crate built on evidence must not do.
    match env.var("CODEX_HOME") {
        Some(value) if crate::target::is_absolute_on(env.platform(), &value) => {
            report
                .evidence
                .push(Evidence::env_var("CODEX_HOME", value.to_string()));
        }
        Some(value) => {
            report.note_problem(format!(
                "CODEX_HOME is set to a relative path ({value}); ignoring it and reading {}",
                home.display()
            ));
        }
        None => {}
    }
    if let Some(path) = env.which("codex") {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: env
                .run("codex", &["--version"])
                .ok()
                .map(|out| out.trim().to_string()),
            executable: Some(path),
        };
    }
    if !env.is_dir(&home) {
        return report;
    }
    report
        .evidence
        .push(Evidence::dir(&home).saying("CODEX_HOME"));
    if matches!(report.installed, Installed::Absent) {
        report.installed = Installed::Present {
            version: None,
            executable: None,
        };
    }

    // The index maps session id to title; without it sessions are still
    // discoverable, just untitled.
    let index_path = home.join("session_index.jsonl");
    let titles = if env.is_file(&index_path) {
        report
            .evidence
            .push(Evidence::file(&index_path).saying("session id to title index"));
        read_index_titles(env, &index_path)
    } else {
        Vec::new()
    };

    let state_db = home.join("state_5.sqlite");
    if env.is_file(&state_db) {
        report.evidence.push(
            Evidence::file(&state_db)
                .saying("Codex desktop metadata (SQLite; present but not parsed)"),
        );
    }

    let sessions_dir = home.join("sessions");
    if !env.is_dir(&sessions_dir) {
        return report;
    }
    report
        .evidence
        .push(Evidence::dir(&sessions_dir).saying("rollout transcripts, sharded YYYY/MM/DD"));

    let mut files = Vec::new();
    collect_newest_rollouts(env, &sessions_dir, 0, limit, &mut files, &mut report);
    for path in files {
        report
            .agent_sessions
            .push(session_from_rollout(env, &path, &titles));
    }
    report
}

/// The Codex desktop app's own report: presence only, sessions shared.
pub fn desktop_report(env: &Machine<'_>) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::CodexDesktop, "Codex Desktop");
    let home = codex_home(env);
    let state_db = home.join("state_5.sqlite");

    let bundle = match env.platform() {
        crate::target::Platform::MacOs => Some(PathBuf::from("/Applications/Codex.app")),
        _ => None,
    };

    if let Some(bundle) = bundle.filter(|path| env.exists(path)) {
        report
            .evidence
            .push(Evidence::dir(&bundle).saying("Codex desktop application bundle"));
        report.installed = Installed::Present {
            version: None,
            executable: Some(bundle),
        };
    } else if env.is_file(&state_db) {
        // The state database is the desktop app's own file; the CLI does not
        // write it. Its presence is the strongest signal available off macOS.
        report.evidence.push(
            Evidence::file(&state_db)
                .saying("written by Codex Desktop, not the CLI; app has run on this machine"),
        );
        report.installed = Installed::Present {
            version: None,
            executable: None,
        };
    } else if env.platform() != crate::target::Platform::MacOs {
        report.installed = Installed::Unsupported {
            reason: "Codex Desktop ships for macOS; nothing to inspect from this platform"
                .to_string(),
        };
        return report;
    }

    if !matches!(report.installed, Installed::Absent) {
        report.evidence.push(Evidence::dir(&home).saying(
            "shares CODEX_HOME with the CLI; transcripts are reported under codex-cli, not here",
        ));
    }
    report
}

/// Walk the `YYYY/MM/DD` shards newest-first, stopping at `limit` files.
fn collect_newest_rollouts(
    env: &Machine<'_>,
    dir: &Path,
    depth: usize,
    limit: usize,
    out: &mut Vec<PathBuf>,
    report: &mut ToolReport,
) {
    // Three levels of date directories, then the files.
    const DATE_DEPTH: usize = 3;

    if out.len() >= limit {
        return;
    }
    let mut children = match env.list_io(dir) {
        Ok(children) => children,
        Err(err) => {
            report.note_problem(format!("{}: {err}", dir.display()));
            return;
        }
    };
    // Descending: `2026` before `2025`, `12` before `01`. Zero-padded date
    // components make lexicographic order the same as chronological order.
    children.reverse();

    for entry in children {
        if out.len() >= limit {
            return;
        }
        if depth < DATE_DEPTH {
            if entry.kind == crate::target::EntryKind::Dir {
                collect_newest_rollouts(env, &entry.path, depth + 1, limit, out, report);
            }
            continue;
        }
        if is_rollout(&entry.path) {
            out.push(entry.path);
        }
    }
}

fn is_rollout(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

/// The session id embedded in `rollout-<timestamp>-<uuid>.jsonl`.
///
/// The timestamp itself contains dashes (`2026-05-01T10-00-00`), so the id is
/// the *last five* dash-separated groups — a UUID — not "everything after the
/// second dash". Splitting on the wrong dash yields a plausible-looking string
/// that resumes nothing.
pub fn session_id_from_rollout(file_name: &str) -> Option<String> {
    let stem = file_name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        return None;
    }
    let uuid = parts[parts.len() - 5..].join("-");
    is_uuid_shaped(&uuid).then_some(uuid)
}

fn is_uuid_shaped(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, group)| group.len() == *len)
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
}

fn session_from_rollout(
    env: &Machine<'_>,
    path: &Path,
    titles: &[(String, String)],
) -> AgentSession {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let parsed = session_id_from_rollout(file_name);
    let id = parsed
        .clone()
        .unwrap_or_else(|| file_name.trim_end_matches(".jsonl").to_string());
    let title = titles
        .iter()
        .find(|(indexed, _)| *indexed == id)
        .map(|(_, title)| title.clone());

    AgentSession {
        id,
        agent: AgentId::Codex,
        cwd: crate::catalog::read_cwd_field(env, path, &["cwd", "workdir", "working_directory"]),
        git_branch: None,
        title,
        transcript: Some(path.to_path_buf()),
        modified_unix: env.modified_unix(path),
        agent_version: None,
        is_subagent: false,
        parent_session_id: None,
        confidence: if parsed.is_some() {
            Confidence::Probable
        } else {
            Confidence::Possible
        },
        evidence: vec![Evidence::file(path).saying("Codex rollout transcript")],
    }
}

/// `(session id, title)` pairs from `session_index.jsonl`.
fn read_index_titles(env: &Machine<'_>, path: &Path) -> Vec<(String, String)> {
    const MAX_RECORDS: usize = 20_000;
    /// The index is one short record per session; 4MiB is thousands of them
    /// and keeps the remote case from pulling an unbounded file.
    const MAX_BYTES: usize = 4 * 1024 * 1024;

    let Ok(bytes) = env.read_prefix_io(path, MAX_BYTES) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(bytes.as_slice()).into_owned();
    text.lines()
        .take(MAX_RECORDS)
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            let id = value
                .get("id")
                .or_else(|| value.get("session_id"))
                .or_else(|| value.get("sessionId"))?
                .as_str()?
                .to_string();
            let title = value
                .get("title")
                .or_else(|| value.get("summary"))?
                .as_str()
                .filter(|t| !t.is_empty())?
                .to_string();
            Some((id, title))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalTarget;

    const UUID: &str = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

    #[test]
    fn parses_the_session_id_out_of_a_rollout_name() {
        let name = format!("rollout-2026-05-01T10-00-00-{UUID}.jsonl");
        assert_eq!(session_id_from_rollout(&name).as_deref(), Some(UUID));
    }

    #[test]
    fn the_timestamps_own_dashes_do_not_confuse_the_split() {
        // "everything after rollout-<date>-" would give "10-00-00-0199a1b2-…".
        let name = format!("rollout-2026-05-01T10-00-00-{UUID}.jsonl");
        let id = session_id_from_rollout(&name).expect("parsed");
        assert!(
            !id.contains("10-00-00"),
            "timestamp leaked into the id: {id}"
        );
    }

    #[test]
    fn a_name_without_a_uuid_is_rejected_rather_than_half_parsed() {
        assert_eq!(
            session_id_from_rollout("rollout-2026-05-01-notauuid.jsonl"),
            None
        );
        assert_eq!(session_id_from_rollout("something-else.jsonl"), None);
    }

    fn write_rollout(home: &Path, date: [&str; 3], name: &str, body: &str) {
        let dir = home
            .join(".codex")
            .join("sessions")
            .join(date[0])
            .join(date[1])
            .join(date[2]);
        std::fs::create_dir_all(&dir).expect("create shard");
        std::fs::write(dir.join(name), body).expect("write rollout");
    }

    #[test]
    fn the_cwd_is_read_out_of_a_session_meta_envelope() {
        // Codex wraps the first record rather than writing the fields flat:
        // `{"timestamp":…,"type":"session_meta","payload":{"cwd":…}}`. Every
        // fixture here used to be flat, so reading only top-level keys passed
        // the tests and lost the cwd on every real transcript.
        let home = tempfile::tempdir().expect("tempdir");
        write_rollout(
            home.path(),
            ["2026", "05", "09"],
            &format!("rollout-2026-05-09T00-00-00-{UUID}.jsonl"),
            r#"{"timestamp":"2026-05-09T00:00:00Z","type":"session_meta","payload":{"id":"abc","cwd":"/srv/api","originator":"codex_cli_rs"}}"#,
        );

        let report = scan(&Machine::local(home.path()), 10);

        assert_eq!(report.agent_sessions.len(), 1);
        assert_eq!(
            report.agent_sessions[0].cwd.as_deref(),
            Some(Path::new("/srv/api")),
            "the cwd lives under `payload`, not at the top level"
        );
    }

    #[test]
    fn walks_date_shards_newest_first() {
        let home = tempfile::tempdir().expect("tempdir");
        for (year, month, day) in [
            ("2025", "01", "02"),
            ("2026", "05", "01"),
            ("2026", "05", "09"),
        ] {
            write_rollout(
                home.path(),
                [year, month, day],
                &format!("rollout-{year}-{month}-{day}T00-00-00-{UUID}.jsonl"),
                r#"{"cwd":"/repo"}"#,
            );
        }

        let report = scan(&Machine::local(home.path()), 2);

        assert_eq!(report.agent_sessions.len(), 2, "honours the limit");
        let paths: Vec<String> = report
            .agent_sessions
            .iter()
            .map(|s| {
                // Forward slashes so the shard assertions below read the same
                // on Windows, where `display()` renders separators as `\`.
                s.transcript
                    .as_ref()
                    .expect("transcript")
                    .display()
                    .to_string()
                    .replace('\\', "/")
            })
            .collect();
        assert!(
            paths[0].contains("2026/05/09"),
            "newest first, got {paths:?}"
        );
        assert!(
            paths[1].contains("2026/05/01"),
            "then next-newest: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("2025")),
            "the oldest shard should not be reached at all: {paths:?}"
        );
    }

    #[test]
    fn titles_come_from_the_session_index() {
        let home = tempfile::tempdir().expect("tempdir");
        write_rollout(
            home.path(),
            ["2026", "05", "01"],
            &format!("rollout-2026-05-01T10-00-00-{UUID}.jsonl"),
            r#"{"cwd":"/repo"}"#,
        );
        std::fs::write(
            home.path().join(".codex").join("session_index.jsonl"),
            format!("{{\"id\":\"{UUID}\",\"title\":\"Fix the parser\"}}\n"),
        )
        .expect("write index");

        let report = scan(&Machine::local(home.path()), 10);

        assert_eq!(
            report.agent_sessions[0].title.as_deref(),
            Some("Fix the parser")
        );
        assert_eq!(
            report.agent_sessions[0].cwd.as_deref(),
            Some(Path::new("/repo"))
        );
    }

    #[test]
    fn codex_home_relocates_the_whole_store() {
        let root = tempfile::tempdir().expect("tempdir");
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(
            elsewhere
                .join("sessions")
                .join("2026")
                .join("05")
                .join("01"),
        )
        .expect("create");
        std::fs::write(
            elsewhere
                .join("sessions")
                .join("2026")
                .join("05")
                .join("01")
                .join(format!("rollout-2026-05-01T10-00-00-{UUID}.jsonl")),
            r#"{"cwd":"/elsewhere"}"#,
        )
        .expect("write");

        let env = Machine::over(
            LocalTarget::at(root.path()).with_var("CODEX_HOME", elsewhere.to_string_lossy()),
        );
        let report = scan(&env, 10);

        assert_eq!(report.agent_sessions.len(), 1);
        assert!(report.evidence.iter().any(|e| e.locator == "CODEX_HOME"));
    }

    #[test]
    fn a_relative_codex_home_is_ignored_not_resolved_against_the_cwd() {
        let home = tempfile::tempdir().expect("tempdir");
        let env =
            Machine::over(LocalTarget::at(home.path()).with_var("CODEX_HOME", "relative/path"));
        assert_eq!(codex_home(&env), home.path().join(".codex"));
    }

    #[test]
    fn a_rejected_codex_home_is_not_reported_as_if_it_had_applied() {
        // Claiming the resolved default under the variable's name is a
        // provenance lie, in a crate whose entire premise is provenance.
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".codex")).expect("create");
        let env =
            Machine::over(LocalTarget::at(home.path()).with_var("CODEX_HOME", "relative/path"));

        let report = scan(&env, 10);

        assert!(
            !report.evidence.iter().any(|e| e.locator == "CODEX_HOME"),
            "the variable did not move anything, so it is not evidence: {:?}",
            report.evidence
        );
        assert!(
            report.problems.iter().any(|p| p.contains("relative")),
            "but the user should still be told it was ignored: {:?}",
            report.problems
        );
    }

    #[test]
    fn the_desktop_app_does_not_duplicate_the_clis_sessions() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".codex")).expect("create");
        std::fs::write(home.path().join(".codex").join("state_5.sqlite"), b"SQLite")
            .expect("write");

        let desktop = desktop_report(&Machine::local(home.path()));

        assert!(matches!(desktop.installed, Installed::Present { .. }));
        assert!(
            desktop.agent_sessions.is_empty(),
            "transcripts belong to the codex-cli report"
        );
    }
}
