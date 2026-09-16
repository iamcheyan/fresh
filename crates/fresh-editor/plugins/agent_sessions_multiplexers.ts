/// <reference path="./lib/fresh.d.ts" />

/**
 * GNU screen and zellij, for the agent-sessions hub.
 *
 * Neither records anything about agents, and both keep their session model in
 * a running server, so discovery is the same two-tier shape as tmux: sockets
 * from the filesystem, real sessions only when a command can be run.
 *
 *   * **screen** puts sockets in `$SCREENDIR`, else `/var/run/screen/S-<user>`
 *     or `/tmp/screens/S-<user>`. The socket is named `<pid>.<tty>.<host>`.
 *   * **zellij** keeps per-session directories under
 *     `$XDG_RUNTIME_DIR/zellij/<version>/<session>`, falling back to
 *     `/tmp/zellij-<uid>`, and `zellij list-sessions --no-formatting` prints
 *     them with a `(current)` or `(EXITED …)` suffix.
 */

import {
  baseName,
  isAbsoluteOn,
  joinPath,
  registerScanner,
  type CollectedSession,
  type ScannerReport,
} from "./lib/agent_scanner.ts";

const editor = getEditor();

registerScanner({
  id: "screen",
  displayName: "GNU screen",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const env = await machine.env(["SCREENDIR", "USER", "LOGNAME"]);

    const dirs: string[] = [];
    const explicit = env["SCREENDIR"];
    if (explicit) {
      if (isAbsoluteOn(machine, explicit)) {
        dirs.push(explicit);
      } else {
        problems.push(`SCREENDIR is set to a relative path (${explicit}); ignoring it`);
      }
    }
    const user = env["USER"] || env["LOGNAME"] || "";
    if (user) {
      dirs.push(`/var/run/screen/S-${user}`, `/tmp/screens/S-${user}`);
    }

    const sessions: CollectedSession[] = [];
    let anyDir = false;
    for (const dir of dirs) {
      const walk = await machine.walkTree(dir, {
        includeHidden: true,
        includeDirs: false,
        maxDepth: 1,
        maxEntries: 500,
      });
      if (walk.entries.length === 0) continue;
      anyDir = true;
      for (const entry of walk.entries) {
        const name = baseName(entry.path);
        // `<pid>.<tty>.<host>` — what a user recognises is the part after the
        // first dot, so the pid is dropped from the label but kept in the id.
        const dot = name.indexOf(".");
        sessions.push({
          id: name,
          title: dot >= 0 ? name.slice(dot + 1) : name,
          path: entry.path,
          mtime: entry.mtime,
          evidence: [{ locator: entry.path, saying: "screen session socket" }],
        });
      }
    }

    return { sessions, installed: anyDir, problems };
  },
});

registerScanner({
  id: "zellij",
  displayName: "zellij",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const env = await machine.env(["XDG_RUNTIME_DIR"]);

    // The runtime directory tells us zellij has run here. Its per-session
    // subdirectories are not the session list — that comes from the command.
    let installed = false;
    const runtime = env["XDG_RUNTIME_DIR"];
    if (runtime && isAbsoluteOn(machine, runtime)) {
      const walk = await machine.walkTree(joinPath(machine, runtime, "zellij"), {
        includeDirs: true,
        includeHidden: true,
        maxDepth: 1,
        maxEntries: 200,
      });
      installed = walk.entries.length > 0;
    }

    const listed = await machine.run("zellij", ["list-sessions", "--no-formatting"]);
    if (listed.code !== 0) {
      if (listed.stderr.trim()) {
        problems.push(`zellij list-sessions: ${listed.stderr.trim()}`);
      }
      return { sessions: [], installed, problems };
    }

    const sessions: CollectedSession[] = [];
    for (const line of listed.stdout.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      // `<name> [Created ...] (current)` or `(EXITED - ...)`.
      const name = trimmed.split(/\s+/)[0] ?? trimmed;

      // Three states, not two. `(current)` is the session this very command
      // ran inside; `(EXITED …)` is dead. Everything else is alive but says
      // nothing about whether a client is attached, and reporting "not
      // exited" as "attached" turns every running detached session into a
      // false positive.
      let attached: boolean | undefined;
      if (trimmed.includes("EXITED")) attached = false;
      else if (trimmed.includes("(current)")) attached = true;

      sessions.push({
        id: name,
        title: name,
        attached,
        evidence: [
          { locator: "zellij list-sessions --no-formatting", saying: trimmed },
        ],
      });
    }

    return { sessions, installed: true, problems };
  },
});

editor.debug("agent-sessions: screen and zellij scanners registered");
