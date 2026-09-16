/// <reference path="./lib/fresh.d.ts" />

/**
 * Herdr — a terminal multiplexer that knows what an agent is.
 *
 * # Layout
 *
 * Herdr resolves its config directory as `$XDG_CONFIG_HOME/herdr` when that
 * variable is set, else `$HOME/.config/herdr`. A debug build uses `herdr-dev`
 * instead, so both are checked. Inside:
 *
 *     <config>/session.json            # the default session's snapshot
 *     <config>/herdr.sock              # JSON API socket
 *     <config>/sessions/<name>/…       # one directory per named session
 *
 * The default session's data *is* the config directory — not
 * `sessions/default` — which is the one thing about this layout a reasonable
 * guess gets wrong.
 *
 * # Why Herdr is the best source here
 *
 * Every other multiplexer leaves you inferring which pane holds an agent from
 * its command name. Herdr writes it down. Each pane may carry `agent_name`,
 * `managed_agent_kind` and an `agent_session`:
 *
 *     {"source":"claude","agent":"claude","kind":"id",
 *      "value":"3cc1b4b4-ea2d-5dac-9a45-aa08481b6b8f"}
 *
 * `kind` is `id` or `path` — the agent's own session id, or a path to its
 * transcript. Either links a pane to a Claude Code row *exactly*, with no cwd
 * heuristics, which is what the hub reports as a recorded-session-id link.
 *
 * # Schema drift
 *
 * `session.json` carries a `version` and Herdr has already migrated its
 * workspace shape once: a legacy snapshot put panes directly on the workspace,
 * the current one nests them under `tabs`. This reader handles both and treats
 * every field as optional, because a snapshot written by a newer Herdr than
 * this was built against is the normal case, not the exception.
 */

import {
  isAbsoluteOn,
  joinPath,
  registerScanner,
  type CollectedSession,
  type ScannerReport,
} from "./lib/agent_scanner.ts";

const editor = getEditor();

/** Snapshots can hold a lot of pane scrollback metadata; the structure this
 *  reads is at the front. */
const SNAPSHOT_BYTES = 512 * 1024;

interface PaneAgentSession {
  source?: string;
  agent?: string;
  kind?: string;
  value?: string;
}

interface PaneSnapshot {
  cwd?: string;
  agent_name?: string;
  managed_agent_kind?: string;
  agent_session?: PaneAgentSession;
}

interface TabSnapshot {
  panes?: Record<string, PaneSnapshot>;
}

interface WorkspaceSnapshot {
  id?: string;
  name?: string;
  identity_cwd?: string;
  tabs?: TabSnapshot[];
  /** The legacy shape, before panes moved under `tabs`. */
  panes?: Record<string, PaneSnapshot>;
}

interface Snapshot {
  version?: number;
  workspaces?: WorkspaceSnapshot[];
}

/** Panes in id order, from both the current and the legacy shape. */
function panesOf(workspace: WorkspaceSnapshot): { id: string; pane: PaneSnapshot }[] {
  const out: { id: string; pane: PaneSnapshot }[] = [];
  const collect = (panes: Record<string, PaneSnapshot> | undefined) => {
    for (const id of Object.keys(panes ?? {}).sort()) {
      const pane = panes?.[id];
      if (pane) out.push({ id, pane });
    }
  };
  for (const tab of workspace.tabs ?? []) collect(tab.panes);
  collect(workspace.panes);
  return out;
}

/** The config directories to check, release build first, each with the label
 *  that keeps its sessions distinct from the other's. */
async function configDirs(machine: FreshMachine): Promise<{ dir: string; variant: string }[]> {
  const env = await machine.env(["XDG_CONFIG_HOME"]);
  const xdg = env["XDG_CONFIG_HOME"];
  const base =
    xdg && isAbsoluteOn(machine, xdg) ? xdg : joinPath(machine, machine.home, ".config");
  // A debug build of Herdr is a genuinely separate install with its own
  // sessions, and both name their default session "default". Folding them
  // into one report would collide that id, so the variant is carried into it.
  return [
    { dir: joinPath(machine, base, "herdr"), variant: "" },
    { dir: joinPath(machine, base, "herdr-dev"), variant: "herdr-dev" },
  ];
}

registerScanner({
  id: "herdr",
  displayName: "Herdr",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const sessions: CollectedSession[] = [];
    let installed = false;

    for (const { dir, variant } of await configDirs(machine)) {
      // Depth 3 covers `session.json`, the sockets, and
      // `sessions/<name>/session.json`.
      const walk = await machine.walkTree(dir, {
        includeHidden: true,
        includeDirs: false,
        maxDepth: 3,
        maxEntries: 2000,
      });
      if (walk.entries.length === 0) continue;
      installed = true;

      const snapshots = walk.entries.filter((e) => e.rel.endsWith("session.json"));
      const prefixes = await machine.readFilePrefixes(
        snapshots.map((e) => ({ path: e.path, maxBytes: SNAPSHOT_BYTES })),
      );

      for (let i = 0; i < snapshots.length; i++) {
        const entry = snapshots[i];
        const prefix = prefixes[i];
        if (prefix?.error) {
          problems.push(`${entry.path}: ${prefix.error}`);
          continue;
        }
        let snapshot: Snapshot;
        try {
          snapshot = JSON.parse(prefix?.text ?? "") as Snapshot;
        } catch (e) {
          problems.push(
            `${entry.path}: ${e instanceof Error ? e.message : String(e)}`,
          );
          continue;
        }

        // The default session's snapshot sits in the config directory itself;
        // a named one is `sessions/<name>/session.json`.
        const parts = entry.rel.split("/").filter((p) => p.length > 0);
        const sessionName = parts.length >= 2 ? parts[parts.length - 2] : "default";
        const prefixId = variant ? `${variant}/` : "";

        for (const workspace of snapshot.workspaces ?? []) {
          const workspaceId = workspace.id ?? workspace.name ?? "workspace";
          for (const { id: paneId, pane } of panesOf(workspace)) {
            const agent = pane.agent_session;
            // Only panes Herdr says are running an agent. A pane with no
            // agent recorded is a terminal, not a session worth reporting.
            if (!agent?.value && !pane.managed_agent_kind && !pane.agent_name) continue;
            sessions.push({
              id: `${prefixId}${sessionName}/${workspaceId}/${paneId}`,
              title: pane.agent_name ?? pane.managed_agent_kind ?? agent?.agent,
              cwd: pane.cwd ?? workspace.identity_cwd,
              path: entry.path,
              mtime: entry.mtime,
              // The exact link: Herdr recorded the agent's own session id, so
              // no cwd guessing is needed to tie this to a transcript. A
              // `path` kind names the transcript instead, which the hub
              // cannot key on, so it is left out rather than misreported.
              agentSessionId: agent?.kind === "id" ? agent.value : undefined,
              evidence: [
                {
                  locator: entry.path,
                  saying: agent?.value
                    ? `Herdr records this pane's agent session (${agent.kind}: ${agent.value})`
                    : `Herdr records this pane as running ${pane.managed_agent_kind ?? pane.agent_name}`,
                },
              ],
            });
          }
        }
      }
    }

    return { sessions, installed, problems };
  },
});

editor.debug("agent-sessions: Herdr scanner registered");
