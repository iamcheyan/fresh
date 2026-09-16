//! The Claude desktop application.
//!
//! # What it does and does not hold
//!
//! Claude Desktop is not a coding-agent session store: conversations live
//! server-side, and what it keeps locally is application state — MCP server
//! configuration, an Electron cache, logs. So this provider reports
//! *presence and configuration*, not sessions, and says so rather than
//! returning an empty list that reads like "no conversations".
//!
//! The one genuinely useful artefact is `claude_desktop_config.json`, whose
//! `mcpServers` map names every MCP server the app launches. Those are
//! long-running child processes of a session-multiplexing tool, which puts
//! them squarely in this crate's remit.
//!
//! # Paths
//!
//! ```text
//! macOS   ~/Library/Application Support/Claude/claude_desktop_config.json
//! Linux   ~/.config/Claude/claude_desktop_config.json
//! Windows %APPDATA%/Claude/claude_desktop_config.json
//! ```

use std::path::PathBuf;

use crate::machine::Machine;
use crate::model::{Evidence, Installed, ToolId, ToolReport};
use crate::target::Platform;

/// The application-support directory for `app`, per platform.
pub fn app_support_dir(env: &Machine<'_>, app: &str) -> PathBuf {
    match env.platform() {
        Platform::MacOs => env.home_join(["Library", "Application Support", app]),
        Platform::Windows => env
            .var("APPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| env.home_join(["AppData", "Roaming"]))
            .join(app),
        _ => env
            .var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| env.home_join([".config"]))
            .join(app),
    }
}

pub fn scan(env: &Machine<'_>) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::ClaudeDesktop, "Claude Desktop");
    let dir = app_support_dir(env, "Claude");

    if !env.is_dir(&dir) {
        return report;
    }
    report
        .evidence
        .push(Evidence::dir(&dir).saying("Claude Desktop application support directory"));
    report.installed = Installed::Present {
        version: None,
        executable: None,
    };

    let config = dir.join("claude_desktop_config.json");
    if !env.is_file(&config) {
        return report;
    }

    let servers = env
        .read_prefix_io(&config, 1024 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes.as_slice()).ok())
        .and_then(|value| {
            value
                .get("mcpServers")
                .and_then(serde_json::Value::as_object)
                .map(|map| map.keys().cloned().collect::<Vec<_>>())
        })
        .unwrap_or_default();

    report
        .evidence
        .push(Evidence::file(&config).saying(if servers.is_empty() {
            "Claude Desktop configuration; no MCP servers configured".to_string()
        } else {
            format!(
                "Claude Desktop configuration; launches {} MCP server(s): {}",
                servers.len(),
                servers.join(", ")
            )
        }));

    report.note_problem(
        "Claude Desktop keeps conversations server-side; no local session transcripts exist to list"
            .to_string(),
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalTarget;

    #[test]
    fn names_the_mcp_servers_the_app_launches() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join(".config").join("Claude");
        std::fs::create_dir_all(&dir).expect("create");
        std::fs::write(
            dir.join("claude_desktop_config.json"),
            r#"{"mcpServers":{"github":{"command":"npx"},"fs":{"command":"uvx"}}}"#,
        )
        .expect("write config");

        let report = scan(&Machine::over(
            LocalTarget::at(home.path()).with_platform(Platform::Linux),
        ));

        let detail = report
            .evidence
            .iter()
            .find(|e| e.locator.ends_with("claude_desktop_config.json"))
            .and_then(|e| e.detail.clone())
            .unwrap_or_default();
        assert!(detail.contains("github"), "got {detail}");
        assert!(detail.contains("fs"), "got {detail}");
    }

    #[test]
    fn says_why_there_are_no_sessions_rather_than_looking_empty() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join(".config").join("Claude");
        std::fs::create_dir_all(&dir).expect("create");
        std::fs::write(dir.join("claude_desktop_config.json"), "{}").expect("write");

        let report = scan(&Machine::over(
            LocalTarget::at(home.path()).with_platform(Platform::Linux),
        ));

        assert!(report.agent_sessions.is_empty());
        assert!(
            report.problems.iter().any(|p| p.contains("server-side")),
            "the empty list needs an explanation: {:?}",
            report.problems
        );
    }

    #[test]
    fn app_support_dir_differs_per_platform() {
        let home = tempfile::tempdir().expect("tempdir");
        let mac = app_support_dir(
            &Machine::over(LocalTarget::at(home.path()).with_platform(Platform::MacOs)),
            "Claude",
        );
        assert!(
            mac.ends_with("Library/Application Support/Claude"),
            "{mac:?}"
        );

        let linux = app_support_dir(
            &Machine::over(LocalTarget::at(home.path()).with_platform(Platform::Linux)),
            "Claude",
        );
        assert!(linux.ends_with(".config/Claude"), "{linux:?}");
    }
}
