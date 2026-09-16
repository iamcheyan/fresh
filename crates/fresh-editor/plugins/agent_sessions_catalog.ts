/// <reference path="./lib/fresh.d.ts" />

/**
 * The table-driven coding agents, for the agent-sessions hub.
 *
 * Nineteen tools that all keep transcripts the same way — a directory under
 * `$HOME`, one file per session, the working directory in the first record —
 * registered as one scanner each from a single table. Adding an agent is a
 * row in `lib/agent_catalog.ts`, not new code here.
 *
 * Tools whose layout is genuinely their own (tmux, Claude Code, Codex, Herdr,
 * Orca) get their own plugin instead.
 *
 * # Cost
 *
 * One `walkTree` per store — not one listing per directory — and then one
 * `readFilePrefixes` for every transcript the walk kept. So a store costs two
 * round trips regardless of how many sessions are in it, which is what makes
 * this usable against a machine at the end of an SSH connection.
 */

import {
  firstJsonRecord,
  isAbsoluteOn,
  joinPath,
  pickString,
  registerScanner,
  type CollectedSession,
  type Evidence,
  type ScannerReport,
} from "./lib/agent_scanner.ts";
import {
  AGENT_STORES,
  fileRuleAccepts,
  hasExtension,
  sessionIdFor,
  type AgentStore,
} from "./lib/agent_catalog.ts";

const editor = getEditor();

/** Bytes read from each transcript. The identifying records are at the very
 *  start, and transcripts reach megabytes. */
const HEADER_BYTES = 64 * 1024;

/** The directory to walk, and the variable that moved it, if any. */
async function storeRoot(
  machine: FreshMachine,
  store: AgentStore,
): Promise<{ root: string; evidence: Evidence[]; problems: string[] }> {
  const evidence: Evidence[] = [];
  const problems: string[] = [];

  let base = joinPath(machine, machine.home, ...store.baseSegments);
  if (store.baseEnv.length > 0) {
    const env = await machine.env(store.baseEnv);
    for (const name of store.baseEnv) {
      const value = env[name];
      if (!value || value.trim().length === 0) continue;
      if (isAbsoluteOn(machine, value)) {
        base = value;
        // Recorded whether or not the directory exists: "you pointed this
        // variable somewhere" is true either way, and is exactly what
        // someone debugging an empty scan needs to see. It is not evidence
        // the agent is installed.
        evidence.push({ locator: name, saying: value });
        break;
      }
      // A relative override is rejected rather than resolved: these are
      // spelled as absolute paths, and resolving one against some other
      // directory would aim the scan somewhere unrelated.
      problems.push(
        `${name} is set to a relative path (${value}); ignoring it and reading ${base}`,
      );
    }
  }

  return { root: joinPath(machine, base, ...store.suffix), evidence, problems };
}

async function scanStore(
  machine: FreshMachine,
  store: AgentStore,
): Promise<ScannerReport> {
  const { root, evidence, problems } = await storeRoot(machine, store);

  // One bulk walk for the whole store.
  const walk = await machine.walkTree(root, {
    includeHidden: true,
    includeDirs: false,
    maxDepth: store.maxDepth,
    // One past a generous cap, so "we stopped early" is observed rather than
    // inferred from a count that happens to land on the limit.
    maxEntries: 5001,
  });
  if (walk.entries.length === 0) {
    // A store that is not there is not an error: the agent is not installed.
    return { sessions: [], installed: false, problems };
  }
  if (walk.truncated) {
    problems.push(
      `${root}: stopped at the walk limit, so some sessions are not reported`,
    );
  }
  evidence.push({ locator: root, saying: `${store.displayName} session store` });

  const transcripts = walk.entries.filter(
    (entry) =>
      entry.kind !== "dir" &&
      hasExtension(store.extensions, entry.rel) &&
      fileRuleAccepts(store.fileRule, entry.rel),
  );

  if (store.layout === "opaque") {
    // Discoverable but not parsed (opencode's SQLite, Aider's per-repo
    // history). Reporting a row per file would invent sessions that cannot
    // be identified, so the files are evidence and the session list is empty.
    for (const entry of transcripts.slice(0, 50)) {
      evidence.push({
        locator: entry.path,
        saying: `${store.displayName} state; format not parsed, so no sessions are reported from it`,
      });
    }
    return { sessions: [], installed: true, problems };
  }

  // Newest first *before* any cap, so a truncated result is the newest
  // sessions rather than whichever ones sorted first by path.
  transcripts.sort((a, b) => b.mtime - a.mtime);

  // One batched read for every transcript's header, rather than one round
  // trip each.
  const prefixes = await machine.readFilePrefixes(
    transcripts.map((entry) => ({ path: entry.path, maxBytes: HEADER_BYTES })),
  );

  const sessions: CollectedSession[] = transcripts.map((entry, i) => {
    const prefix = prefixes[i];
    if (prefix?.error) problems.push(`${entry.path}: ${prefix.error}`);
    const record = prefix?.text ? firstJsonRecord(prefix.text) : null;
    return {
      id: sessionIdFor(store.layout, entry.rel),
      cwd: pickString(record, store.cwdFields),
      path: entry.path,
      mtime: entry.mtime,
      evidence: [
        {
          locator: entry.path,
          saying: `${store.displayName} transcript under ${root}`,
        },
      ],
    };
  });

  return { sessions, installed: true, problems: [...problems] };
}

for (const store of AGENT_STORES) {
  registerScanner({
    id: store.id,
    displayName: store.displayName,
    scan: (machine) => scanStore(machine, store),
  });
}

editor.debug(`agent-sessions: ${AGENT_STORES.length} catalog scanners registered`);
