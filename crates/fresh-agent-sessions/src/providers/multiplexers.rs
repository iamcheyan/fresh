//! GNU screen and zellij.
//!
//! Neither records anything about agents, and both keep their session model in
//! a running server, so discovery here is the same two-tier shape as tmux:
//! sockets from the filesystem, real sessions only when commands are enabled.
//!
//! * **screen** puts its sockets in `$SCREENDIR`, else `/var/run/screen/S-<user>`
//!   or `/tmp/screens/S-<user>`. The socket name is `<pid>.<tty>.<host>`, which
//!   is where the session's pid comes from.
//! * **zellij** keeps per-session directories under
//!   `$XDG_RUNTIME_DIR/zellij/<version>/<session>` (falling back to
//!   `/tmp/zellij-<uid>`), and `zellij list-sessions --no-formatting` prints
//!   them with an `(current)`/`(EXITED …)` suffix.

use std::path::PathBuf;

use crate::machine::Machine;
use crate::model::{Confidence, Evidence, Installed, MuxSession, ToolId, ToolReport};

pub fn scan_screen(env: &Machine<'_>) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::Screen, "GNU screen");

    if let Some(path) = env.which("screen") {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: None,
            executable: Some(path),
        };
    }

    let user = env
        .var("USER")
        .or_else(|| env.var("LOGNAME"))
        .unwrap_or_default();
    let mut dirs = Vec::new();
    if let Some(explicit) = env.var("SCREENDIR").map(PathBuf::from) {
        if explicit.is_absolute() {
            report
                .evidence
                .push(Evidence::env_var("SCREENDIR", explicit.to_string_lossy()));
            dirs.push(explicit);
        }
    }
    if !user.is_empty() {
        dirs.push(PathBuf::from(format!("/var/run/screen/S-{user}")));
        dirs.push(PathBuf::from(format!("/tmp/screens/S-{user}")));
    }

    for dir in dirs {
        let Ok(entries) = env.list_io(&dir) else {
            continue;
        };
        report
            .evidence
            .push(Evidence::dir(&dir).saying("screen socket directory"));
        if matches!(report.installed, Installed::Absent) {
            report.installed = Installed::Present {
                version: None,
                executable: None,
            };
        }
        for socket in entries.into_iter().map(|entry| entry.path) {
            let name = socket
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            report.mux_sessions.push(MuxSession {
                id: name.clone(),
                // `<pid>.<tty>.<host>` — the part after the first dot is the
                // name a user would recognise.
                name: name.split_once('.').map(|(_, rest)| rest.to_string()),
                created_unix: env.modified_unix(&socket),
                attached: None,
                socket: Some(socket.clone()),
                panes: Vec::new(),
                confidence: Confidence::Probable,
                evidence: vec![Evidence::socket(&socket).saying("screen session socket")],
            });
        }
    }

    report
}

pub fn scan_zellij(env: &Machine<'_>) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::Zellij, "zellij");

    if let Some(path) = env.which("zellij") {
        report.evidence.push(Evidence::executable(&path));
        report.installed = Installed::Present {
            version: None,
            executable: Some(path),
        };
    }

    let runtime = env
        .var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("zellij"))
        // No uid means no `/tmp/zellij-<uid>` to guess at; the command path
        // below still works, and inventing a uid would point at someone else's
        // directory.
        .or_else(|| {
            env.uid()
                .map(|uid| PathBuf::from(format!("/tmp/zellij-{uid}")))
        })
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    if env.is_dir(&runtime) {
        report
            .evidence
            .push(Evidence::dir(&runtime).saying("zellij runtime directory"));
        if matches!(report.installed, Installed::Absent) {
            report.installed = Installed::Present {
                version: None,
                executable: None,
            };
        }
    }

    if env.which("zellij").is_some() {
        match env.run("zellij", &["list-sessions", "--no-formatting"]) {
            Ok(output) => {
                for line in output.lines().filter(|line| !line.trim().is_empty()) {
                    // `<name> [Created ...] (current)` or `(EXITED - ...)` —
                    // the name is the first whitespace-delimited token.
                    let name = line.split_whitespace().next().unwrap_or(line).to_string();
                    // Three states, not two. `(current)` is the session this
                    // very command ran inside; `(EXITED …)` is dead. Everything
                    // else is alive but says nothing about whether a client is
                    // attached, and reporting "not exited" as "attached" turns
                    // every running detached session into a false positive.
                    let attached = if line.contains("EXITED") {
                        Some(false)
                    } else if line.contains("(current)") {
                        Some(true)
                    } else {
                        None
                    };
                    report.mux_sessions.push(MuxSession {
                        id: name.clone(),
                        name: Some(name),
                        created_unix: None,
                        attached,
                        socket: None,
                        panes: Vec::new(),
                        confidence: Confidence::Certain,
                        evidence: vec![Evidence::command("zellij list-sessions --no-formatting")
                            .saying(line.trim().to_string())],
                    });
                }
            }
            Err(err) => report.note_problem(format!("zellij list-sessions: {err}")),
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalTarget;
    use crate::target::MapTarget;

    #[test]
    fn zellij_distinguishes_current_from_merely_running() {
        let target = MapTarget::new("m", "/home/remote").with_command(
            "zellij",
            &["list-sessions", "--no-formatting"],
            "alpha [Created 1h ago] (current)\n\
             beta [Created 2h ago]\n\
             gamma [Created 3h ago] (EXITED - attach to resurrect)\n",
        );
        let env = Machine::over(target);

        let report = scan_zellij(&env);

        assert_eq!(report.mux_sessions.len(), 3);
        assert_eq!(report.mux_sessions[0].attached, Some(true), "(current)");
        assert_eq!(
            report.mux_sessions[1].attached, None,
            "running, but zellij does not say whether a client is attached"
        );
        assert_eq!(report.mux_sessions[2].attached, Some(false), "EXITED");
    }

    #[test]
    fn screen_splits_the_pid_off_the_socket_name() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("screens");
        std::fs::create_dir_all(&dir).expect("create");
        std::fs::write(dir.join("4242.pts-3.buildbox"), b"").expect("write socket stand-in");

        let env = Machine::over(
            LocalTarget::at(home.path()).with_var("SCREENDIR", dir.to_string_lossy()),
        );
        let report = scan_screen(&env);

        assert_eq!(report.mux_sessions.len(), 1);
        assert_eq!(report.mux_sessions[0].id, "4242.pts-3.buildbox");
        assert_eq!(
            report.mux_sessions[0].name.as_deref(),
            Some("pts-3.buildbox")
        );
    }
}
