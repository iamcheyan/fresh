/// <reference path="./lib/fresh.d.ts" />

/**
 * Agent Sessions — the scanner registry.
 *
 * Other tools leave their sessions on disk: tmux sockets, Claude Code
 * transcripts, Codex rollouts, Herdr snapshots, Orca worktrees. This plugin
 * knows *none* of those formats. It owns the machinery — open the machine,
 * run every registered scanner against it, merge and correlate what they
 * return — and each tool's knowledge lives in its own plugin, registered
 * here:
 *
 *     const hub = editor.getPluginApi("agent-sessions");
 *     hub?.registerScanner({
 *       id: "tmux",
 *       displayName: "tmux",
 *       async scan(machine) {
 *         const { entries } = await machine.walkTree(`${machine.home}/.tmux`, {
 *           includeHidden: true,
 *         });
 *         return { sessions: entries.map(toSession) };
 *       },
 *     });
 *
 * That split is the point. These are *other projects'* file formats, and they
 * change on their schedule rather than the editor's: a scanner living in a
 * `.ts` file can be fixed by editing it, where the same knowledge compiled
 * into the editor needs a new binary to correct a field name.
 *
 * A scanner is handed a machine and asks it questions. It never reads a path
 * itself, never learns whether it is looking at this computer or one at the
 * end of an SSH connection, and cannot stall the editor: every call it can
 * make runs off the editor thread.
 */

import type {
  AgentSessionsApi,
  CollectedSession,
  Link,
  ScanOptions,
  ScanResult,
  Scanner,
  ScannerReport,
  MachineSpec,
} from "./lib/agent_scanner.ts";
import { baseName } from "./lib/agent_scanner.ts";

const editor = getEditor();

declare global {
  interface FreshPluginRegistry {
    "agent-sessions": AgentSessionsApi;
  }
}

// ── Registry ──────────────────────────────────────────────────────

const scanners: Scanner[] = [];

function registerScanner(scanner: Scanner): () => void {
  if (typeof scanner !== "object" || scanner === null) {
    throw new Error("agent-sessions.registerScanner: scanner must be an object");
  }
  if (typeof scanner.id !== "string" || scanner.id.length === 0) {
    throw new Error("agent-sessions.registerScanner: id must be a non-empty string");
  }
  if (typeof scanner.displayName !== "string" || scanner.displayName.length === 0) {
    throw new Error("agent-sessions.registerScanner: displayName must be a non-empty string");
  }
  if (typeof scanner.scan !== "function") {
    throw new Error("agent-sessions.registerScanner: scan must be a function");
  }
  // Re-registering the same id replaces, so a plugin reload does not end up
  // scanning twice and reporting every session in duplicate.
  unregisterScanner(scanner.id);
  scanners.push(scanner);
  scanners.sort((a, b) => a.id.localeCompare(b.id));
  return () => {
    unregisterScanner(scanner.id);
  };
}

function unregisterScanner(id: string): boolean {
  const i = scanners.findIndex((s) => s.id === id);
  if (i < 0) return false;
  scanners.splice(i, 1);
  return true;
}

// ── Scanning ──────────────────────────────────────────────────────

/** A link's two endpoints as one key. The separator is a character that
 *  cannot appear in a tool id or a session id, so two different pairs can
 *  never collide into one. */
function pairKey(host: string, hosted: string): string {
  return `${host}\u0000${hosted}`;
}

/** `tool/id`, the key a link refers to a row by. */
function keyOf(session: CollectedSession): string {
  return `${session.tool}/${session.id}`;
}

/** Newest first, with unknown mtimes last rather than pretending to be old. */
function byNewest(a: CollectedSession, b: CollectedSession): number {
  return (b.mtime ?? 0) - (a.mtime ?? 0);
}

/**
 * Tie rows together across tools.
 *
 * Two reasons, in strength order. A row that *records* another tool's session
 * id is exact — a tmux pane naming the Claude session running in it is not a
 * guess. A shared cwd is weaker: two tools open on the same directory are
 * usually the same work, but need not be.
 */
function correlate(sessions: CollectedSession[]): Link[] {
  const links: Link[] = [];

  const byId = new Map<string, CollectedSession[]>();
  for (const s of sessions) {
    const list = byId.get(s.id);
    if (list) list.push(s);
    else byId.set(s.id, [s]);
  }

  for (const host of sessions) {
    if (!host.agentSessionId) continue;
    for (const hosted of byId.get(host.agentSessionId) ?? []) {
      if (hosted.tool === host.tool) continue;
      links.push({
        host: keyOf(host),
        hosted: keyOf(hosted),
        reason: "recorded-session-id",
        confidence: "exact",
      });
    }
  }

  // Only rows not already tied by a recorded id: an exact link makes the
  // cwd coincidence redundant noise.
  const exact = new Set(links.map((l) => pairKey(l.host, l.hosted)));
  const byCwd = new Map<string, CollectedSession[]>();
  for (const s of sessions) {
    if (!s.cwd) continue;
    const list = byCwd.get(s.cwd);
    if (list) list.push(s);
    else byCwd.set(s.cwd, [s]);
  }
  for (const group of byCwd.values()) {
    if (group.length < 2) continue;
    for (const host of group) {
      for (const hosted of group) {
        if (host === hosted || host.tool === hosted.tool) continue;
        const pair = pairKey(keyOf(host), keyOf(hosted));
        if (exact.has(pair)) continue;
        links.push({
          host: keyOf(host),
          hosted: keyOf(hosted),
          reason: "same-cwd",
          confidence: "likely",
        });
      }
    }
  }

  return links;
}

async function scan(spec: MachineSpec | null, opts?: ScanOptions): Promise<ScanResult> {
  const limit = opts?.maxSessionsPerTool ?? 200;
  const problems: string[] = [];

  // A spec opens its own machine; `null` means "wherever the editor is
  // acting", which the bare ops already address — so that case is the same
  // shape with the machine id left at 0 and nothing to close.
  const machine: FreshMachine =
    spec === null || spec === undefined
      ? {
          id: 0,
          platform: "",
          home: editor.getEnv("HOME") ?? editor.getEnv("USERPROFILE") ?? "",
          label: "",
          walkTree: (root, options) => editor.walkTree(root, options),
          readFilePrefixes: (requests) => editor.readFilePrefixes(requests),
          run: (program, args, cwd) => editor.runOnTarget(program, args, cwd),
          env: (names) => editor.machineEnv(names),
          close: () => Promise.resolve(true),
        }
      : await editor.openMachine(spec);

  const sessions: CollectedSession[] = [];
  const tools: ScanResult["tools"] = [];

  try {
    for (const scanner of scanners) {
      let report: ScannerReport;
      try {
        report = await scanner.scan(machine);
      } catch (e) {
        // One scanner throwing must not lose the rest of the scan — and the
        // failure is reported rather than swallowed.
        problems.push(`${scanner.id}: ${e instanceof Error ? e.message : String(e)}`);
        tools.push({
          id: scanner.id,
          displayName: scanner.displayName,
          installed: false,
          count: 0,
        });
        continue;
      }

      const found = (report.sessions ?? []).slice().sort(byNewest);
      if (found.length > limit) {
        problems.push(
          `${scanner.id}: reporting the newest ${limit} of ${found.length} sessions`,
        );
        found.length = limit;
      }
      for (const s of found) {
        // The scanner's own id wins over whatever it put in `tool`, so a row
        // can never be attributed to a scanner that did not produce it.
        //
        // `||` rather than `??`: a scanner that found no title tends to report
        // an empty string rather than omitting the field, and `??` would let
        // that through and render a blank row.
        const title = s.title || (s.cwd ? baseName(s.cwd) : "") || s.id;
        sessions.push({ ...s, tool: scanner.id, title });
      }
      tools.push({
        id: scanner.id,
        displayName: scanner.displayName,
        installed: report.installed ?? found.length > 0,
        count: found.length,
      });
      for (const p of report.problems ?? []) problems.push(`${scanner.id}: ${p}`);
    }
  } finally {
    // Always, including when a scanner threw: a leaked handle holds an SSH
    // connection open for the life of the editor.
    if (machine.id !== 0) await machine.close();
  }

  sessions.sort(byNewest);
  return {
    machine: machine.label || "local",
    sessions,
    links: opts?.correlate === false ? [] : correlate(sessions),
    tools,
    problems,
  };
}

editor.exportPluginApi("agent-sessions", {
  registerScanner,
  unregisterScanner,
  listScanners(): { id: string; displayName: string }[] {
    return scanners.map((s) => ({ id: s.id, displayName: s.displayName }));
  },
  scan,
} satisfies AgentSessionsApi);

editor.debug(`Agent Sessions plugin loaded (${scanners.length} scanners registered)`);

