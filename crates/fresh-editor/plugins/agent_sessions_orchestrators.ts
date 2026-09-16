/// <reference path="./lib/fresh.d.ts" />

/**
 * Orca, Superset and super.engineering, for the agent-sessions hub.
 *
 * Three desktop agent orchestrators with the same idea — one git worktree per
 * task, agents running inside it — and three ways of writing it down. They
 * share a plugin because they share a discovery shape: a state directory under
 * `$HOME`, a JSON blob describing worktrees, and a CLI.
 *
 * # Orca
 *
 * An Electron app. Its user-data directory is `$ORCA_USER_DATA`, else
 * `$XDG_DATA_HOME/Orca`, else `~/.orca`. Persisted state is one JSON file per
 * profile, carrying `repos`, `projects`, `folderWorkspaces` and
 * `worktreeMeta` (keyed by worktree id).
 *
 * `~/.orca/agent-hooks` is a reliable "Orca manages agents on this machine"
 * signal even when no state file parses.
 *
 * # Superset
 *
 * State under `~/.superset`: `config.json`, `sessions/` (project-less
 * workspaces, each its own git repo), and `host/<org>/manifest.json` — the
 * local host server's endpoint and token.
 *
 * The manifest means a *live* Superset host could be queried. This reports
 * that it exists and deliberately does not use the token in it: reading a
 * credential off disk to make an authenticated request is not something a
 * discovery pass should do on its own.
 *
 * # super.engineering
 *
 * A native macOS app with an `sc` CLI that has `--json` output. There is
 * nothing to read off Linux, and this says so rather than reporting it absent
 * — "you do not have it" and "I cannot see it from here" are different
 * answers, and only one of them is true.
 */

import {
  isAbsoluteOn,
  joinPath,
  registerScanner,
  type CollectedSession,
  type ScannerReport,
} from "./lib/agent_scanner.ts";

const editor = getEditor();

const STATE_BYTES = 512 * 1024;

interface WorktreeMeta {
  name?: string;
  path?: string;
  branch?: string;
  repoId?: string;
}

interface PersistedState {
  worktreeMeta?: Record<string, WorktreeMeta>;
  folderWorkspaces?: Record<string, { path?: string; name?: string }>;
}

registerScanner({
  id: "orca",
  displayName: "Orca",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const env = await machine.env(["ORCA_USER_DATA", "XDG_DATA_HOME"]);

    const explicit = env["ORCA_USER_DATA"];
    const xdg = env["XDG_DATA_HOME"];
    const data =
      explicit && isAbsoluteOn(machine, explicit)
        ? explicit
        : xdg && isAbsoluteOn(machine, xdg)
          ? joinPath(machine, xdg, "Orca")
          : joinPath(machine, machine.home, ".orca");

    const walk = await machine.walkTree(data, {
      includeHidden: true,
      includeDirs: true,
      maxDepth: 2,
      maxEntries: 2000,
    });
    if (walk.entries.length === 0) {
      return { sessions: [], installed: false, problems };
    }

    // The hooks directory is the strongest "Orca manages agents here" signal,
    // and it holds even when no state file parses — so it is reported rather
    // than inferred from whether any worktrees came back.
    const hooks = walk.entries.some((entry) => entry.rel === "agent-hooks" && entry.kind === "dir");

    const states = walk.entries.filter(
      (e) => e.kind !== "dir" && e.rel.toLowerCase().endsWith(".json"),
    );
    const prefixes = await machine.readFilePrefixes(
      states.map((e) => ({ path: e.path, maxBytes: STATE_BYTES })),
    );

    const sessions: CollectedSession[] = [];
    for (let i = 0; i < states.length; i++) {
      const entry = states[i];
      const prefix = prefixes[i];
      if (prefix?.error) {
        problems.push(`${entry.path}: ${prefix.error}`);
        continue;
      }
      let state: PersistedState;
      try {
        state = JSON.parse(prefix?.text ?? "") as PersistedState;
      } catch {
        // Not every JSON file under the data directory is persisted state —
        // keybindings.json is not — so an unparseable one is expected, not a
        // problem worth reporting.
        continue;
      }

      for (const [id, meta] of Object.entries(state.worktreeMeta ?? {})) {
        sessions.push({
          id,
          title: meta.name ?? meta.branch ?? id,
          cwd: meta.path,
          path: entry.path,
          mtime: entry.mtime,
          evidence: [
            {
              locator: entry.path,
              saying: `Orca worktree${meta.branch ? ` on branch ${meta.branch}` : ""}`,
            },
          ],
        });
      }
    }

    if (hooks && sessions.length === 0) {
      problems.push(
        `${data}: Orca installs agent hooks here, so it manages agents on this machine, but no worktree state could be read`,
      );
    }
    return { sessions, installed: true, problems };
  },
});

registerScanner({
  id: "superset",
  displayName: "Superset",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const root = joinPath(machine, machine.home, ".superset");

    const walk = await machine.walkTree(root, {
      includeHidden: true,
      includeDirs: true,
      maxDepth: 3,
      maxEntries: 2000,
    });
    if (walk.entries.length === 0) {
      return { sessions: [], installed: false, problems };
    }

    // Each directory directly under `sessions/` is a workspace.
    const sessions: CollectedSession[] = walk.entries
      .filter((e) => e.kind === "dir" && /^sessions\/[^/]+$/.test(e.rel))
      .map((entry) => ({
        id: entry.rel.slice("sessions/".length),
        title: entry.rel.slice("sessions/".length),
        cwd: entry.path,
        path: entry.path,
        mtime: entry.mtime,
        evidence: [
          { locator: entry.path, saying: "Superset session workspace (its own git repo)" },
        ],
      }));

    // Reported because it means a live host could be queried — not used.
    for (const manifest of walk.entries.filter((e) => e.rel.endsWith("manifest.json"))) {
      problems.push(
        `${manifest.path}: a Superset host manifest is present; its endpoint is not queried and its token is deliberately not read`,
      );
    }

    return { sessions, installed: true, problems };
  },
});

registerScanner({
  id: "super-engineering",
  displayName: "super.engineering",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];

    // A native macOS app. Off macOS there is genuinely nothing to read, and
    // saying "not installed" would claim knowledge this does not have.
    if (machine.platform !== "macos" && machine.platform !== "") {
      problems.push(
        "super.engineering ships for macOS; nothing to inspect from this platform",
      );
      return { sessions: [], installed: false, problems };
    }

    const listed = await machine.run("sc", ["list", "--json"]);
    if (listed.code !== 0) {
      if (listed.stderr.trim()) problems.push(`sc list --json: ${listed.stderr.trim()}`);
      return { sessions: [], installed: false, problems };
    }

    let rows: unknown;
    try {
      rows = JSON.parse(listed.stdout) as unknown;
    } catch (e) {
      problems.push(`sc list --json: ${e instanceof Error ? e.message : String(e)}`);
      return { sessions: [], installed: true, problems };
    }
    if (!Array.isArray(rows)) {
      problems.push("sc list --json did not return an array");
      return { sessions: [], installed: true, problems };
    }

    const sessions: CollectedSession[] = [];
    for (const row of rows as Record<string, unknown>[]) {
      const id = typeof row["id"] === "string" ? row["id"] : undefined;
      if (!id) continue;
      // The worktree path, under whichever of these keys this build uses.
      const cwd = ["worktree", "path", "directory"]
        .map((key) => row[key])
        .find((value): value is string => typeof value === "string" && value.length > 0);
      sessions.push({
        id,
        title: typeof row["name"] === "string" ? row["name"] : id,
        cwd,
        evidence: [{ locator: "sc list --json", saying: `super.engineering session ${id}` }],
      });
    }

    return { sessions, installed: true, problems };
  },
});

editor.debug("agent-sessions: Orca, Superset and super.engineering scanners registered");
