# fresh-agent-sessions

Read-only discovery of the agent sessions, workspaces and worktrees that
*other* tools are managing on this machine.

A developer's machine now routinely runs several coding agents at once, each
under a different host: a Claude Code session in a tmux pane, two Codex
sessions in Orca worktrees, a Herdr workspace with four agents in it. Every one
of those hosts knows about its own sessions and nothing about anyone else's.
This crate reads all of their state and reports one list.

```console
$ fresh-agent-sessions --commands

Claude Code
   [claude] 3cc1b4b4-ea2d-5dac-9a45-aa08481b6b8f — /home/user/fresh

Codex CLI (codex-cli 0.154.0)

tmux (tmux 3.4)
  [session] work — 1 pane(s), detached
      claude in /home/user/fresh

Cross-tool links (1):
  tmux/$0:%0 hosts claude-code/3cc1b4b4-… (SameCwdAndAgent)
```

## What it covers

| Tool | What is read | How it was worked out |
| --- | --- | --- |
| **tmux** | sockets in `$TMUX_TMPDIR/tmux-<uid>/`; live sessions and panes via `list-sessions` / `list-panes` | verified against tmux 3.4 |
| **screen**, **zellij** | socket / runtime directories; `zellij list-sessions` | documented layouts |
| **Herdr** | `~/.config/herdr[-dev]/session.json`, `sessions/<name>/`, `herdr.sock` | layout and field names checked against Herdr's session snapshot |
| **Orca** | `$ORCA_USER_DATA` / `$XDG_DATA_HOME/Orca` / `~/.orca`, profile state `worktreeMeta`, agent hooks | paths checked against Orca's `PersistedState` and `orcad-app-paths.ts` |
| **Superset** | `~/.superset/{config.json,sessions/,host/<org>/manifest.json}` | Superset CLI docs |
| **super.engineering** | macOS app support + `sc worktree list --json` | vendor docs; macOS-only, reported as *unsupported* elsewhere |
| **Claude Code** | `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`, sub-agent transcripts, `~/.claude.json` | verified against Claude Code 2.1.273 |
| **Codex CLI / Desktop** | `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`, `session_index.jsonl`, `state_5.sqlite` | verified against Codex CLI 0.154.0 |
| **Claude Desktop** | app-support dir and `claude_desktop_config.json` MCP servers | conversations are server-side; the crate says so rather than reporting an empty list |
| **~19 more agents** | Gemini, Copilot, Cursor, Grok, Devin, Hermes, Rovo, Pi, OMP, Prime, OpenClaw, Droid, Cline, Kimi, opencode, Amp, Aider, goose, Qwen | declarative table in `catalog.rs` |

## Findings worth knowing

Three things that only came out of testing against live installs:

**Claude Code's project directory name is lossy, and you should not decode it.**
The bucket under `~/.claude/projects/` is the working directory with every
non-alphanumeric character replaced by *one* dash — runs are not collapsed, so
`/home/user/fresh` becomes `-home-user-fresh`. It is not injective: `~/a-b` and
`~/a/b` encode identically. But every transcript record carries the real `cwd`,
`gitBranch` and `version`, so this crate reads those and uses the directory
name only as a last-resort label.

**tmux escapes its own format output.** A `\x1f` delimiter comes back as the
four characters `\037`, and a literal backslash in a window name comes back
doubled. The first version of this provider returned zero sessions against a
live server while its tests passed. The escaping is reversible, but only with a
scanner that consumes escapes atomically — a title whose text is `\037` arrives
as `\\037`, where a substring split finds a false separator one character in.

**Orca's own doc comment disagrees with its code** about Windows drive letters
(it claims `C:\` encodes to `c--`; the function never lowercases, and the
comment above it explains why it must not). The code is the authority here.

## Design

- **Read-only, and it stays that way.** Nothing writes, moves or deletes.
  Running a tool's own listing command is off unless the caller opts in with
  `Env::with_commands`.
- **Nothing is assumed about the machine.** Home, environment, platform, uid
  and `PATH` all come from `Env`, so tests describe an install in a `TempDir`
  and a Linux runner can exercise the macOS-only paths.
- **Every fact carries its provenance.** Each record holds the `Evidence` it
  came from and a `Confidence`. A `cwd` the agent wrote into its own transcript
  is `Certain`; one inferred from a pane's foreground command is `Possible`.
- **Duplicates are linked, not merged.** One conversation legitimately appears
  as a Claude Code transcript *and* a Herdr pane. `Discovery::correlate` emits a
  `Link` saying so — exactly when the host recorded the agent's session id, as a
  hypothesis when all they share is a directory. It never rewrites one
  provider's record with another's belief.
- **A scan never fails.** A missing directory means "not installed"; an
  unreadable one, or a state file that will not parse, lands in
  `ToolReport::problems` and the scan carries on.

## Licensing

GPL-2.0, like the rest of the workspace, and it adds no dependencies. No
third-party code is vendored or copied: the formats were worked out from facts
(paths, environment variables, filename patterns, JSON field names), with
Orca (MIT) and Herdr (Apache-2.0) consulted as references. Apache-2.0 is
incompatible with GPL-2.0, so that one was checked mechanically — see
[THIRD-PARTY-NOTES.md](THIRD-PARTY-NOTES.md).

## Privacy

These stores hold the user's prompts and their agents' output. This crate reads
only enough of a transcript to identify it and never puts transcript *content*
in a report. Files holding credentials (Superset's `config.json` and its host
manifests) are reported as existing and are not read for their secrets.

## Scope

Local state only. Several of these tools support remote hosts, WSL distros and
cloud runtimes; their sessions live on those machines and a scan does not
pretend otherwise.

## Usage

```rust
use fresh_agent_sessions::{scan, Env};

let discovery = scan(&Env::from_process());
for tool in discovery.present_tools() {
    println!("{}: {} session(s)", tool.display_name, tool.agent_sessions.len());
}
```

```console
fresh-agent-sessions [--json] [--all] [--commands] [--no-correlate] [--limit N]
```
