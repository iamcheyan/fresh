//! Claude Code (the CLI).
//!
//! # Layout, confirmed against a live install
//!
//! ```text
//! ~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl
//! ~/.claude/projects/<encoded-cwd>/<session-uuid>/          # sub-agent transcripts
//! ~/.claude.json                                            # global settings
//! ```
//!
//! # The project directory name is lossy, and that matters
//!
//! The directory under `projects/` is the working directory with **every
//! non-alphanumeric character replaced by a single dash**. Runs are *not*
//! collapsed, so `/home/user/fresh` becomes `-home-user-fresh` (verified on
//! this machine) and a path containing `/.claude` becomes `--claude`.
//!
//! That transform is not injective: `~/a-b` and `~/a/b` encode identically.
//! Decoding it is therefore guesswork, and this provider does not do it when
//! it has a better option — which it almost always does:
//!
//! **Every transcript record carries the real `cwd`.** A line from a live
//! session on this machine:
//!
//! ```json
//! {"type":"user","sessionId":"3cc1b4b4-…","cwd":"/home/user/fresh",
//!  "gitBranch":"claude/serene-heisenberg-u4cdl7","version":"2.1.273", …}
//! ```
//!
//! So the working directory, the git branch and the agent version are read out
//! of the transcript's first usable record ([`Confidence::Certain`]), and the
//! encoded directory name is used only as a fallback label
//! ([`Confidence::Possible`]) when a transcript cannot be read.
//!
//! # Sub-agent transcripts
//!
//! A session id also names a *directory* beside the `.jsonl`, holding the Task
//! sub-agents' transcripts. They share the parent's session id and are not
//! independently resumable, so they are reported with `is_subagent` set rather
//! than as sessions in their own right — a caller building a resume picker
//! should filter them out, and one building a tree needs them.

use std::path::{Path, PathBuf};

use crate::machine::Machine;
use crate::model::{AgentId, AgentSession, Confidence, Evidence, Installed, ToolId, ToolReport};

/// Encode a working directory the way Claude Code names its project bucket.
///
/// Each character outside `[A-Za-z0-9]` becomes its own dash, so a run of two
/// separators yields two dashes rather than one. Exposed because a caller that
/// already knows the directory it cares about can address the bucket directly
/// instead of scanning all of them.
pub fn encode_project_dir(path: &Path) -> String {
    let text = path.to_string_lossy();
    let normalised = text.replace('\\', "/");
    let trimmed = if normalised == "/" || is_windows_root(&normalised) {
        normalised.as_str()
    } else {
        normalised.trim_end_matches('/')
    };
    trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn is_windows_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// The header facts a transcript's first records carry.
#[derive(Debug, Default, PartialEq, Eq)]
struct Header {
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    git_branch: Option<String>,
    version: Option<String>,
}

impl Header {
    fn is_complete(&self) -> bool {
        self.session_id.is_some() && self.cwd.is_some()
    }
}

pub fn scan(env: &Machine<'_>, limit: usize) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::ClaudeCode, "Claude Code");

    let (home, _moved) = env.dir_override(&["CLAUDE_CONFIG_DIR"], env.home_join([".claude"]));
    let settings = env.home_join([".claude.json"]);
    if env.is_file(&settings) {
        report.evidence.push(
            Evidence::file(&settings)
                .saying("Claude Code global settings; presence proves install"),
        );
        report.installed = Installed::Present {
            version: None,
            executable: env.which("claude"),
        };
    }
    if let Some(path) = env.which("claude") {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: None,
            executable: Some(path),
        };
    }

    let projects = home.join("projects");
    if !env.is_dir(&projects) {
        return report;
    }
    report
        .evidence
        .push(Evidence::dir(&projects).saying("one directory per working directory, dash-encoded"));
    if matches!(report.installed, Installed::Absent) {
        report.installed = Installed::Present {
            version: None,
            executable: None,
        };
    }

    let buckets = match read_sorted_dir(env, &projects) {
        Ok(buckets) => buckets,
        Err(err) => {
            report.note_problem(format!("{}: {err}", projects.display()));
            return report;
        }
    };

    let mut truncated = false;
    for bucket in buckets {
        if !env.is_dir(&bucket) {
            continue;
        }
        if scan_bucket(env, &bucket, limit, &mut report) {
            truncated = true;
            break;
        }
    }
    if truncated {
        report.note_problem(format!(
            "stopped at the {limit}-conversation limit; the sessions reported are the first \
             {limit} in directory order, then sorted by modification time — not necessarily \
             the {limit} most recent on disk"
        ));
    }

    report.agent_sessions.sort_by(|a, b| {
        b.modified_unix
            .cmp(&a.modified_unix)
            .then_with(|| a.id.cmp(&b.id))
    });
    report
}

/// Returns true if the limit was reached and transcripts were left unread.
///
/// The limit counts *resumable conversations*, not rows: sub-agent transcripts
/// ride along with the parent that owns them. Counting rows would let one
/// heavily-delegating session exhaust the whole budget by itself and return a
/// page with a single resumable entry on it.
fn scan_bucket(env: &Machine<'_>, bucket: &Path, limit: usize, report: &mut ToolReport) -> bool {
    let entries = match read_sorted_dir(env, bucket) {
        Ok(entries) => entries,
        Err(err) => {
            report.note_problem(format!("{}: {err}", bucket.display()));
            return false;
        }
    };

    for path in entries {
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if top_level_count(report) >= limit {
            return true;
        }
        let session = read_session(env, &path, bucket, false);
        // The sibling directory named after the session holds its sub-agents.
        let subagent_dir = path.with_extension("");
        if env.is_dir(&subagent_dir) {
            if let Ok(children) = read_sorted_dir(env, &subagent_dir) {
                for child in children {
                    if child.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                        let mut sub = read_session(env, &child, bucket, true);
                        // A sub-agent's transcript records its *parent's*
                        // sessionId. Left as the row's id it would collide with
                        // the parent and with every sibling, and one host pane
                        // naming that id would link to all of them. Keep the
                        // parent in `parent_session_id` and make the row's own
                        // id distinct by its transcript's filename.
                        sub.parent_session_id = Some(std::mem::replace(
                            &mut sub.id,
                            format!(
                                "{}/{}",
                                session.id,
                                child
                                    .file_stem()
                                    .and_then(|stem| stem.to_str())
                                    .unwrap_or("subagent")
                            ),
                        ));
                        // A sub-agent inherits the parent's directory; its own
                        // transcript may not repeat it.
                        if sub.cwd.is_none() {
                            sub.cwd.clone_from(&session.cwd);
                        }
                        report.agent_sessions.push(sub);
                    }
                }
            }
        }
        report.agent_sessions.push(session);
    }
    false
}

fn read_session(env: &Machine<'_>, path: &Path, bucket: &Path, is_subagent: bool) -> AgentSession {
    let header = read_header(env, path);
    let bucket_name = bucket
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    let id = header.session_id.clone().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string()
    });

    let mut evidence = vec![Evidence::file(path)];
    let confidence = if header.is_complete() {
        evidence.push(
            Evidence::file(path).saying("cwd, gitBranch and version read from transcript records"),
        );
        Confidence::Certain
    } else {
        evidence.push(
            Evidence::dir(bucket)
                .saying(format!("no readable header; bucket name {bucket_name} is dash-encoded and cannot be decoded unambiguously")),
        );
        Confidence::Possible
    };

    AgentSession {
        id,
        agent: AgentId::Claude,
        cwd: header.cwd,
        git_branch: header.git_branch,
        title: None,
        transcript: Some(path.to_path_buf()),
        modified_unix: env.modified_unix(path),
        agent_version: header.version,
        is_subagent,
        parent_session_id: None,
        confidence,
        evidence,
    }
}

/// Read `cwd`, `gitBranch`, `version` and `sessionId` from the transcript.
///
/// Not just the first line: the first record is often a `queue-operation`,
/// which carries `sessionId` but no `cwd` (observed on a live session). So
/// this reads forward until it has both, or runs out of budget.
///
/// The budget is a line count, not a byte count, because a single record can
/// legitimately be a megabyte — one tool result with a large stdout — and a
/// byte budget would stop mid-record and find nothing.
fn read_header(env: &Machine<'_>, path: &Path) -> Header {
    const MAX_RECORDS: usize = 32;
    /// Enough for the header records of every transcript seen, and a bound the
    /// remote case needs: without it a scan pulls whole transcripts across the
    /// network to read their first few fields.
    const MAX_BYTES: usize = 256 * 1024;

    let Ok(bytes) = env.read_prefix_io(path, MAX_BYTES) else {
        return Header::default();
    };
    let text = String::from_utf8_lossy(bytes.as_slice());
    let mut header = Header::default();
    // The last line of a truncated read is very likely a partial record; it
    // simply fails to parse and is skipped, which is the same handling any
    // other unparseable line gets.
    for line in text.lines().take(MAX_RECORDS) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let string = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        if header.session_id.is_none() {
            header.session_id = string("sessionId");
        }
        if header.cwd.is_none() {
            header.cwd = string("cwd").map(PathBuf::from);
        }
        if header.git_branch.is_none() {
            header.git_branch = string("gitBranch");
        }
        if header.version.is_none() {
            header.version = string("version");
        }
        if header.is_complete() && header.git_branch.is_some() && header.version.is_some() {
            break;
        }
    }
    header
}

/// Resumable conversations found so far, ignoring sub-agent transcripts.
fn top_level_count(report: &ToolReport) -> usize {
    report
        .agent_sessions
        .iter()
        .filter(|session| !session.is_subagent)
        .count()
}

fn read_sorted_dir(env: &Machine<'_>, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    Ok(env
        .list_io(dir)?
        .into_iter()
        .map(|entry| entry.path)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_path_the_way_a_live_install_did() {
        // Confirmed on this machine: /home/user/fresh -> -home-user-fresh.
        assert_eq!(
            encode_project_dir(Path::new("/home/user/fresh")),
            "-home-user-fresh"
        );
    }

    #[test]
    fn runs_of_separators_are_not_collapsed() {
        // The encoding is one dash per character, so a dotfile directory
        // produces a double dash. Collapsing runs stops matching real buckets.
        assert_eq!(encode_project_dir(Path::new("/a/.claude")), "-a--claude");
    }

    #[test]
    fn a_windows_drive_root_keeps_its_trailing_separator_and_its_case() {
        // Orca's own doc comment claims `C:\` encodes to `c--`, but the
        // function it documents never lowercases — and the comment two lines
        // above it explains why it must not, or the encoding would stop
        // matching real directories on Windows. The code is the authority.
        assert_eq!(encode_project_dir(Path::new("C:/")), "C--");
    }

    #[test]
    fn the_encoding_is_lossy_which_is_why_we_read_cwd_from_the_transcript() {
        assert_eq!(
            encode_project_dir(Path::new("/a-b")),
            encode_project_dir(Path::new("/a/b")),
            "two different directories collide, so decoding a bucket name is a guess"
        );
    }

    fn write_install(home: &Path, bucket: &str, session: &str, lines: &[&str]) -> PathBuf {
        let dir = home.join(".claude").join("projects").join(bucket);
        std::fs::create_dir_all(&dir).expect("create bucket");
        let path = dir.join(format!("{session}.jsonl"));
        std::fs::write(&path, lines.join("\n")).expect("write transcript");
        path
    }

    #[test]
    fn reads_cwd_and_branch_from_the_transcript_not_the_directory_name() {
        let home = tempfile::tempdir().expect("tempdir");
        // The bucket name deliberately disagrees with the recorded cwd; the
        // transcript is the authority.
        write_install(
            home.path(),
            "-wrong-place",
            "11111111-2222-3333-4444-555555555555",
            &[
                r#"{"type":"queue-operation","sessionId":"11111111-2222-3333-4444-555555555555"}"#,
                r#"{"type":"user","sessionId":"11111111-2222-3333-4444-555555555555","cwd":"/home/user/fresh","gitBranch":"main","version":"2.1.273"}"#,
            ],
        );

        let report = scan(&Machine::local(home.path()), 100);

        assert_eq!(report.agent_sessions.len(), 1);
        let session = &report.agent_sessions[0];
        assert_eq!(session.cwd.as_deref(), Some(Path::new("/home/user/fresh")));
        assert_eq!(session.git_branch.as_deref(), Some("main"));
        assert_eq!(session.agent_version.as_deref(), Some("2.1.273"));
        assert_eq!(session.confidence, Confidence::Certain);
    }

    #[test]
    fn a_header_past_the_first_record_is_still_found() {
        // The first record of a real session was a `queue-operation` with no
        // cwd, so a first-line-only reader would report cwd: None.
        let home = tempfile::tempdir().expect("tempdir");
        let mut lines = vec![r#"{"type":"queue-operation","sessionId":"abc"}"#.to_string()];
        lines.extend((0..10).map(|i| format!(r#"{{"type":"noise","n":{i}}}"#)));
        lines.push(r#"{"type":"user","sessionId":"abc","cwd":"/srv/app"}"#.to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_install(home.path(), "-srv-app", "abc", &refs);

        let report = scan(&Machine::local(home.path()), 100);
        assert_eq!(
            report.agent_sessions[0].cwd.as_deref(),
            Some(Path::new("/srv/app"))
        );
    }

    #[test]
    fn subagent_transcripts_are_marked_not_listed_as_sessions() {
        let home = tempfile::tempdir().expect("tempdir");
        let parent = write_install(
            home.path(),
            "-repo",
            "parent-id",
            &[r#"{"sessionId":"parent-id","cwd":"/repo"}"#],
        );
        let subagents = parent.with_extension("");
        std::fs::create_dir_all(&subagents).expect("create subagents dir");
        std::fs::write(
            subagents.join("task-1.jsonl"),
            r#"{"sessionId":"parent-id"}"#,
        )
        .expect("write subagent");

        let report = scan(&Machine::local(home.path()), 100);

        let top: Vec<_> = report
            .agent_sessions
            .iter()
            .filter(|s| !s.is_subagent)
            .collect();
        let subs: Vec<_> = report
            .agent_sessions
            .iter()
            .filter(|s| s.is_subagent)
            .collect();
        assert_eq!(top.len(), 1, "one resumable session");
        assert_eq!(subs.len(), 1, "one sub-agent transcript, marked as such");
        assert_eq!(
            subs[0].cwd.as_deref(),
            Some(Path::new("/repo")),
            "a sub-agent inherits its parent's working directory"
        );
    }

    #[test]
    fn a_subagent_gets_its_own_id_and_names_its_parent() {
        // A sub-agent transcript records the PARENT's sessionId. Used as the
        // row's own id it collides with the parent and every sibling, and one
        // host pane naming that id links to all of them at once.
        let home = tempfile::tempdir().expect("tempdir");
        let parent = write_install(
            home.path(),
            "-repo",
            "parent-id",
            &[r#"{"sessionId":"parent-id","cwd":"/repo"}"#],
        );
        let subagents = parent.with_extension("");
        std::fs::create_dir_all(&subagents).expect("create subagents dir");
        for task in ["task-1", "task-2"] {
            std::fs::write(
                subagents.join(format!("{task}.jsonl")),
                r#"{"sessionId":"parent-id"}"#,
            )
            .expect("write subagent");
        }

        let report = scan(&Machine::local(home.path()), 100);

        let ids: std::collections::HashSet<&str> = report
            .agent_sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        assert_eq!(ids.len(), 3, "every row needs a distinct id: {ids:?}");
        for sub in report.agent_sessions.iter().filter(|s| s.is_subagent) {
            assert_eq!(sub.parent_session_id.as_deref(), Some("parent-id"));
            assert_ne!(sub.id, "parent-id");
        }
    }

    #[test]
    fn the_limit_counts_conversations_not_transcript_files() {
        // One heavily-delegating session must not exhaust the whole budget:
        // a limit of 2 should still return 2 resumable conversations.
        let home = tempfile::tempdir().expect("tempdir");
        for session in ["a", "b", "c"] {
            let path = write_install(
                home.path(),
                "-repo",
                session,
                &[&format!(r#"{{"sessionId":"{session}","cwd":"/repo"}}"#)],
            );
            let subagents = path.with_extension("");
            std::fs::create_dir_all(&subagents).expect("create subagents dir");
            for task in 0..4 {
                std::fs::write(
                    subagents.join(format!("t{task}.jsonl")),
                    format!(r#"{{"sessionId":"{session}"}}"#),
                )
                .expect("write subagent");
            }
        }

        let report = scan(&Machine::local(home.path()), 2);

        let resumable = report
            .agent_sessions
            .iter()
            .filter(|session| !session.is_subagent)
            .count();
        assert_eq!(resumable, 2, "the limit is a budget of conversations");
        assert!(
            report.problems.iter().any(|p| p.contains("limit")),
            "truncation must be reported: {:?}",
            report.problems
        );
    }

    #[test]
    fn an_unreadable_transcript_still_yields_a_session_at_lower_confidence() {
        let home = tempfile::tempdir().expect("tempdir");
        write_install(home.path(), "-repo", "opaque", &["not json at all"]);

        let report = scan(&Machine::local(home.path()), 100);

        assert_eq!(report.agent_sessions.len(), 1);
        assert_eq!(report.agent_sessions[0].id, "opaque");
        assert_eq!(report.agent_sessions[0].cwd, None);
        assert_eq!(report.agent_sessions[0].confidence, Confidence::Possible);
    }

    #[test]
    fn a_missing_install_is_absent_not_an_error() {
        let home = tempfile::tempdir().expect("tempdir");
        let report = scan(&Machine::local(home.path()), 100);
        assert!(report.is_empty());
        assert!(report.problems.is_empty());
    }
}
