# Third-party notes

`fresh-agent-sessions` is GPL-2.0, like the rest of this workspace. It contains
no third-party code and vendors nothing. This file records what was consulted
while working out the storage formats it reads, and why that is not a licensing
problem.

## Dependencies

None added. The crate uses `serde`, `serde_json` and `tracing` (plus `tempfile`
for tests), all already in this workspace. Adding it changed the dependency
graph by exactly one entry — itself — so the licence set `deny.toml` governs is
unaffected.

## Projects consulted

| Project | Licence | What was used |
| --- | --- | --- |
| [Orca](https://github.com/stablyai/orca) | MIT (© 2026 Lovecast Inc.) | Its `src/main/ai-vault/session-scanner-agent-sources.ts` catalogues ~18 agents' session directories. The **paths, environment-variable names and filename patterns** were cross-checked against it. |
| [Herdr](https://github.com/herdrdev/herdr) | Apache-2.0 | Its session-snapshot handling establishes the **on-disk layout and the JSON field names** of `session.json`. |
| tmux | ISC / BSD | Behaviour observed by running tmux 3.4, not from source. |
| Claude Code, Codex CLI | — | Formats observed from the files they wrote into this machine's own home directory. Codex CLI is itself Apache-2.0. |
| Superset, super.engineering, Claude/Codex Desktop | — | **Public documentation only.** None was installed and no binary was examined. |

## Why this is clean

**Only facts were taken.** A path such as `~/.config/herdr/session.json`, an
environment variable named `CODEX_HOME`, and a filename pattern such as
`rollout-<timestamp>-<uuid>.jsonl` are facts about a data format, not creative
expression, and facts carry no copyright.

**Field names are functional necessity.** The `serde` structs in
`providers/herdr.rs` and `providers/orchestrators.rs` must spell fields exactly
as the format does — `identity_cwd`, `managed_agent_kind`, `worktreeMeta` — or
they do not deserialise. These are interface elements dictated by
interoperability, not choices with expressive alternatives.

**No code or prose was copied.** This was checked mechanically rather than
asserted: every comment and line of this crate was compared against the full
comment and source text of both projects by n-gram overlap.

- Against **Herdr** (Apache-2.0): no comment overlap at all. The only matches
  anywhere were Rust and `serde` boilerplate with no expressive alternative —
  `#[derive(Debug, Clone, Copy, PartialEq, Eq, …)]`,
  `#[serde(default, skip_serializing_if = "Option::is_none")]`, the
  `Debug::fmt` signature.
- Against **Orca** (MIT): no overlap in code. Two comments had tracked Orca's
  phrasing more closely than intended and were rewritten; what remains is
  generic English ("that has nothing to do with the"), unavoidable technical
  noun phrases ("the agent's own session id"), literal path patterns, and this
  crate's own citations of Orca's filenames.

**Apache-2.0 and the GPL version.** Apache-2.0 is incompatible with GPL-2.0
but *compatible* with GPL-3.0, so the workspace's licence version decides
whether this combination matters at all:

* **Under GPL-2.0** (where the workspace sits today) it is the one pairing that
  could bite — and it does not arise, because Herdr contributed format facts
  and nothing else.
* **Under GPL-3.0** the question disappears entirely: Apache-2.0 material may
  be combined into a GPLv3 work, so even the boilerplate matches would be
  moot, and Herdr's status stops being a constraint on this crate at all.

Either way nothing here changes: no Herdr code or text is present or
distributed. The move to GPLv3 only widens the margin.

Note for whoever makes that move: the repository is GPL-2.0 **without** an "or
later" grant (`LICENSE` is the bare v2 text — the "or (at your option) any
later version" line at its end belongs to the *how to apply these terms*
appendix, not to a grant, and no source file carries a version header).
Relicensing to v3 therefore needs the agreement of every copyright holder;
`git shortlog -sne --all` currently lists six. That is a repository-wide
decision and nothing in this crate depends on the outcome.

**Reading the source was permitted.** Both MIT and Apache-2.0 grant the right
to use and study the software. No clean-room separation was needed, and none is
claimed.

**No terms were accepted for the proprietary tools.** Superset and
super.engineering ship as macOS applications that were never obtained or run
here, so no licence agreement or anti-reverse-engineering clause was ever
entered into. Their entries come from published documentation.

## If Orca or Herdr would prefer otherwise

The attribution above is offered because it is accurate and useful, not because
either licence compels it at this scale. If either project would rather not be
cited this way, open an issue and it will be changed.
