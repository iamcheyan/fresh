/// <reference path="./lib/fresh.d.ts" />

/**
 * The Codex CLI, and the Codex desktop app that shares its store.
 *
 * # Layout
 *
 *     $CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl
 *     $CODEX_HOME/session_index.jsonl      # id -> title index
 *     $CODEX_HOME/state_5.sqlite           # desktop-app metadata
 *     $CODEX_HOME/history.jsonl            # cross-session prompt history
 *
 * `CODEX_HOME` defaults to `~/.codex`.
 *
 * # The first record is an envelope
 *
 * Codex does not write the session's fields at the top level. The opening
 * record wraps them:
 *
 *     {"timestamp":…,"type":"session_meta","payload":{"id":…,"cwd":"/srv/api"}}
 *
 * Reading only top-level keys loses the working directory on every real
 * transcript — the field the picker and the cross-tool correlation both lean
 * on. Confirmed against the installed Codex binary, whose string table carries
 * `session_meta` next to `payload`.
 *
 * # The date shards matter
 *
 * Sessions are bucketed by date, so the newest are reachable without reading
 * the whole tree. A flat walk of a year of daily use stats thousands of files
 * to return twenty rows.
 *
 * # The desktop app is not a separate store
 *
 * Codex Desktop reads the same `CODEX_HOME` the CLI writes, adding
 * `state_5.sqlite` beside it. So the transcripts are reported once, here, and
 * the desktop app is reported as present without duplicating them: the same
 * conversation listed under two tool names is exactly the double-count this
 * is meant to avoid.
 */

import {
  isAbsoluteOn,
  joinPath,
  jsonRecords,
  pickStringFrom,
  registerScanner,
  type CollectedSession,
  type Evidence,
  type ScannerReport,
} from "./lib/agent_scanner.ts";

const editor = getEditor();

const HEADER_BYTES = 64 * 1024;

/** `$CODEX_HOME`, or `~/.codex`. */
async function codexHome(
  machine: FreshMachine,
  problems: string[],
): Promise<{ home: string; evidence: Evidence[] }> {
  const evidence: Evidence[] = [];
  const env = await machine.env(["CODEX_HOME"]);
  const value = env["CODEX_HOME"];
  if (value && value.trim().length > 0) {
    if (isAbsoluteOn(machine, value)) {
      // Only when the variable actually moved the store. Reporting the
      // resolved default under the variable's name would claim provenance the
      // variable never had.
      evidence.push({ locator: "CODEX_HOME", saying: value });
      return { home: value, evidence };
    }
    problems.push(
      `CODEX_HOME is set to a relative path (${value}); ignoring it and reading the default`,
    );
  }
  return { home: joinPath(machine, machine.home, ".codex"), evidence };
}

registerScanner({
  id: "codex-cli",
  displayName: "Codex CLI",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const { home, evidence } = await codexHome(machine, problems);
    const sessionsDir = joinPath(machine, home, "sessions");

    // Depth 4 is `YYYY/MM/DD/<file>`. One walk for the whole shard tree.
    const walk = await machine.walkTree(sessionsDir, {
      includeHidden: true,
      includeDirs: false,
      maxDepth: 4,
      maxEntries: 5001,
    });
    if (walk.entries.length === 0) {
      return { sessions: [], installed: false, problems };
    }
    if (walk.truncated) {
      problems.push(`${sessionsDir}: stopped at the walk limit, so some sessions are not reported`);
    }
    evidence.push({
      locator: sessionsDir,
      saying: "rollout transcripts, sharded YYYY/MM/DD",
    });

    const rollouts = walk.entries.filter(
      (e) => /(^|\/)rollout-.*\.jsonl$/i.test(e.rel),
    );
    // Newest first. The shard names sort chronologically because they are
    // zero-padded, but mtime is the honest answer and costs nothing — the
    // walk already carried it.
    rollouts.sort((a, b) => b.mtime - a.mtime);

    const prefixes = await machine.readFilePrefixes(
      rollouts.map((e) => ({ path: e.path, maxBytes: HEADER_BYTES })),
    );

    const sessions: CollectedSession[] = rollouts.map((entry, i) => {
      const prefix = prefixes[i];
      if (prefix?.error) problems.push(`${entry.path}: ${prefix.error}`);
      const records = prefix?.text ? jsonRecords(prefix.text) : [];
      // `pickStringFrom` looks inside a `payload` envelope as well as at the
      // top level, which is what makes the cwd readable at all here.
      const cwd = pickStringFrom(records, ["cwd", "workdir", "working_directory"]);
      const recordedId = pickStringFrom(records, ["id", "session_id"]);

      // `rollout-<timestamp>-<uuid>.jsonl`. The uuid is the last five
      // dash-separated groups; the timestamp's own dashes come first, which
      // is why this counts from the end rather than splitting.
      const stem = (entry.rel.split("/").pop() ?? "").replace(/\.jsonl$/i, "");
      const groups = stem.split("-");
      const uuid = groups.length >= 5 ? groups.slice(-5).join("-") : stem;

      return {
        id: recordedId || uuid,
        cwd,
        path: entry.path,
        mtime: entry.mtime,
        evidence: [{ locator: entry.path, saying: "Codex rollout transcript" }],
      };
    });

    return { sessions, installed: true, problems };
  },
});

registerScanner({
  id: "codex-desktop",
  displayName: "Codex Desktop",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const { home } = await codexHome(machine, problems);

    // The state database is the desktop app's own file — the CLI does not
    // write it — so its presence is the signal that the app has run here. The
    // transcripts beside it stay the CLI scanner's to report, so this reports
    // presence and no sessions rather than listing the same conversations
    // under a second tool name.
    const probe = await machine.walkTree(home, {
      includeHidden: true,
      includeDirs: false,
      maxDepth: 1,
      maxEntries: 200,
    });
    const present = probe.entries.some((entry) => entry.rel === "state_5.sqlite");

    return { sessions: [], installed: present, problems };
  },
});

editor.debug("agent-sessions: Codex scanners registered");

export { codexHome };
