# Agent-session discovery

Purpose: what "find the agent sessions other tools are running on this machine"
reads, how it is built, and the facts and licence reasoning behind it. This
carries forward the `fresh-agent-sessions` crate's README and third-party notes;
the crate is gone and the knowledge is not.

---

## 1. Where it lives now

Discovery is entirely plugin-side. Core knows how to reach a machine and
nothing about what lives on it.

| Piece | File |
| --- | --- |
| The hub — opens the machine, runs every registered scanner, correlates, reports | `plugins/agent_sessions.ts` |
| Shared types and the `registerScanner` helper | `plugins/lib/agent_scanner.ts` |
| tmux | `plugins/agent_sessions_tmux.ts` |
| screen, zellij | `plugins/agent_sessions_multiplexers.ts` |
| Claude Code | `plugins/agent_sessions_claude_code.ts` (+ `lib/claude_code_format.ts`) |
| Codex CLI and Desktop | `plugins/agent_sessions_codex.ts` |
| Herdr | `plugins/agent_sessions_herdr.ts` |
| Orca, Superset, super.engineering | `plugins/agent_sessions_orchestrators.ts` |
| Claude Desktop | `plugins/agent_sessions_claude_desktop.ts` |
| ~19 table-driven agents | `plugins/agent_sessions_catalog.ts` (+ `lib/agent_catalog.ts`) |

The editor-side surface is four generic primitives — `walkTree`,
`readFilePrefixes`, `runOnTarget`, `machineEnv` — bound to a machine handle by
`editor.openMachine(spec)`. A scanner is handed a `FreshMachine` and never has
to think about which computer it is talking to; the same scanner reads a local
home directory and an SSH one.

The Orchestrator's discover dialog is the only caller: it picks a machine that
has an open window and asks the hub to scan through that window's authority.

## 2. What is read

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
| **Claude Desktop** | app-support dir and `claude_desktop_config.json` MCP servers | conversations are server-side; the scanner says so rather than reporting an empty list |
| **~19 more agents** | Gemini, Copilot, Cursor, Grok, Devin, Hermes, Rovo, Pi, OMP, Prime, OpenClaw, Droid, Cline, Kimi, opencode, Amp, Aider, goose, Qwen | declarative table in `lib/agent_catalog.ts` |

An externally-reported inventory of these stores, checked row by row against
the scanners, is in [agent-session-stores.md](agent-session-stores.md) — it is
where the known gaps are written down.

## 3. Findings worth knowing

Three things that only came out of testing against live installs.

**Claude Code's project directory name is lossy, and you should not decode it.**
The bucket under `~/.claude/projects/` is the working directory with every
non-alphanumeric character replaced by *one* dash — runs are not collapsed, so
`/home/user/fresh` becomes `-home-user-fresh`. It is not injective: `~/a-b` and
`~/a/b` encode identically. But every transcript record carries the real `cwd`,
`gitBranch` and `version`, so the scanner reads those and uses the directory
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

## 4. Design

- **Read-only, and it stays that way.** Nothing writes, moves or deletes.
  Running a tool's own listing command is off unless the caller opts in with
  `allowCommands`.
- **Nothing is assumed about the machine.** Home, platform and environment come
  from the `FreshMachine` handle, so the same scanner reads a local install and
  a remote one, and a Linux runner can exercise the macOS-only paths.
- **Every fact carries its provenance.** Each row holds the `Evidence` it came
  from — the path, variable or command that produced it — and links carry a
  `Confidence`.
- **Duplicates are linked, not merged.** One conversation legitimately appears
  as a Claude Code transcript *and* a tmux pane. The hub's `correlate` pass
  emits a `Link` saying so — `recorded-session-id` when the host wrote the
  agent's own id down, `same-cwd` as a hypothesis when all they share is a
  directory. It never rewrites one scanner's row with another's belief.
- **A scan never fails, and never goes quiet.** A missing directory means "not
  installed"; an unreadable one, a state file that will not parse, or a scanner
  that throws lands in `problems` and the scan carries on. The discover dialog
  shows every problem, because a tool skipped for an unreadable store is
  otherwise indistinguishable from a tool that is not installed.
- **One scanner cannot lose the scan.** Each is called inside its own
  `try`/`catch`; a throw becomes a problem against that scanner's id.

## 5. Privacy

These stores hold the user's prompts and their agents' output. A scanner reads
only enough of a transcript to identify it, and never puts transcript *content*
in a row. Files holding credentials (Superset's `config.json` and its host
manifests) are reported as existing and are not read for their secrets.

## 6. Scope

A scan reads one machine — the one behind the window whose authority it was
given. Several of these tools support remote hosts, WSL distros and cloud
runtimes; those sessions live on other machines, and a scan does not pretend
otherwise. Scanning another machine means opening a window on it first.

## 7. Provenance and licensing

The scanners contain no third-party code and vendor nothing. This records what
was consulted while working out the formats, and why that is not a licensing
problem.

### Projects consulted

| Project | Licence | What was used |
| --- | --- | --- |
| [Orca](https://github.com/stablyai/orca) | MIT (© 2026 Lovecast Inc.) | Its `src/main/ai-vault/session-scanner-agent-sources.ts` catalogues ~18 agents' session directories. The **paths, environment-variable names and filename patterns** were cross-checked against it. |
| [Herdr](https://github.com/herdrdev/herdr) | Apache-2.0 | Its session-snapshot handling establishes the **on-disk layout and the JSON field names** of `session.json`. |
| tmux | ISC / BSD | Behaviour observed by running tmux 3.4, not from source. |
| Claude Code, Codex CLI | — | Formats observed from the files they wrote into this machine's own home directory. Codex CLI is itself Apache-2.0. |
| Superset, super.engineering, Claude/Codex Desktop | — | **Public documentation only.** None was installed and no binary was examined. |

### Why this is clean

**Only facts were taken.** A path such as `~/.config/herdr/session.json`, an
environment variable named `CODEX_HOME`, and a filename pattern such as
`rollout-<timestamp>-<uuid>.jsonl` are facts about a data format, not creative
expression, and facts carry no copyright.

**Field names are functional necessity.** The parsers must spell fields exactly
as the format does — `identity_cwd`, `managed_agent_kind`, `worktreeMeta` — or
they do not deserialise. These are interface elements dictated by
interoperability, not choices with expressive alternatives.

**No code or prose was copied.** This was checked mechanically rather than
asserted: every comment and line of the original implementation was compared
against the full comment and source text of both projects by n-gram overlap.

- Against **Herdr** (Apache-2.0): no comment overlap at all. The only matches
  anywhere were boilerplate with no expressive alternative — derive lists,
  `serde` attributes, a `Debug::fmt` signature.
- Against **Orca** (MIT): no overlap in code. Two comments had tracked Orca's
  phrasing more closely than intended and were rewritten; what remains is
  generic English, unavoidable technical noun phrases, literal path patterns,
  and our own citations of Orca's filenames.

The port from Rust to TypeScript carried the same facts and rewrote the prose
again; nothing in the plugins was translated from either project's source.

**Apache-2.0 and the GPL version.** Apache-2.0 is incompatible with GPL-2.0 but
*compatible* with GPL-3.0, so the workspace's licence version decides whether
this combination matters at all:

* **Under GPL-2.0** (where the workspace sits today) it is the one pairing that
  could bite — and it does not arise, because Herdr contributed format facts
  and nothing else.
* **Under GPL-3.0** the question disappears entirely: Apache-2.0 material may
  be combined into a GPLv3 work, so even the boilerplate matches would be moot.

Either way nothing here changes: no Herdr code or text is present or
distributed. The move to GPLv3 only widens the margin.

Note for whoever makes that move: the repository is GPL-2.0 **without** an "or
later" grant (`LICENSE` is the bare v2 text — the "or (at your option) any
later version" line at its end belongs to the *how to apply these terms*
appendix, not to a grant, and no source file carries a version header).
Relicensing to v3 therefore needs the agreement of every copyright holder;
`git shortlog -sne --all` currently lists six. That is a repository-wide
decision and nothing here depends on the outcome.

**Reading the source was permitted.** Both MIT and Apache-2.0 grant the right
to use and study the software. No clean-room separation was needed, and none is
claimed.

**No terms were accepted for the proprietary tools.** Superset and
super.engineering ship as macOS applications that were never obtained or run
here, so no licence agreement or anti-reverse-engineering clause was ever
entered into. Their entries come from published documentation.

### If Orca or Herdr would prefer otherwise

The attribution above is offered because it is accurate and useful, not because
either licence compels it at this scale. If either project would rather not be
cited this way, open an issue and it will be changed.
