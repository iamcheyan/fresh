/// <reference path="./lib/fresh.d.ts" />

/**
 * tmux sessions, for the agent-sessions hub.
 *
 * # What there is to find
 *
 * Almost nothing on disk, and that is the finding. tmux keeps its session
 * model in the server process; the only artefact in the filesystem is the
 * socket the server listens on, at `$TMUX_TMPDIR/tmux-<uid>/<name>` (`/tmp`
 * when unset). Confirmed on tmux 3.4:
 *
 *     $ ls -la /tmp/tmux-0/
 *     srw-rw---- 1 root root 0 Sep 16 05:58 default
 *
 * So there are two modes, answering different questions:
 *
 *   * **Sockets only** (no commands): "a tmux server exists, or recently
 *     did". A socket outlives a crashed server, so it is a weak signal.
 *   * **With commands**: ask the server. `list-sessions` gives the real list
 *     and `list-panes -a` the panes, working directories and foreground
 *     commands — which is where the agents actually are.
 *
 * # Finding agents in panes
 *
 * tmux records no agent identity, so a pane running Claude Code looks exactly
 * like one running `less`. The only signal is `pane_current_command`, the
 * basename of the foreground process, and matching on it is a guess — a shell
 * script named `claude` matches too. Pane-derived rows therefore stay weak,
 * and the hub's correlation is what ties a pane to a real transcript.
 *
 * # Delimiting fields, and tmux's escaping
 *
 * Every field tmux prints can legitimately contain `|`, `:` or a space —
 * window names and pane titles are user-set, working directories hold nearly
 * anything — so the formats below delimit with `\x1f`, which no path or title
 * realistically contains.
 *
 * tmux does not pass that byte through. It escapes format output, and the
 * first version of this scanner silently returned zero sessions because of it.
 * Measured on tmux 3.4:
 *
 *     $ tmux list-windows -F "$(printf 'X\037Y')" | od -c
 *     0000000   X   \   0   3   7   Y  \n      # the delimiter came back as four chars
 *
 *     $ tmux rename-window 'a\b'; tmux list-windows -F '#{window_name}' | od -c
 *     0000000   a   \   \   b  \n              # a literal backslash is doubled
 *
 * So tmux applies C-style escaping: a control byte becomes `\nnn` octal and a
 * literal backslash becomes `\\`. That is reversible — but only with a
 * scanner, not a substring split. A pane title containing the four characters
 * `\037` arrives as `\\037`, where a naive `split("\\037")` finds a false
 * delimiter one character in. `splitEscapedFields` therefore consumes each
 * escape atomically and is the only thing that decides what is a separator.
 */

import {
  baseName,
  isAbsoluteOn,
  registerScanner,
  type CollectedSession,
  type ScannerReport,
} from "./lib/agent_scanner.ts";
import {
  AGENT_COMMANDS,
  PANE_FORMAT,
  SESSION_FORMAT,
  splitEscapedFields,
} from "./lib/tmux_format.ts";

const editor = getEditor();

/** Sockets tmux may be listening on. */
async function socketPaths(
  machine: FreshMachine,
  problems: string[],
): Promise<string[]> {
  // Absolute only: a relative value would resolve against some other
  // directory entirely and point the search at an unrelated tmux.
  const env = await machine.env(["TMUX_TMPDIR"]);
  let tmpdir = "/tmp";
  const override = env["TMUX_TMPDIR"];
  if (override) {
    if (isAbsoluteOn(machine, override)) {
      tmpdir = override;
    } else {
      problems.push(
        `TMUX_TMPDIR is set to a relative path (${override}); ignoring it and reading /tmp`,
      );
    }
  }

  // `tmux-<uid>` directories, found by listing rather than by asking for a
  // uid: the directory is mode 0700, so this sees only the ones it may read,
  // which is the right scope anyway — and it works the same on a machine that
  // cannot report a uid at all.
  const dirs = await machine.walkTree(tmpdir, {
    includeDirs: true,
    includeHidden: true,
    maxDepth: 1,
    maxEntries: 2000,
  });
  const sockets: string[] = [];
  for (const entry of dirs.entries) {
    if (entry.kind !== "dir" || !/^tmux-\d+$/.test(entry.rel)) continue;
    const inner = await machine.walkTree(entry.path, {
      includeHidden: true,
      includeDirs: false,
      maxDepth: 1,
      maxEntries: 500,
    });
    // Anything that is not a directory inside `tmux-<uid>/` is a socket. The
    // walk reports sockets as files or symlinks depending on what the
    // transport can tell, so the test is by location rather than by kind.
    for (const sock of inner.entries) sockets.push(sock.path);
  }
  return sockets;
}

/** Ask a running server for its sessions and panes. */
async function liveSessions(
  machine: FreshMachine,
  socket: string,
  problems: string[],
): Promise<CollectedSession[] | null> {
  const list = await machine.run("tmux", ["-S", socket, "list-sessions", "-F", SESSION_FORMAT]);
  if (list.code !== 0) {
    // Not a failure worth shouting about: a socket whose server has gone is
    // exactly the case the socket-only answer covers.
    problems.push(`tmux -S ${socket} list-sessions: ${list.stderr.trim() || `exit ${list.code}`}`);
    return null;
  }

  const sessions = new Map<string, CollectedSession>();
  for (const line of list.stdout.split("\n")) {
    if (!line.trim()) continue;
    const [id, name, created, attached, path] = splitEscapedFields(line);
    if (!id) continue;
    sessions.set(id, {
      id: name || id,
      title: name || id,
      cwd: path || undefined,
      mtime: Number.parseInt(created ?? "", 10) || undefined,
      // tmux reports the number of attached clients, so "0" is detached.
      attached: (attached ?? "0") !== "0",
      evidence: [{ locator: socket, saying: "tmux server socket" }],
    });
  }

  // Panes come from one `list-panes -a` rather than one call per session:
  // the whole point of going to the server is to pay for one round trip.
  const panes = await machine.run("tmux", ["-S", socket, "list-panes", "-a", "-F", PANE_FORMAT]);
  const out: CollectedSession[] = [...sessions.values()];
  if (panes.code === 0) {
    for (const line of panes.stdout.split("\n")) {
      if (!line.trim()) continue;
      const [sessionId, windowIndex, windowName, paneId, , paneCwd, paneCommand, paneTitle] =
        splitEscapedFields(line);
      if (!paneCommand || !AGENT_COMMANDS.has(baseName(paneCommand))) continue;
      const session = sessionId ? sessions.get(sessionId) : undefined;
      out.push({
        // The pane id is unique within the server and stable while it lives.
        id: `${session?.id ?? sessionId ?? "tmux"}${paneId ?? ""}`,
        title: `${paneCommand} — ${windowName || windowIndex || ""}`.trim(),
        cwd: paneCwd || session?.cwd,
        attached: session?.attached,
        mtime: session?.mtime,
        evidence: [
          {
            locator: `${socket} ${paneId ?? ""}`.trim(),
            // Worth stating plainly: this is a name match on the foreground
            // process, not tmux telling us what is running.
            saying: `pane_current_command is "${paneCommand}"${
              paneTitle ? `, title "${paneTitle}"` : ""
            } — a name match, not an identity`,
          },
        ],
      });
    }
  } else if (panes.stderr.trim()) {
    problems.push(`tmux -S ${socket} list-panes: ${panes.stderr.trim()}`);
  }
  return out;
}

registerScanner({
  id: "tmux",
  displayName: "tmux",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const sockets = await socketPaths(machine, problems);
    if (sockets.length === 0) {
      return { sessions: [], installed: false, problems };
    }

    const sessions: CollectedSession[] = [];
    for (const socket of sockets) {
      const live = await liveSessions(machine, socket, problems);
      if (live) {
        sessions.push(...live);
        continue;
      }
      // Commands unavailable or the server gone: the socket alone is the
      // honest answer, and it is a weak one.
      sessions.push({
        id: socket,
        title: baseName(socket),
        evidence: [
          {
            locator: socket,
            saying: "socket exists; server not queried, so the session list is unknown",
          },
        ],
      });
    }
    return { sessions, installed: true, problems };
  },
});

editor.debug("agent-sessions: tmux scanner registered");
