/// <reference path="./lib/fresh.d.ts" />

/**
 * Claude Code (the CLI), for the agent-sessions hub.
 *
 * # Layout, confirmed against a live install
 *
 *     ~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl
 *     ~/.claude/projects/<encoded-cwd>/<session-uuid>/     # sub-agent transcripts
 *     ~/.claude.json                                      # global settings
 *
 * # The project directory name is lossy, and that matters
 *
 * The directory under `projects/` is the working directory with every
 * non-alphanumeric character replaced by a single dash. Runs are *not*
 * collapsed, so `/home/user/fresh` becomes `-home-user-fresh` and a path
 * containing `/.claude` becomes `--claude`.
 *
 * That transform is not injective: `~/a-b` and `~/a/b` encode identically.
 * Decoding it is guesswork, and this scanner does not do it when it has a
 * better option — which it almost always does:
 *
 * **Every transcript record carries the real `cwd`.** A line from a live
 * session:
 *
 *     {"type":"user","sessionId":"3cc1b4b4-…","cwd":"/home/user/fresh",
 *      "gitBranch":"claude/serene-heisenberg-u4cdl7","version":"2.1.273", …}
 *
 * So the working directory and branch are read out of the transcript, and the
 * encoded directory name is only a fallback label when a transcript cannot be
 * read.
 *
 * # Sub-agent transcripts
 *
 * A session id also names a *directory* beside the `.jsonl`, holding the Task
 * sub-agents' transcripts. They share the parent's id and are not
 * independently resumable, so they are reported as their own rows tied back to
 * the parent rather than as sessions in their own right — a resume picker
 * filters them out, a tree view needs them.
 */

import {
  joinPath,
  jsonRecords,
  pickStringFrom,
  registerScanner,
  type CollectedSession,
  type ScannerReport,
} from "./lib/agent_scanner.ts";
import { encodeProjectDir } from "./lib/claude_code_format.ts";

const editor = getEditor();

/**
 * Enough for the header records of every transcript seen, and a bound the
 * remote case needs: without it a scan pulls whole transcripts across the
 * network to read their first few fields.
 */
const HEADER_BYTES = 256 * 1024;

registerScanner({
  id: "claude-code",
  displayName: "Claude Code",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const projects = joinPath(machine, machine.home, ".claude", "projects");

    // One bulk walk for every bucket and transcript. Depth 3 covers
    // `<bucket>/<uuid>.jsonl` and `<bucket>/<uuid>/<sub>.jsonl`.
    const walk = await machine.walkTree(projects, {
      includeHidden: true,
      includeDirs: false,
      maxDepth: 3,
      maxEntries: 5001,
    });
    if (walk.entries.length === 0) {
      return { sessions: [], installed: false, problems };
    }
    if (walk.truncated) {
      problems.push(`${projects}: stopped at the walk limit, so some sessions are not reported`);
    }

    const transcripts = walk.entries.filter((e) => e.rel.toLowerCase().endsWith(".jsonl"));
    // Newest first before any cap, so what survives is the newest rather than
    // whatever sorted first by path.
    transcripts.sort((a, b) => b.mtime - a.mtime);

    const prefixes = await machine.readFilePrefixes(
      transcripts.map((e) => ({ path: e.path, maxBytes: HEADER_BYTES })),
    );

    const sessions: CollectedSession[] = [];
    for (let i = 0; i < transcripts.length; i++) {
      const entry = transcripts[i];
      const prefix = prefixes[i];
      if (prefix?.error) problems.push(`${entry.path}: ${prefix.error}`);

      // `<bucket>/<uuid>.jsonl` is depth 2; anything deeper is a sub-agent
      // transcript under `<bucket>/<parent-uuid>/`.
      const parts = entry.rel.split("/").filter((p) => p.length > 0);
      const bucket = parts[0] ?? "";
      const isSubagent = parts.length > 2;
      const fileStem = (parts[parts.length - 1] ?? "").replace(/\.jsonl$/i, "");
      const parentId = isSubagent ? parts[parts.length - 2] : undefined;

      // Read forward rather than taking the first line: the opening record is
      // often a `queue-operation` carrying `sessionId` but no `cwd`.
      const records = prefix?.text ? jsonRecords(prefix.text) : [];
      const cwd = pickStringFrom(records, ["cwd"]);
      const branch = pickStringFrom(records, ["gitBranch"]);
      const recordedId = pickStringFrom(records, ["sessionId"]);

      const evidence = [
        {
          locator: entry.path,
          saying: cwd
            ? `Claude Code transcript; cwd read from the transcript itself`
            : `Claude Code transcript under the "${bucket}" bucket; cwd not readable, and the bucket name cannot be decoded back to a path`,
        },
      ];
      if (branch) evidence.push({ locator: entry.path, saying: `git branch ${branch}` });

      sessions.push({
        // A sub-agent shares its parent's id, so using the id alone would make
        // every sub-agent collide with the parent. The file stem is unique.
        id: isSubagent ? `${parentId}/${fileStem}` : recordedId || fileStem,
        title: isSubagent ? `sub-agent of ${parentId?.slice(0, 8) ?? "?"}` : undefined,
        cwd,
        path: entry.path,
        mtime: entry.mtime,
        // The parent's id, so the hub can tie a sub-agent to its session.
        agentSessionId: isSubagent ? parentId : undefined,
        evidence,
      });
    }

    return { sessions, installed: true, problems };
  },
});

editor.debug("agent-sessions: Claude Code scanner registered");

export { encodeProjectDir };
