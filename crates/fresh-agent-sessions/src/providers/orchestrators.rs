//! Orca, Superset and super.engineering.
//!
//! Three desktop agent orchestrators with the same idea — one git worktree per
//! task, agents running inside it — and three different ways of writing it
//! down. They share a module because they share a discovery shape: a state
//! directory under `$HOME`, a JSON blob describing worktrees, and a CLI.
//!
//! # Orca
//!
//! An Electron app (MIT, `stablyai/orca`). Its user-data directory is
//! `$ORCA_USER_DATA`, else `$XDG_DATA_HOME/Orca`, else `~/.orca` — read off
//! `src/main/orcad/orcad-app-paths.ts`. Persisted state is one JSON file per
//! profile, whose `PersistedState` carries `repos`, `projects`,
//! `folderWorkspaces` and `worktreeMeta` (a map keyed by worktree id).
//!
//! Orca also keeps relay session state under `~/.orca/sessions`, and installs
//! agent hooks under `~/.orca/agent-hooks` — the latter is a reliable
//! "Orca has managed agents on this machine" signal even when no state file
//! parses.
//!
//! # Superset
//!
//! A macOS desktop app plus a CLI. State lives under `~/.superset`:
//! `config.json` (auth token and active org), `sessions/` (project-less
//! workspaces, each its own git repo), `bin/superset` (a shim the app writes
//! on launch) and `host/<organizationId>/manifest.json` — the local host
//! server's endpoint and token, written so other clients can find it.
//!
//! The manifest is the interesting artefact: it means a *live* Superset host
//! can be queried. This crate reports the manifest's existence and its
//! endpoint, and deliberately does not use the token in it. Reading a
//! credential off disk to make an authenticated request is not something a
//! discovery pass should do on its own.
//!
//! # super.engineering
//!
//! A native macOS app (Rust) with an `sc` CLI that has `--json` output. There
//! is nothing to read on Linux, and this provider says so with
//! [`Installed::Unsupported`] rather than reporting it as absent — "you do not
//! have it" and "I cannot see it from here" are different answers, and only
//! one of them is true when someone runs this on a Linux CI box.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::env::{Env, Platform};
use crate::model::{Confidence, Evidence, Installed, ToolId, ToolReport, Workspace};

// ---------------------------------------------------------------- Orca

/// Orca's user-data directory: `$ORCA_USER_DATA`, `$XDG_DATA_HOME/Orca`, or
/// `~/.orca`.
pub fn orca_user_data(env: &Env) -> PathBuf {
    if let Some(explicit) = env.var("ORCA_USER_DATA").map(PathBuf::from) {
        if explicit.is_absolute() {
            return explicit;
        }
    }
    match env.var("XDG_DATA_HOME").map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg.join("Orca"),
        _ => env.home_join([".orca"]),
    }
}

pub fn scan_orca(env: &Env) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::Orca, "Orca");
    let data = orca_user_data(env);

    if let Some(path) = env.which("orca") {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: None,
            executable: Some(path),
        };
    }
    if !data.is_dir() {
        return report;
    }
    report
        .evidence
        .push(Evidence::dir(&data).saying("Orca user-data directory"));
    if matches!(report.installed, Installed::Absent) {
        report.installed = Installed::Present {
            version: None,
            executable: None,
        };
    }

    // Corroborating artefacts. None of them carry worktrees, but each proves
    // Orca has actually run here rather than just been unpacked.
    for (relative, meaning) in [
        (
            "agent-hooks",
            "installed agent hooks; Orca manages agents here",
        ),
        ("sessions", "relay workspace session state"),
        ("keybindings.json", "user keybindings"),
    ] {
        let path = data.join(relative);
        if path.exists() {
            report.evidence.push(Evidence::dir(&path).saying(meaning));
        }
    }

    for state_file in profile_state_files(&data) {
        read_orca_state(&state_file, &mut report);
    }
    report
}

/// Every persisted-state JSON file under the user-data directory.
///
/// Orca keys state by profile, and the profile index names the files. Rather
/// than reimplement that indirection, this looks for the JSON files that
/// actually parse as state — a profile added by a newer Orca is then found for
/// free, where a hard-coded filename would miss it.
fn profile_state_files(data: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for dir in [data.to_path_buf(), data.join("profiles")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        paths.sort();
        candidates.extend(paths);
    }
    candidates
}

fn read_orca_state(path: &Path, report: &mut ToolReport) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(state) = serde_json::from_str::<OrcaState>(&text) else {
        return;
    };
    // Every JSON file in the directory parses as `OrcaState` — all its fields
    // default — so "did it parse?" cannot distinguish state from keybindings.
    // Requiring a schema version *and* some content does.
    if state.schema_version.is_none() || (state.worktree_meta.is_empty() && state.repos.is_empty())
    {
        return;
    }

    let evidence = Evidence::file(path).saying("Orca persisted state");
    report.evidence.push(evidence.clone());

    let repo_paths: Vec<(String, PathBuf)> = state
        .repos
        .iter()
        .filter_map(|repo| Some((repo.id.clone()?, repo.path.clone()?)))
        .collect();

    let mut ids: Vec<&String> = state.worktree_meta.keys().collect();
    ids.sort();
    for id in ids {
        let meta = &state.worktree_meta[id];
        report.workspaces.push(Workspace {
            id: id.clone(),
            name: meta.name.clone().or_else(|| meta.branch.clone()),
            path: meta.path.clone(),
            repo: meta
                .repo_id
                .as_ref()
                .and_then(|repo_id| {
                    repo_paths
                        .iter()
                        .find(|(id, _)| id == repo_id)
                        .map(|(_, path)| path.clone())
                })
                .or_else(|| meta.repo_path.clone()),
            branch: meta.branch.clone(),
            // Orca's isolation primitive *is* the git worktree — every entry
            // in this map is one.
            is_worktree: Some(true),
            confidence: Confidence::Certain,
            evidence: vec![evidence.clone()],
        });
    }
}

/// The part of Orca's `PersistedState` that names worktrees.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrcaState {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    repos: Vec<OrcaRepo>,
    #[serde(default)]
    worktree_meta: std::collections::HashMap<String, OrcaWorktreeMeta>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrcaRepo {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrcaWorktreeMeta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    repo_path: Option<PathBuf>,
}

// ------------------------------------------------------------ Superset

pub fn scan_superset(env: &Env) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::Superset, "Superset");
    let root = env.home_join([".superset"]);

    // The app writes a shim here on launch, so it is a stronger "has run"
    // signal than the CLI being on PATH.
    let shim = root.join("bin").join("superset");
    if let Some(path) = env
        .which("superset")
        .or_else(|| shim.is_file().then(|| shim.clone()))
    {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: None,
            executable: Some(path),
        };
    }
    if !root.is_dir() {
        return report;
    }
    report
        .evidence
        .push(Evidence::dir(&root).saying("Superset state directory"));
    if matches!(report.installed, Installed::Absent) {
        report.installed = Installed::Present {
            version: None,
            executable: None,
        };
    }

    let config = root.join("config.json");
    if config.is_file() {
        // Named, never read: this file holds a session token.
        report.evidence.push(
            Evidence::file(&config)
                .saying("CLI auth token and active organization; not read by this crate"),
        );
    }

    // A running host server per organization.
    if let Ok(orgs) = std::fs::read_dir(root.join("host")) {
        let mut dirs: Vec<PathBuf> = orgs
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        dirs.sort();
        for org in dirs {
            let manifest = org.join("manifest.json");
            if !manifest.is_file() {
                continue;
            }
            let endpoint = std::fs::read_to_string(&manifest)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| {
                    value
                        .get("endpoint")
                        .or_else(|| value.get("url"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            report
                .evidence
                .push(Evidence::file(&manifest).saying(match endpoint {
                    Some(endpoint) => format!(
                    "local host server manifest; endpoint {endpoint} (auth token present, not read)"
                ),
                    None => "local host server manifest".to_string(),
                }));
        }
    }

    // Project-less workspaces: one managed directory each, its own git repo.
    if let Ok(entries) = std::fs::read_dir(root.join("sessions")) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            report.workspaces.push(Workspace {
                id: name.clone(),
                name: Some(name),
                path: Some(dir.clone()),
                repo: None,
                branch: None,
                // A project-less session is its own repo, not a worktree of
                // another one.
                is_worktree: Some(false),
                confidence: Confidence::Probable,
                evidence: vec![Evidence::dir(&dir)
                    .saying("Superset project-less session (managed folder, own git repo)")],
            });
        }
    }

    report
}

// --------------------------------------------------- super.engineering

pub fn scan_super_engineering(env: &Env) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::SuperEngineering, "super.engineering");

    if let Some(path) = env.which("sc") {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: None,
            executable: Some(path),
        };
    }

    if env.platform() != Platform::MacOs {
        // Do not claim absence for a macOS-only app seen from Linux. If the
        // `sc` CLI is on PATH we already recorded that above; the app itself
        // is simply not inspectable from here.
        if matches!(report.installed, Installed::Absent) {
            report.installed = Installed::Unsupported {
                reason: "super.engineering is a native macOS app; its state is not present on this platform"
                    .to_string(),
            };
        }
        return report;
    }

    let support = env.home_join(["Library", "Application Support"]);
    for name in ["super", "Super", "engineering.super.app", "sh.super.app"] {
        let dir = support.join(name);
        if dir.is_dir() {
            report.evidence.push(
                Evidence::dir(&dir).saying("super.engineering application support directory"),
            );
            report.installed = Installed::Present {
                version: None,
                executable: None,
            };
        }
    }

    for bundle in ["/Applications/Super.app", "/Applications/super.app"] {
        let path = Path::new(bundle);
        if path.exists() {
            report
                .evidence
                .push(Evidence::dir(path).saying("application bundle"));
            report.installed = Installed::Present {
                version: None,
                executable: Some(path.to_path_buf()),
            };
        }
    }

    // The app's own store is undocumented and not shipped as source, so rather
    // than guess at a schema, ask the CLI it ships for exactly this purpose.
    if env.commands_enabled() && env.which("sc").is_some() {
        match env.run("sc", &["worktree", "list", "--json"]) {
            Ok(output) => read_sc_worktrees(&output, &mut report),
            Err(err) => report.note_problem(format!("sc worktree list --json: {err}")),
        }
    }

    report
}

/// Parse `sc worktree list --json`.
///
/// Documented as an array of `{type, id, url, primary}`. Anything else is
/// reported as a problem rather than silently yielding nothing, because a
/// changed contract is exactly what a caller needs to hear about.
fn read_sc_worktrees(output: &str, report: &mut ToolReport) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output.trim()) else {
        report.note_problem("sc worktree list --json: output was not JSON".to_string());
        return;
    };
    let Some(rows) = value.as_array() else {
        report.note_problem("sc worktree list --json: expected an array".to_string());
        return;
    };
    for row in rows {
        let Some(id) = row.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let url = row.get("url").and_then(serde_json::Value::as_str);
        report.workspaces.push(Workspace {
            id: id.to_string(),
            name: row
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            // The documented row is `{type, id, url, primary}` — there is no
            // `path` key, so reading one left every workspace with `path:
            // None` and unable to correlate with anything. The location is in
            // `url`, as a `file://` URL. `path` is still preferred if a future
            // version adds it.
            path: row
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .or_else(|| url.and_then(path_from_file_url)),
            repo: None,
            branch: row
                .get("branch")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            // `primary` marks the repo's own checkout; everything else is a
            // worktree created for a task.
            is_worktree: row
                .get("primary")
                .and_then(serde_json::Value::as_bool)
                .map(|primary| !primary),
            confidence: Confidence::Certain,
            evidence: vec![
                Evidence::command("sc worktree list --json").saying(url.unwrap_or(id).to_string())
            ],
        });
    }
}

/// The filesystem path inside a `file://` URL.
///
/// Deliberately minimal — enough for the local, already-decoded paths `sc`
/// emits, and nothing more. A URL with a host (`file://server/share`) is not a
/// local path and is rejected rather than silently turned into one.
fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = if let Some(stripped) = rest.strip_prefix("localhost/") {
        // `file://localhost/srv/a` is the same place as `file:///srv/a`.
        format!("/{stripped}")
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        return None;
    };
    // Percent-decoding is not attempted: `sc` emits paths that round-trip as
    // written, and a half-hearted decoder would corrupt a directory whose name
    // legitimately contains a percent sign.
    (!path.contains('%')).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::StubRunner;

    #[test]
    fn orca_reads_worktrees_out_of_persisted_state() {
        let home = tempfile::tempdir().expect("tempdir");
        let data = home.path().join(".orca");
        std::fs::create_dir_all(&data).expect("create");
        std::fs::write(
            data.join("state.json"),
            r#"{
              "schemaVersion": 12,
              "repos": [{"id": "r1", "path": "/src/fresh"}],
              "worktreeMeta": {
                "wt-2": {"name": "feature", "path": "/src/fresh-wt/feature",
                         "branch": "feature", "repoId": "r1"}
              }
            }"#,
        )
        .expect("write state");

        let report = scan_orca(&Env::fake(home.path()));

        assert_eq!(report.workspaces.len(), 1);
        let workspace = &report.workspaces[0];
        assert_eq!(workspace.branch.as_deref(), Some("feature"));
        assert_eq!(workspace.repo.as_deref(), Some(Path::new("/src/fresh")));
        assert_eq!(workspace.is_worktree, Some(true));
    }

    #[test]
    fn orca_ignores_json_files_that_are_not_state() {
        // Every field of OrcaState defaults, so keybindings.json parses too.
        let home = tempfile::tempdir().expect("tempdir");
        let data = home.path().join(".orca");
        std::fs::create_dir_all(&data).expect("create");
        std::fs::write(data.join("keybindings.json"), r#"{"cmd+k": "quickOpen"}"#).expect("write");

        let report = scan_orca(&Env::fake(home.path()));

        assert!(report.workspaces.is_empty());
        assert!(
            report
                .evidence
                .iter()
                .all(|e| !e.locator.ends_with("keybindings.json")
                    || e.detail.as_deref() != Some("Orca persisted state")),
            "keybindings must not be reported as state"
        );
    }

    #[test]
    fn orca_honours_its_data_dir_overrides_in_priority_order() {
        let root = tempfile::tempdir().expect("tempdir");
        let explicit = root.path().join("explicit");
        let xdg = root.path().join("xdg");

        let env = Env::fake(root.path())
            .with_var("XDG_DATA_HOME", xdg.to_string_lossy())
            .with_var("ORCA_USER_DATA", explicit.to_string_lossy());
        assert_eq!(orca_user_data(&env), explicit);

        let env = Env::fake(root.path()).with_var("XDG_DATA_HOME", xdg.to_string_lossy());
        assert_eq!(orca_user_data(&env), xdg.join("Orca"));

        assert_eq!(
            orca_user_data(&Env::fake(root.path())),
            root.path().join(".orca")
        );
    }

    #[test]
    fn superset_reports_the_host_manifest_without_reading_its_token() {
        let home = tempfile::tempdir().expect("tempdir");
        let host = home.path().join(".superset").join("host").join("org-1");
        std::fs::create_dir_all(&host).expect("create");
        std::fs::write(
            host.join("manifest.json"),
            r#"{"endpoint":"http://127.0.0.1:47821","token":"SECRET-DO-NOT-LEAK"}"#,
        )
        .expect("write manifest");

        let report = scan_superset(&Env::fake(home.path()));

        let manifest = report
            .evidence
            .iter()
            .find(|e| e.locator.ends_with("manifest.json"))
            .expect("manifest evidence");
        let detail = manifest.detail.as_deref().unwrap_or_default();
        assert!(detail.contains("127.0.0.1:47821"), "got {detail}");
        assert!(
            !detail.contains("SECRET-DO-NOT-LEAK"),
            "the token must never reach a report: {detail}"
        );
    }

    #[test]
    fn superset_lists_project_less_sessions_as_workspaces() {
        let home = tempfile::tempdir().expect("tempdir");
        let sessions = home.path().join(".superset").join("sessions");
        std::fs::create_dir_all(sessions.join("quick-fix")).expect("create");

        let report = scan_superset(&Env::fake(home.path()));

        assert_eq!(report.workspaces.len(), 1);
        assert_eq!(report.workspaces[0].name.as_deref(), Some("quick-fix"));
        assert_eq!(report.workspaces[0].is_worktree, Some(false));
    }

    #[test]
    fn super_engineering_off_macos_is_unsupported_not_absent() {
        let home = tempfile::tempdir().expect("tempdir");
        let report = scan_super_engineering(&Env::fake(home.path()).with_platform(Platform::Linux));
        assert!(
            matches!(report.installed, Installed::Unsupported { .. }),
            "got {:?}",
            report.installed
        );
    }

    #[test]
    fn super_engineering_reads_the_sc_cli_worktree_contract() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");
        std::fs::write(bin.join("sc"), "#!/bin/sh\n").expect("write sc");

        let env = Env::fake(home.path())
            .with_platform(Platform::MacOs)
            .with_path_dirs([bin])
            .with_commands(StubRunner::new().returning(
                "sc",
                &["worktree", "list", "--json"],
                r#"[{"type":"worktree","id":"w1","url":"file:///src/a","primary":true},
                    {"type":"worktree","id":"w2","url":"file:///src/b","primary":false}]"#,
            ));

        let report = scan_super_engineering(&env);

        assert_eq!(report.workspaces.len(), 2);
        assert_eq!(
            report.workspaces[0].is_worktree,
            Some(false),
            "primary is the repo checkout, not a task worktree"
        );
        assert_eq!(report.workspaces[1].is_worktree, Some(true));
        assert_eq!(
            report.workspaces[0].path.as_deref(),
            Some(Path::new("/src/a")),
            "without a path the workspace can never correlate with a session"
        );
    }

    #[test]
    fn file_urls_become_paths_only_when_they_name_a_local_one() {
        assert_eq!(
            path_from_file_url("file:///src/a"),
            Some(PathBuf::from("/src/a"))
        );
        assert_eq!(
            path_from_file_url("file://localhost/src/a"),
            Some(PathBuf::from("/src/a"))
        );
        assert_eq!(
            path_from_file_url("file://server/share"),
            None,
            "a remote host is not a local path"
        );
        assert_eq!(path_from_file_url("https://example.com/x"), None);
        assert_eq!(
            path_from_file_url("file:///a%20b"),
            None,
            "not decoded, so not guessed at"
        );
    }

    #[test]
    fn a_changed_sc_contract_is_reported_rather_than_silently_empty() {
        let mut report = ToolReport::absent(ToolId::SuperEngineering, "super.engineering");
        read_sc_worktrees(r#"{"worktrees": []}"#, &mut report);
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    }
}
