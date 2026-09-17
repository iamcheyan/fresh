# Third-party agent stores: a reported inventory vs. our scanners

Purpose: record an externally-published inventory of where coding agents keep
their state, checked row by row against what the `agent-sessions` scanner
plugins actually look at, so the gaps are written down rather than rediscovered.

**Provenance.** The inventory below was posted publicly by a third party and
shared with us; it is one person's `du` output on one machine. It has not been
verified against any of these tools' source or docs. Treat it as a lead worth
checking, not as a specification — where it disagrees with a scanner, the
disagreement is a question, not a bug report. Sizes are theirs and are included
only because they say something about which stores matter in practice.

---

## 1. The reported inventory

| Store | Where it is reported to be | Reported size |
| --- | --- | --- |
| Pi sessions | `~/.pi/agent/sessions/--<cwd>--/` | 758M |
| Codex sessions | `~/.codex/sessions` + `archived_sessions` | 1.9G |
| Claude transcripts | `~/.claude/projects/<encoded-path>/` | 105M |
| Cursor | `~/.cursor/projects/` + App Support SQLite | 33M |
| Grok | `~/.grok/sessions/<urlencoded-cwd>/` | 564K |
| Codex worktrees | `~/.codex/worktrees` | 11G |
| Herdr worktrees | `~/.herdr/worktrees` | 9.3G |
| Claude worktrees | in-repo: `<repo>/.claude/worktrees/` | — |
| Cursor worktrees | `~/.cursor/worktrees` | — |

## 2. Row by row against our scanners

| Row | Our coverage | Where |
| --- | --- | --- |
| Pi sessions | covered | `lib/agent_catalog.ts`, row `pi` — `.pi/agent/sessions`, relocated by `PI_CODING_AGENT_DIR` |
| Codex sessions | **partial** | `agent_sessions_codex.ts` walks `$CODEX_HOME/sessions` only |
| Claude transcripts | covered | `agent_sessions_claude_code.ts` + `lib/claude_code_format.ts`, which also decodes the bucket name |
| Cursor | **partial** | catalog row `cursor` covers `.cursor/projects` (files under an `agent-transcripts` component); the SQLite side is not read |
| Grok | covered | catalog row `grok` — `.grok/sessions`, `summary.json`, relocated by `GROK_SESSIONS_DIR` |
| Codex worktrees | **not covered** | — |
| Herdr worktrees | **not covered** | `agent_sessions_herdr.ts` reads a different root (see §3.3) |
| Claude worktrees | **not covered** | — |
| Cursor worktrees | **not covered** | — |

Five of nine rows land somewhere a scanner already looks. The misses are not
scattered: four of them are the same miss.

## 3. What the comparison actually turned up

### 3.1 Worktrees are a category we do not scan at all

Every uncovered row is a worktree directory, and they are the two largest
entries in the inventory by an order of magnitude — 11G and 9.3G against 1.9G
for the biggest session store. Our scanners are transcript-oriented: they answer
"what sessions exist", and a worktree is not a session.

`agent_sessions_orchestrators.ts` is the one partial exception — it reports Orca
worktrees, but it gets them from Orca's own JSON state (`worktreeMeta`), not by
walking a worktree root. Nothing generalises from that to `~/.codex/worktrees`.

Whether this is a gap depends on a product question we have not answered: is a
checked-out worktree with no live agent a *session* to surface in discover? If
yes, it is a new scanner shape (walk a root of git checkouts, correlate each
back to its repo) rather than another catalog row. The `<repo>/.claude/worktrees/`
row makes that harder still: it is the only reported store that is not under
`$HOME`, so a `$HOME`-rooted walk can never find it — it would have to be
discovered from the repo side.

### 3.2 `archived_sessions` — the one straightforward miss

`agent_sessions_codex.ts` resolves `$CODEX_HOME` (default `~/.codex`) and walks
`sessions` under it. If Codex also keeps `archived_sessions`, we silently under-
report: the sessions are there, we are simply not looking. This is the cheapest
item here — one more walk root in an existing scanner — but it should be checked
against Codex itself before being added, because an "archived" session may well
be one discover *should* leave out.

### 3.3 Herdr's root does not match

We resolve Herdr under `$XDG_CONFIG_HOME/herdr`, else `~/.config/herdr` (with a
`herdr-dev` sibling for debug builds). The inventory reports `~/.herdr/worktrees`.
Both can be true — a config directory and a data directory are different things,
and worktrees belong in the latter. Worth confirming; if Herdr really does keep
state in `~/.herdr`, our scanner is looking in the right place for sessions and
the wrong place for everything else.

### 3.4 Path-encoded bucket names are an unused fallback

Three stores encode the working directory into the *directory name*: Pi
(`--<cwd>--`), Grok (url-encoded), and Claude Code (`<encoded-path>`). We decode
that only for Claude Code. For Pi and Grok the catalog reads `cwd` out of the
transcript's own fields and ignores the bucket name.

That is fine while the field is present, and wrong the moment it is not: a
transcript without a `cwd` yields a session with no working directory, even
though the directory containing it spells the cwd out. Since correlation and the
blank-title fallback both key off `cwd`, losing it degrades more than one thing.
A `cwdFromBucket` encoding on `AgentStore` would close this, reusing the decoder
that already exists for Claude Code.

### 3.5 Cursor keeps state in two places

We read `.cursor/projects`. The inventory also lists an Application Support
SQLite database. We have app-support machinery already
(`agent_sessions_claude_desktop.ts` resolves per-platform application-support
directories) and a precedent for opaque database stores (the `opencode` catalog
row reports `.db`/`.sqlite` files as evidence and claims no sessions from them).
So the shape exists if we want it; what we do not have is any reason yet to
believe the SQLite side holds sessions the `projects` directory does not.

## 4. Out of scope

The inventory is framed around disk usage — it is someone working out what to
delete. Reclaiming space is not what discover is for, and nothing here argues we
should grow into it. The sizes matter to us only as evidence of which stores see
real use.
