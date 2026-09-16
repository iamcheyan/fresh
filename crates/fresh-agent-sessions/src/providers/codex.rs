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

use crate::env::{modified_unix, Env};
use crate::model::{AgentId, AgentSession, Confidence, Evidence, Installed, ToolId, ToolReport};

/// `$CODEX_HOME`, or `~/.codex`.
pub fn codex_home(env: &Env) -> PathBuf {
    env.dir_override(&["CODEX_HOME"], env.home_join([".codex"]))
}

pub fn scan(env: &Env, limit: usize) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::CodexCli, "Codex CLI");
    let home = codex_home(env);

    if env.var("CODEX_HOME").is_some() {
        report.evidence.push(Evidence::env_var(
            "CODEX_HOME",
            home.to_string_lossy().into_owned(),
        ));
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
    if !home.is_dir() {
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
    let titles = if index_path.is_file() {
        report
            .evidence
            .push(Evidence::file(&index_path).saying("session id to title index"));
        read_index_titles(&index_path)
    } else {
        Vec::new()
    };

    let state_db = home.join("state_5.sqlite");
    if state_db.is_file() {
        report.evidence.push(
            Evidence::file(&state_db)
                .saying("Codex desktop metadata (SQLite; present but not parsed)"),
        );
    }

    let sessions_dir = home.join("sessions");
    if !sessions_dir.is_dir() {
        return report;
    }
    report
        .evidence
        .push(Evidence::dir(&sessions_dir).saying("rollout transcripts, sharded YYYY/MM/DD"));

    let mut files = Vec::new();
    collect_newest_rollouts(&sessions_dir, 0, limit, &mut files, &mut report);
    for path in files {
        report
            .agent_sessions
            .push(session_from_rollout(&path, &titles));
    }
    report
}

/// The Codex desktop app's own report: presence only, sessions shared.
pub fn desktop_report(env: &Env) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::CodexDesktop, "Codex Desktop");
    let home = codex_home(env);
    let state_db = home.join("state_5.sqlite");

    let bundle = match env.platform() {
        crate::env::Platform::MacOs => Some(PathBuf::from("/Applications/Codex.app")),
        _ => None,
    };

    if let Some(bundle) = bundle.filter(|path| path.exists()) {
        report
            .evidence
            .push(Evidence::dir(&bundle).saying("Codex desktop application bundle"));
        report.installed = Installed::Present {
            version: None,
            executable: Some(bundle),
        };
    } else if state_db.is_file() {
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
    } else if env.platform() != crate::env::Platform::MacOs {
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
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            report.note_problem(format!("{}: {err}", dir.display()));
            return;
        }
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    // Descending: `2026` before `2025`, `12` before `01`. Zero-padded date
    // components make lexicographic order the same as chronological order.
    children.sort_by(|a, b| b.cmp(a));

    for path in children {
        if out.len() >= limit {
            return;
        }
        if depth < DATE_DEPTH {
            if path.is_dir() {
                collect_newest_rollouts(&path, depth + 1, limit, out, report);
            }
            continue;
        }
        if is_rollout(&path) {
            out.push(path);
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

fn session_from_rollout(path: &Path, titles: &[(String, String)]) -> AgentSession {
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
        cwd: crate::catalog::read_cwd_field(path, &["cwd", "workdir", "working_directory"]),
        git_branch: None,
        title,
        transcript: Some(path.to_path_buf()),
        modified_unix: modified_unix(path),
        agent_version: None,
        is_subagent: false,
        confidence: if parsed.is_some() {
            Confidence::Probable
        } else {
            Confidence::Possible
        },
        evidence: vec![Evidence::file(path).saying("Codex rollout transcript")],
    }
}

/// `(session id, title)` pairs from `session_index.jsonl`.
fn read_index_titles(path: &Path) -> Vec<(String, String)> {
    use std::io::{BufRead, BufReader};

    const MAX_RECORDS: usize = 20_000;

    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .take(MAX_RECORDS)
        .filter_map(Result::ok)
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

        let report = scan(&Env::fake(home.path()), 2);

        assert_eq!(report.agent_sessions.len(), 2, "honours the limit");
        let paths: Vec<String> = report
            .agent_sessions
            .iter()
            .map(|s| {
                s.transcript
                    .as_ref()
                    .expect("transcript")
                    .display()
                    .to_string()
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

        let report = scan(&Env::fake(home.path()), 10);

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

        let env = Env::fake(root.path()).with_var("CODEX_HOME", elsewhere.to_string_lossy());
        let report = scan(&env, 10);

        assert_eq!(report.agent_sessions.len(), 1);
        assert!(report.evidence.iter().any(|e| e.locator == "CODEX_HOME"));
    }

    #[test]
    fn a_relative_codex_home_is_ignored_not_resolved_against_the_cwd() {
        let home = tempfile::tempdir().expect("tempdir");
        let env = Env::fake(home.path()).with_var("CODEX_HOME", "relative/path");
        assert_eq!(codex_home(&env), home.path().join(".codex"));
    }

    #[test]
    fn the_desktop_app_does_not_duplicate_the_clis_sessions() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".codex")).expect("create");
        std::fs::write(home.path().join(".codex").join("state_5.sqlite"), b"SQLite")
            .expect("write");

        let desktop = desktop_report(&Env::fake(home.path()));

        assert!(matches!(desktop.installed, Installed::Present { .. }));
        assert!(
            desktop.agent_sessions.is_empty(),
            "transcripts belong to the codex-cli report"
        );
    }
}
