//! Print the agent sessions on this machine.
//!
//! A thin caller of the library, and the only thing that knows about argv. It
//! constructs a [`LocalTarget`] and calls `collect_sessions`; a consumer that
//! wants a remote machine supplies its own `Target` instead. Nothing here is
//! required to use the crate.

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use fresh_agent_sessions::{collect_sessions_with, Confidence, LocalTarget, ScanOptions, Status};

const HELP: &str = "\
fresh-agent-sessions — list the agent sessions other tools are running

  --json           machine-readable output
  --commands       allow running tools' own listing commands (tmux, zellij, sc)
  --limit N        cap sessions reported per tool (default 200)
  --machine NAME   name this machine in every session key
  --verbose        also print the per-tool report and any problems";

fn main() -> ExitCode {
    let mut options = ScanOptions::default();
    let mut json = false;
    let mut commands = false;
    let mut verbose = false;
    let mut machine = "local".to_string();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--commands" => commands = true,
            "--verbose" => verbose = true,
            "--all" => options.include_absent = true,
            "--machine" => match args.next() {
                Some(name) => machine = name,
                None => return fail("--machine needs a name"),
            },
            "--limit" => match args.next().and_then(|value| value.parse().ok()) {
                Some(limit) => options.max_sessions_per_tool = limit,
                None => return fail("--limit needs a number"),
            },
            "-h" | "--help" => {
                println!("{HELP}");
                return ExitCode::SUCCESS;
            }
            other => return fail(&format!("unknown argument: {other}\n\n{HELP}")),
        }
    }

    let target = LocalTarget::from_process().with_name(machine);
    let target = if commands {
        target.with_commands()
    } else {
        target
    };

    // Nothing cancels a one-shot CLI run; a UI would hold this and flip it.
    let cancel = AtomicBool::new(false);
    let result = collect_sessions_with(&target, &cancel, &options);

    if json {
        match serde_json::to_string_pretty(&result) {
            Ok(text) => println!("{text}"),
            Err(err) => return fail(&format!("could not serialize result: {err}")),
        }
        return ExitCode::SUCCESS;
    }

    if let Status::Unreachable { error } = &result.status {
        eprintln!("{}: unreachable — {error}", result.machine);
        return ExitCode::FAILURE;
    }

    for session in &result.sessions {
        let marker = match session.confidence {
            Confidence::Certain => ' ',
            Confidence::Probable => '~',
            Confidence::Possible => '?',
        };
        let owner = match session.owner.pane.as_deref() {
            Some(pane) => format!("{}:{pane}", session.owner.display_name),
            None => session.owner.display_name.clone(),
        };
        println!(
            "{marker}{:<10} {:<9} {:<26} {}",
            session.key.machine.as_str(),
            session.agent.map(|agent| agent.as_str()).unwrap_or("-"),
            owner,
            session.display_title()
        );
    }

    if result.sessions.is_empty() {
        println!("No agent sessions found on {}.", result.machine);
    }

    if verbose {
        for tool in result.discovery.present_tools() {
            println!("\n{}", tool.display_name);
            for evidence in &tool.evidence {
                println!("  {:?} {}", evidence.kind, evidence.locator);
            }
        }
        for problem in &result.problems {
            println!("  ! {problem}");
        }
    }

    if !commands {
        println!(
            "\nNote: live multiplexer state was not queried. Re-run with --commands to ask \
             tmux/zellij for their current sessions."
        );
    }
    ExitCode::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}
