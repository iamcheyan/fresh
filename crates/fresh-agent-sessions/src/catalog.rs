//! Where each coding agent keeps its transcripts.
//!
//! Most coding agents share one shape: a root directory under `$HOME`,
//! relocatable by an environment variable, holding one file (or one directory)
//! per conversation. They differ only in the root, the extension, and which
//! files in the tree actually count as a session. That is a *table*, not
//! twenty hand-written scanners, so this module is the table plus the one
//! walker that reads it.
//!
//! # Provenance
//!
//! These entries were not guessed. Two orchestrators had already worked these
//! layouts out and publish the result in their source, so the paths here were
//! cross-checked against Orca's `session-scanner-agent-sources.ts` (MIT) and
//! Herdr's session handling (Apache-2.0); the stores reachable from this
//! machine (Claude Code, Codex, tmux) were then confirmed against a real
//! install. Each entry's comment says which it is, because a confirmed entry
//! and a cross-checked one deserve different amounts of trust when a scan
//! comes back empty.
//!
//! What was taken from those projects is *facts* — directory paths,
//! environment-variable names, filename patterns — and no code or prose. See
//! `THIRD-PARTY-NOTES.md` at the crate root.
//!
//! # What is deliberately *not* here
//!
//! Two agents do not fit the table and are not forced into it:
//!
//! * **Claude Code** encodes the working directory into its project directory
//!   name and writes the real one inside the transcript, so it gets
//!   [`crate::providers::claude_code`].
//! * **Codex** shards by date and carries a session index, so it gets
//!   [`crate::providers::codex`].
//!
//! Agents whose store is a SQLite database (opencode) are listed with
//! [`Layout::Opaque`]: discoverable, but not parsed, because reading them
//! would mean linking a SQLite driver into an editor.

use std::path::{Path, PathBuf};

use crate::env::{modified_unix, Env};
use crate::model::{AgentId, AgentSession, Confidence, Evidence};

/// Which files under a root count as sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRule {
    /// Any file with a matching extension.
    Any,
    /// Exactly this basename — used by agents that put one manifest in each
    /// session directory (`summary.json`, `metadata.json`).
    BaseName(&'static str),
    /// A basename with this prefix (`session_*`).
    BaseNamePrefix(&'static str),
    /// Some path component equals this. Cursor buries transcripts under
    /// `agent-transcripts/`, and the sibling directories are not sessions.
    PathContains(&'static str),
    /// `state.json` whose parent directory starts with `session_` — Kimi's
    /// shape, where a sibling `agents/*/wire.jsonl` must not be picked up.
    KimiState,
}

impl FileRule {
    fn accepts(self, path: &Path) -> bool {
        let base = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        match self {
            Self::Any => true,
            Self::BaseName(name) => base == name,
            Self::BaseNamePrefix(prefix) => base.starts_with(prefix),
            Self::PathContains(segment) => path
                .components()
                .any(|component| component.as_os_str() == segment),
            Self::KimiState => {
                base == "state.json"
                    && path
                        .parent()
                        .and_then(Path::file_name)
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("session_"))
            }
        }
    }
}

/// Which directories under a root to descend into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirRule {
    /// Descend everywhere, up to [`AgentStore::max_depth`].
    Any,
    /// Descend everywhere except directories with this name.
    ///
    /// Claude's `subagents/` holds transcripts that share the parent's session
    /// id and are not independently resumable; surfacing them as top-level
    /// rows just duplicates the parent.
    Skip(&'static str),
}

impl DirRule {
    fn descend(self, name: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Skip(skipped) => name != skipped,
        }
    }
}

/// How to get a session id out of a transcript path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// The file stem is the session id. The common case.
    FileStem,
    /// The *parent directory's* name is the session id — used by agents whose
    /// session is a directory holding a manifest plus message files.
    ParentDirName,
    /// Discoverable but not parsed; the id is the path.
    Opaque,
}

impl Layout {
    fn session_id(self, path: &Path) -> (String, Confidence) {
        match self {
            Self::FileStem => path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| (stem.to_string(), Confidence::Probable))
                .unwrap_or_else(|| (path.to_string_lossy().into_owned(), Confidence::Possible)),
            Self::ParentDirName => path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(|name| (name.to_string(), Confidence::Probable))
                .unwrap_or_else(|| (path.to_string_lossy().into_owned(), Confidence::Possible)),
            Self::Opaque => (path.to_string_lossy().into_owned(), Confidence::Possible),
        }
    }
}

/// One agent's transcript store.
#[derive(Debug, Clone, Copy)]
pub struct AgentStore {
    pub agent: AgentId,
    pub display_name: &'static str,
    /// Environment variables that relocate the store's *base*, in priority
    /// order. For agents whose variable names the sessions directory itself,
    /// leave [`Self::suffix`] empty.
    pub base_env: &'static [&'static str],
    /// The base directory under `$HOME` when no variable applies.
    pub base_segments: &'static [&'static str],
    /// Appended to the base, however the base was resolved.
    pub suffix: &'static [&'static str],
    /// Extensions a transcript can have, lowercase and dotted.
    pub extensions: &'static [&'static str],
    pub file_rule: FileRule,
    pub dir_rule: DirRule,
    /// How deep to walk below the root. Bounded on purpose: these roots sit in
    /// `$HOME`, and an unbounded walk of a mis-set `*_HOME` variable would
    /// crawl the user's whole disk.
    pub max_depth: usize,
    pub layout: Layout,
    /// Top-level JSON keys that hold the working directory, tried in order
    /// against the transcript's first line. Empty when the format has none.
    pub cwd_fields: &'static [&'static str],
    /// True once this entry has been checked against a real install rather
    /// than only transcribed from another tool's source.
    pub verified_against_install: bool,
}

impl AgentStore {
    /// The directory to walk, with the variable that moved it (if any).
    pub fn root(&self, env: &Env) -> (PathBuf, Option<Evidence>) {
        let default = env.home_join(self.base_segments);
        let mut moved_by = None;
        for name in self.base_env {
            if let Some(value) = env.var(name) {
                if Path::new(value).is_absolute() {
                    moved_by = Some(Evidence::env_var(*name, value));
                    break;
                }
            }
        }
        let base = env.dir_override(self.base_env, default);
        let mut root = base;
        for segment in self.suffix {
            root.push(segment);
        }
        (root, moved_by)
    }
}

/// Every agent discovered by the generic walker.
///
/// Claude Code and Codex are absent on purpose — see the module docs.
pub const AGENT_STORES: &[AgentStore] = &[
    // Gemini CLI writes session JSON under a `tmp` directory keyed by project
    // hash. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Gemini,
        display_name: "Gemini CLI",
        base_env: &[],
        base_segments: &[".gemini", "tmp"],
        suffix: &[],
        extensions: &[".json", ".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 4,
        layout: Layout::FileStem,
        cwd_fields: &["cwd", "projectRoot"],
        verified_against_install: false,
    },
    // Copilot CLI. `COPILOT_HOME` moves the base; sessions are under
    // `session-state`. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Copilot,
        display_name: "GitHub Copilot CLI",
        base_env: &["COPILOT_HOME"],
        base_segments: &[".copilot"],
        suffix: &["session-state"],
        extensions: &[".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd", "workingDirectory"],
        verified_against_install: false,
    },
    // Cursor's agent transcripts sit under `~/.cursor/projects/<project>/
    // agent-transcripts/`; the sibling directories under a project are not
    // sessions, hence the path rule. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Cursor,
        display_name: "Cursor Agent",
        base_env: &[],
        base_segments: &[".cursor", "projects"],
        suffix: &[],
        extensions: &[".jsonl"],
        file_rule: FileRule::PathContains("agent-transcripts"),
        dir_rule: DirRule::Any,
        max_depth: 4,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // Grok writes one directory per session with a `summary.json` in it, so
    // the session id is the directory's name. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Grok,
        display_name: "Grok CLI",
        base_env: &["GROK_SESSIONS_DIR"],
        base_segments: &[".grok", "sessions"],
        suffix: &[],
        extensions: &[".json"],
        file_rule: FileRule::BaseName("summary.json"),
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::ParentDirName,
        cwd_fields: &["cwd", "workspace"],
        verified_against_install: false,
    },
    // Devin's ATIF transcripts live under `<DEVIN_HOME>/transcripts`.
    // Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Devin,
        display_name: "Devin CLI",
        base_env: &["DEVIN_HOME"],
        base_segments: &[".local", "share", "devin", "cli"],
        suffix: &["transcripts"],
        extensions: &[".json"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // Hermes names its session files `session_*`. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Hermes,
        display_name: "Hermes",
        base_env: &[],
        base_segments: &[".hermes", "sessions"],
        suffix: &[],
        extensions: &[".json"],
        file_rule: FileRule::BaseNamePrefix("session_"),
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // Atlassian Rovo Dev: one directory per session with a `metadata.json`.
    // Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Rovo,
        display_name: "Rovo Dev",
        base_env: &[],
        base_segments: &[".rovodev", "sessions"],
        suffix: &[],
        extensions: &[".json"],
        file_rule: FileRule::BaseName("metadata.json"),
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::ParentDirName,
        cwd_fields: &["cwd", "workspace"],
        verified_against_install: false,
    },
    // Pi's variable names the sessions directory itself, not a base — so the
    // suffix is empty. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Pi,
        display_name: "Pi",
        base_env: &["PI_CODING_AGENT_DIR"],
        base_segments: &[".pi", "agent", "sessions"],
        suffix: &[],
        extensions: &[".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // OMP, same shape as Pi. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Omp,
        display_name: "OMP",
        base_env: &["OMP_CODING_AGENT_DIR"],
        base_segments: &[".omp", "agent", "sessions"],
        suffix: &[],
        extensions: &[".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // Prime Agent brands Pi's contract with its own prefix. Path facts cross-checked against
    // Orca's scanner table.
    AgentStore {
        agent: AgentId::PrimeAgent,
        display_name: "Prime Agent",
        base_env: &["PRIME_CODING_AGENT_DIR", "PRIME_AGENT_SESSIONS_DIR"],
        base_segments: &[".prime", "agent", "sessions"],
        suffix: &[],
        extensions: &[".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // OpenClaw keeps sessions under `<state dir>/agents/**/sessions`.
    // Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::OpenClaw,
        display_name: "OpenClaw",
        base_env: &["OPENCLAW_STATE_DIR"],
        base_segments: &[".openclaw"],
        suffix: &["agents"],
        extensions: &[".jsonl"],
        file_rule: FileRule::PathContains("sessions"),
        dir_rule: DirRule::Any,
        max_depth: 4,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // Factory's droid. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Droid,
        display_name: "Factory Droid",
        base_env: &[],
        base_segments: &[".factory", "sessions"],
        suffix: &[],
        extensions: &[".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // A Cline session is a directory holding a single manifest at its top
    // level, so depth 1 is as far as this walk can usefully go. Path facts
    // cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Cline,
        display_name: "Cline",
        base_env: &["CLINE_SESSION_DATA_DIR"],
        base_segments: &[".cline", "data", "sessions"],
        suffix: &[],
        extensions: &[".json"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 1,
        layout: Layout::ParentDirName,
        cwd_fields: &["cwd", "workspace"],
        verified_against_install: false,
    },
    // Kimi: `<sessions>/wd_*/session_*/state.json`. Path facts cross-checked against Orca's scanner table.
    AgentStore {
        agent: AgentId::Kimi,
        display_name: "Kimi Code",
        base_env: &["KIMI_CODE_HOME"],
        base_segments: &[".kimi-code"],
        suffix: &["sessions"],
        extensions: &[".json"],
        file_rule: FileRule::KimiState,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::ParentDirName,
        cwd_fields: &["cwd", "workingDirectory"],
        verified_against_install: false,
    },
    // opencode keeps state in SQLite; listed so a scan reports it as present
    // rather than silently missing it, but not parsed.
    AgentStore {
        agent: AgentId::OpenCode,
        display_name: "opencode",
        base_env: &["OPENCODE_DATA_DIR"],
        base_segments: &[".local", "share", "opencode"],
        suffix: &[],
        extensions: &[".db", ".sqlite"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 2,
        layout: Layout::Opaque,
        cwd_fields: &[],
        verified_against_install: false,
    },
    // Sourcegraph Amp.
    AgentStore {
        agent: AgentId::Amp,
        display_name: "Amp",
        base_env: &["AMP_DATA_DIR"],
        base_segments: &[".amp", "threads"],
        suffix: &[],
        extensions: &[".json", ".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 3,
        layout: Layout::FileStem,
        cwd_fields: &["cwd"],
        verified_against_install: false,
    },
    // Aider's per-repo chat history is a file in the repo, not a home-dir
    // store; what lives in `$HOME` is the input history. Listed so its
    // presence is reported.
    AgentStore {
        agent: AgentId::Aider,
        display_name: "Aider",
        base_env: &[],
        base_segments: &[".aider"],
        suffix: &[],
        extensions: &[".md", ".json"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 2,
        layout: Layout::Opaque,
        cwd_fields: &[],
        verified_against_install: false,
    },
    // Block's goose.
    AgentStore {
        agent: AgentId::Goose,
        display_name: "goose",
        base_env: &["GOOSE_DATA_DIR"],
        base_segments: &[".local", "share", "goose", "sessions"],
        suffix: &[],
        extensions: &[".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 2,
        layout: Layout::FileStem,
        cwd_fields: &["cwd", "working_dir"],
        verified_against_install: false,
    },
    // Qwen Code, a Gemini-CLI fork, keeps Gemini's layout under its own home.
    AgentStore {
        agent: AgentId::Qwen,
        display_name: "Qwen Code",
        base_env: &[],
        base_segments: &[".qwen", "tmp"],
        suffix: &[],
        extensions: &[".json", ".jsonl"],
        file_rule: FileRule::Any,
        dir_rule: DirRule::Any,
        max_depth: 4,
        layout: Layout::FileStem,
        cwd_fields: &["cwd", "projectRoot"],
        verified_against_install: false,
    },
];

/// Walk one store's root and turn every transcript into an [`AgentSession`].
///
/// Errors are not returned: a store whose root does not exist is simply not
/// installed, and an unreadable subdirectory is one the caller could not have
/// fixed either. Both are reported through `problems`.
pub fn scan_store(
    store: &AgentStore,
    env: &Env,
    limit: usize,
    problems: &mut Vec<String>,
) -> StoreScan {
    let (root, moved_by) = store.root(env);
    let mut evidence = Vec::new();
    if let Some(moved) = moved_by {
        // Recorded whether or not the directory exists: "you pointed this
        // variable somewhere" is true either way, and is exactly what someone
        // debugging an empty scan needs to see. It is *not* evidence the agent
        // is installed — `root_exists` below is what says that.
        evidence.push(moved);
    }
    if !root.is_dir() {
        return StoreScan {
            root_exists: false,
            sessions: Vec::new(),
            evidence,
        };
    }
    evidence.push(Evidence::dir(&root).saying(format!("{} session store", store.display_name)));

    // One past the limit, so a truncated walk is distinguishable from one that
    // happened to land exactly on it.
    let mut files = Vec::new();
    collect_files(&root, &root, store, 0, limit + 1, &mut files, problems);
    files.sort();
    let truncated = files.len() > limit;
    files.truncate(limit);
    if truncated {
        problems.push(format!(
            "{}: stopped at the {limit}-session limit; the rows reported are the first {limit} \
             in path order, not the {limit} most recently used",
            store.display_name
        ));
    }

    if store.layout == Layout::Opaque {
        // The store is discoverable but its format is not parsed (opencode's
        // SQLite, Aider's per-repo history). Emitting a row per file would
        // invent sessions we cannot actually identify, so report the files as
        // evidence and leave the session list empty — which is what this
        // crate's docs have always claimed happens.
        for path in &files {
            evidence.push(Evidence::file(path).saying(format!(
                "{} state; format not parsed, so no sessions are reported from it",
                store.display_name
            )));
        }
        return StoreScan {
            root_exists: true,
            sessions: Vec::new(),
            evidence,
        };
    }

    let sessions = files
        .into_iter()
        .map(|path| session_from_transcript(store, &root, path))
        .collect();
    StoreScan {
        root_exists: true,
        sessions,
        evidence,
    }
}

/// What one store's walk found.
///
/// `root_exists` is separate from `evidence` on purpose: an environment
/// variable pointing at a directory that is not there is worth reporting, but
/// it is not grounds for calling the agent installed.
#[derive(Debug)]
pub struct StoreScan {
    /// Whether the store's root directory is actually present.
    pub root_exists: bool,
    pub sessions: Vec<AgentSession>,
    pub evidence: Vec<Evidence>,
}

fn collect_files(
    root: &Path,
    dir: &Path,
    store: &AgentStore,
    depth: usize,
    limit: usize,
    out: &mut Vec<PathBuf>,
    problems: &mut Vec<String>,
) {
    if out.len() >= limit || depth > store.max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            problems.push(format!("{}: {err}", dir.display()));
            return;
        }
    };
    // Sorted, so a truncated scan is at least deterministic rather than
    // whatever order the filesystem handed back.
    let mut children: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    children.sort();

    for path in children {
        if out.len() >= limit {
            return;
        }
        // `symlink_metadata`, not `metadata`: a symlink pointing back up the
        // tree would otherwise make this walk unbounded.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if store.dir_rule.descend(name) {
                collect_files(root, &path, store, depth + 1, limit, out, problems);
            }
        } else if meta.is_file() && matches_file(store, root, &path) {
            out.push(path);
        }
    }
}

fn matches_file(store: &AgentStore, root: &Path, path: &Path) -> bool {
    let extension_ok = store.extensions.iter().any(|wanted| {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| wanted.trim_start_matches('.').eq_ignore_ascii_case(ext))
    });
    if !extension_ok {
        return false;
    }
    // Path rules are evaluated relative to the root: an absolute path that
    // happens to contain `sessions` higher up (a user whose home is
    // `/home/sessions`) must not count as a match.
    let relative = path.strip_prefix(root).unwrap_or(path);
    store.file_rule.accepts(relative)
}

fn session_from_transcript(store: &AgentStore, root: &Path, path: PathBuf) -> AgentSession {
    let (id, id_confidence) = store.layout.session_id(&path);
    let cwd = read_cwd_field(&path, store.cwd_fields);
    // The record is only as good as its weakest part, and for a path-derived
    // session that is always the id: a cwd read out of the transcript is
    // something the agent wrote down, but the id is still our reading of a
    // filename convention. So the id's confidence is the record's.
    let confidence = id_confidence;
    AgentSession {
        id,
        agent: store.agent,
        cwd,
        git_branch: None,
        title: None,
        transcript: Some(path.clone()),
        modified_unix: modified_unix(&path),
        agent_version: None,
        is_subagent: false,
        parent_session_id: None,
        confidence,
        evidence: vec![Evidence::file(&path).saying(format!(
            "{} transcript under {}",
            store.display_name,
            root.display()
        ))],
    }
}

/// The first of `fields` present as a string in the file's first JSON object.
///
/// Why only the first line: these files are append-only transcripts that grow
/// to megabytes, and the working directory is written in the header record. A
/// scan that parsed every line to find it would read the user's entire agent
/// history off disk to build a list.
pub fn read_cwd_field(path: &Path, fields: &[&str]) -> Option<PathBuf> {
    if fields.is_empty() {
        return None;
    }
    let text = read_first_line(path, 64 * 1024)?;
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    for field in fields {
        if let Some(found) = value.get(*field).and_then(serde_json::Value::as_str) {
            if !found.is_empty() {
                return Some(PathBuf::from(found));
            }
        }
    }
    None
}

/// Read at most `max_bytes` of `path`, stopping at the first newline.
pub fn read_first_line(path: &Path, max_bytes: usize) -> Option<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; max_bytes];
    let read = file.read(&mut buffer).ok()?;
    buffer.truncate(read);
    let text = String::from_utf8_lossy(&buffer).into_owned();
    // A JSON file (not JSONL) has no newline before its closing brace, so fall
    // back to the whole buffer rather than returning nothing.
    match text.split_once('\n') {
        Some((line, _)) => Some(line.to_string()),
        None => Some(text),
    }
}
