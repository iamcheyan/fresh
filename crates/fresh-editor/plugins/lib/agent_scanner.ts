/// <reference path="./fresh.d.ts" />

/**
 * Shared vocabulary for agent-session scanners.
 *
 * The hub (`agent_sessions.ts`) owns the machinery; each third-party tool gets
 * its own plugin that registers a scanner here. Both sides import these types
 * from this file so there is one definition rather than two that drift.
 *
 * A scanner plugin is small:
 *
 *     import { registerScanner, type Scanner } from "./lib/agent_scanner.ts";
 *
 *     registerScanner({
 *       id: "tmux",
 *       displayName: "tmux",
 *       async scan(machine) { ... },
 *     });
 */

// `getEditor()` is called inside the two functions that need it rather than
// at module load, so the pure helpers below can be imported by a test with no
// editor to talk to (see `tests/README.md`).

/** How strongly a fact is believed. */
export type Confidence = "exact" | "strong" | "likely" | "weak";

/** Where a fact came from, so a caller can answer "why do you think this
 *  exists?" without re-reading the disk. */
export interface Evidence {
  /** What was looked at — a path, an environment variable, a command. */
  locator: string;
  /** What it said, in one line. */
  saying?: string;
}

/** One session a scanner found. Deliberately flat: a caller filling a picker
 *  wants rows, not a tree it has to walk. */
export interface CollectedSession {
  /** Stable within the tool — a tmux session name, a transcript uuid. */
  id: string;
  /** Set by the hub to the registering scanner's id; a scanner may leave it
   *  out. */
  tool?: string;
  /** What to show. The hub falls back to the cwd's basename, then the id, so
   *  a row is never blank. */
  title?: string;
  /** Working directory, when the format records one. The most useful field
   *  for correlation: two tools naming the same cwd are usually the same
   *  work. */
  cwd?: string;
  /** Transcript or state file, when there is one. */
  path?: string;
  /** Unix timestamp of last activity, for newest-first ordering. */
  mtime?: number;
  /** True when the tool reports the session as attached or running. */
  attached?: boolean;
  /** A session id this row records for *another* tool — how a tmux pane is
   *  tied to the Claude transcript running inside it. */
  agentSessionId?: string;
  evidence?: Evidence[];
}

/** What one scanner reports about its tool. */
export interface ScannerReport {
  sessions: CollectedSession[];
  /** Whether the tool is present at all. Omitted means "inferred from whether
   *  anything was found". */
  installed?: boolean;
  /** Anything that went wrong but did not stop the scan. Surfaced to the
   *  user rather than swallowed. */
  problems?: string[];
}

/** A tool's scanner. */
export interface Scanner {
  /** Stable and unique; also the `tool` on every session it returns. */
  id: string;
  /** Shown as the group heading. */
  displayName: string;
  /** Read the machine and report. "Tool not installed" is an empty report,
   *  not a throw. A throw is caught and recorded as a problem, so one broken
   *  scanner cannot lose the rest of the scan. */
  scan(machine: FreshMachine): Promise<ScannerReport>;
}

/** Two rows that look like the same work seen by different tools. */
export interface Link {
  /** `tool/id` of the row doing the hosting — the tmux pane. */
  host: string;
  /** `tool/id` of the row being hosted — the agent transcript. */
  hosted: string;
  reason: "recorded-session-id" | "same-cwd";
  confidence: Confidence;
}

/** The whole answer for one machine. */
export interface ScanResult {
  machine: string;
  sessions: CollectedSession[];
  links: Link[];
  tools: { id: string; displayName: string; installed: boolean; count: number }[];
  /** Everything that went wrong, across every scanner. Never dropped: a scan
   *  that silently returns less than it should is worse than one that says
   *  what it could not read. */
  problems: string[];
}

export interface ScanOptions {
  /** Cap per scanner. These stores are unbounded — a year of daily use is
   *  tens of thousands of transcripts — and a picker wants the newest few. */
  maxSessionsPerTool?: number;
  /** Let scanners run other tools' listing commands (`tmux list-sessions`).
   *  Off keeps the scan to pure filesystem reads, which still finds every
   *  tool that writes a transcript. */
  allowCommands?: boolean;
  /** Skip the cross-tool correlation pass. */
  correlate?: boolean;
}

/** The hub's exported surface. */
export interface AgentSessionsApi {
  registerScanner(scanner: Scanner): () => void;
  unregisterScanner(id: string): boolean;
  listScanners(): { id: string; displayName: string }[];
  scan(spec: unknown | null, opts?: ScanOptions): Promise<ScanResult>;
}

/** The hub, or null when it is not loaded. */
export function hub(): AgentSessionsApi | null {
  return getEditor().getPluginApi("agent-sessions") as AgentSessionsApi | null;
}

/**
 * Register `scanner` with the hub.
 *
 * Plugins load in dependency order, then alphabetically, and every scanner
 * plugin sorts after `agent_sessions`, so the hub is normally already there.
 * When it is not — a partial load, a plugin directory without it — this waits
 * for `plugins_loaded` rather than failing silently, and says so if the hub
 * never appears. A scanner that quietly fails to register looks exactly like
 * a tool that is not installed, which is the hardest kind of bug to notice.
 */
export function registerScanner(scanner: Scanner): void {
  const api = hub();
  if (api) {
    api.registerScanner(scanner);
    return;
  }
  const editor = getEditor();
  editor.on("plugins_loaded", () => {
    const late = hub();
    if (late) {
      late.registerScanner(scanner);
    } else {
      editor.debug(
        `agent-sessions: hub not loaded; "${scanner.id}" sessions will not be discovered`,
      );
    }
  });
}

// ── Helpers every scanner needs ───────────────────────────────────

/** Join path segments with the machine's own separator. */
export function joinPath(machine: FreshMachine, ...parts: string[]): string {
  const sep = machine.platform === "windows" ? "\\" : "/";
  return parts
    .filter((p) => p.length > 0)
    .join(sep)
    .replace(/[\\/]+/g, sep);
}

/** The last component of a path, whichever separator it uses. */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter((p) => p.length > 0);
  return parts.length > 0 ? parts[parts.length - 1] : "";
}

/**
 * Whether `value` is an absolute path *on this machine*.
 *
 * Not `startsWith("/")`: a scan can run against a machine that does not share
 * the host's path rules, and a Windows target's `D:\store` is as absolute as
 * a Linux one's `/var/data`.
 */
export function isAbsoluteOn(machine: FreshMachine, value: string): boolean {
  if (machine.platform !== "windows") return value.startsWith("/");
  if (value.startsWith("\\\\")) return true; // UNC
  return /^[A-Za-z]:[\\/]/.test(value);
}

/**
 * Read the first JSON object out of each file, tolerating both shapes these
 * formats use: the fields at the top level, or wrapped in an envelope.
 *
 * Codex is the reason for the second: its rollouts open with
 * `{"timestamp":…,"type":"session_meta","payload":{"cwd":…}}`, and reading
 * only the top level loses the working directory on every real transcript.
 */
export function firstJsonRecord(text: string): Record<string, unknown> | null {
  const line = text.split("\n", 1)[0]?.trim();
  if (!line) return null;
  try {
    const parsed = JSON.parse(line) as unknown;
    return typeof parsed === "object" && parsed !== null
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

/**
 * Parse up to `maxRecords` JSONL records from a header read.
 *
 * Not just the first line: a transcript's first record is often bookkeeping
 * that carries an id but no working directory — Claude Code opens with a
 * `queue-operation` — so a caller needs to read forward until it has what it
 * wants.
 *
 * The budget is a record count rather than a byte count because a single
 * record can legitimately be a megabyte (one tool result with a large stdout),
 * and a byte budget would stop mid-record and find nothing. The last line of a
 * truncated read is very likely a partial record; it simply fails to parse and
 * is skipped, which is what any other unparseable line gets.
 */
export function jsonRecords(
  text: string,
  maxRecords = 32,
): Record<string, unknown>[] {
  const out: Record<string, unknown>[] = [];
  for (const line of text.split("\n")) {
    if (out.length >= maxRecords) break;
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const parsed = JSON.parse(trimmed) as unknown;
      if (typeof parsed === "object" && parsed !== null) {
        out.push(parsed as Record<string, unknown>);
      }
    } catch {
      // A partial or non-JSON line is skipped, not fatal.
    }
  }
  return out;
}

/** The first of `fields` present as a non-empty string across `records`,
 *  looking inside a `payload` envelope as well as at the top level. */
export function pickStringFrom(
  records: Record<string, unknown>[],
  fields: string[],
): string | undefined {
  for (const record of records) {
    const found = pickString(record, fields);
    if (found !== undefined) return found;
  }
  return undefined;
}

/** The first of `fields` present as a non-empty string, looking inside a
 *  `payload` envelope as well as at the top level. */
export function pickString(
  record: Record<string, unknown> | null,
  fields: string[],
): string | undefined {
  if (!record) return undefined;
  const envelope = record["payload"];
  const scopes: Record<string, unknown>[] = [record];
  if (typeof envelope === "object" && envelope !== null) {
    scopes.push(envelope as Record<string, unknown>);
  }
  for (const scope of scopes) {
    for (const field of fields) {
      const value = scope[field];
      if (typeof value === "string" && value.length > 0) return value;
    }
  }
  return undefined;
}
