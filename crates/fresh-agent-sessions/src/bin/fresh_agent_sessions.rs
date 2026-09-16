//! Print what other tools are running on this machine.
//!
//! ```text
//! fresh-agent-sessions [--json] [--all] [--commands] [--limit N]
//! ```
//!
//! `--commands` lets providers run their tools' own listing commands (`tmux
//! list-sessions`, `zellij list-sessions`, `sc worktree list --json`). Without
//! it the scan is pure filesystem reads, which is why it is not the default:
//! a discovery pass should not fork processes unless asked.

use std::process::ExitCode;

use fresh_agent_sessions::{scan_with, Confidence, Env, Installed, ScanOptions};

fn main() -> ExitCode {
    let mut options = ScanOptions::default();
    let mut json = false;
    let mut commands = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--all" => options.include_absent = true,
            "--commands" => commands = true,
            "--no-correlate" => options.correlate = false,
            "--limit" => match args.next().and_then(|value| value.parse().ok()) {
                Some(limit) => options.max_sessions_per_tool = limit,
                None => {
                    eprintln!("--limit needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "-h" | "--help" => {
                println!("{}", HELP);
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{HELP}");
                return ExitCode::FAILURE;
            }
        }
    }

    let env = Env::from_process();
    let env = if commands {
        env.with_process_commands()
    } else {
        env
    };
    let discovery = scan_with(&env, &options);

    if json {
        match serde_json::to_string_pretty(&discovery) {
            Ok(text) => println!("{text}"),
            Err(err) => {
                eprintln!("could not serialize discovery: {err}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    print_human(&discovery, commands);
    ExitCode::SUCCESS
}

const HELP: &str = "\
fresh-agent-sessions — find agent sessions, workspaces and worktrees that other tools manage

  --json           machine-readable output
  --all            include tools that left nothing behind
  --commands       allow running tools' own listing commands (tmux, zellij, sc)
  --no-correlate   skip cross-tool linking
  --limit N        cap sessions reported per tool (default 200)";

fn print_human(discovery: &fresh_agent_sessions::Discovery, commands: bool) {
    let reports: Vec<_> = discovery.tools.iter().collect();
    if reports.is_empty() {
        println!("No third-party session-managing tools found.");
        return;
    }

    for report in reports {
        let status = match &report.installed {
            Installed::Present {
                version: Some(v), ..
            } => format!(" ({v})"),
            Installed::Present { .. } => String::new(),
            Installed::Absent => " — not installed".to_string(),
            Installed::Unsupported { reason } => format!(" — {reason}"),
        };
        println!("\n{}{}", report.display_name, status);

        for workspace in &report.workspaces {
            let path = workspace
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "?".to_string());
            let kind = match workspace.is_worktree {
                Some(true) => "worktree",
                Some(false) => "folder",
                None => "workspace",
            };
            println!(
                "  [{kind}] {} — {path}{}",
                workspace.name.as_deref().unwrap_or(&workspace.id),
                workspace
                    .branch
                    .as_deref()
                    .map(|b| format!(" ({b})"))
                    .unwrap_or_default()
            );
        }

        for mux in &report.mux_sessions {
            println!(
                "  [session] {} — {} pane(s){}",
                mux.name.as_deref().unwrap_or(&mux.id),
                mux.panes.len(),
                match mux.attached {
                    Some(true) => ", attached",
                    Some(false) => ", detached",
                    None => "",
                }
            );
            for pane in mux.panes.iter().filter(|pane| pane.agent.is_some()) {
                println!(
                    "      {} in {}",
                    pane.agent.map(|a| a.as_str()).unwrap_or("?"),
                    pane.cwd
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "?".to_string())
                );
            }
        }

        for session in &report.agent_sessions {
            let marker = match session.confidence {
                Confidence::Certain => ' ',
                Confidence::Probable => '~',
                Confidence::Possible => '?',
            };
            println!(
                "  {marker}[{}] {} — {}{}",
                session.agent.as_str(),
                session.id,
                session
                    .cwd
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "unknown cwd".to_string()),
                session
                    .title
                    .as_deref()
                    .map(|t| format!(" — {t}"))
                    .unwrap_or_default()
            );
        }

        for problem in &report.problems {
            println!("  ! {problem}");
        }
    }

    if !discovery.links.is_empty() {
        println!("\nCross-tool links ({}):", discovery.links.len());
        for link in &discovery.links {
            println!("  {} hosts {} ({:?})", link.host, link.hosted, link.reason);
        }
    }

    if !commands {
        println!(
            "\nNote: live multiplexer state was not queried. Re-run with --commands to ask \
             tmux/zellij/sc for their current sessions."
        );
    }
}
