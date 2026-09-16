/**
 * Claude Code's bucket encoding, and reading past the opening record.
 *
 * Both checked against a live install on this machine: the bucket for
 * `/home/user/fresh` is `-home-user-fresh`, and the first record of a real
 * transcript is a `queue-operation` that carries `sessionId` but no `cwd`.
 */
import { encodeProjectDir } from "../lib/claude_code_format.ts";
import { jsonRecords, pickStringFrom } from "../lib/agent_scanner.ts";

let failures = 0;
function eq(actual: unknown, expected: unknown, name: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    console.log(`FAIL ${name}\n  got      ${a}\n  expected ${e}`);
    failures++;
  } else {
    console.log(`ok   ${name}`);
  }
}

// Verified against ~/.claude/projects on a machine running this session.
eq(encodeProjectDir("/home/user/fresh"), "-home-user-fresh", "matches a real bucket name");

// Runs are not collapsed: each non-alphanumeric character becomes one dash.
eq(encodeProjectDir("/home/user/.claude"), "-home-user--claude", "a dot becomes its own dash");
eq(encodeProjectDir("/a//b"), "-a--b", "a doubled slash becomes two dashes");

// A trailing slash must not make a second bucket for the same directory.
eq(encodeProjectDir("/a/b/"), encodeProjectDir("/a/b"), "a trailing slash is ignored");
eq(encodeProjectDir("/"), "-", "the root keeps its one slash");

// Windows paths encode too, separators and drive colon alike.
eq(encodeProjectDir("C:\\Users\\x"), "C--Users-x", "a windows path encodes");

// The transform is lossy, and the scanner must not pretend otherwise: these
// two different directories share one bucket, which is why the cwd is read
// from the transcript instead of decoded from the name.
eq(
  encodeProjectDir("/home/a-b") === encodeProjectDir("/home/a/b"),
  true,
  "different paths can share a bucket, so the name cannot be decoded back",
);

// The real opening record: sessionId present, cwd absent. Reading only the
// first line would lose the working directory on every live transcript.
const REAL_HEAD = [
  '{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-16T05:54:01.939Z","sessionId":"3cc1b4b4-ea2d-5dac-9a45-aa08481b6b8f","content":"create a sub-crate"}',
  '{"type":"user","sessionId":"3cc1b4b4-ea2d-5dac-9a45-aa08481b6b8f","cwd":"/home/user/fresh","gitBranch":"claude/serene-heisenberg-u4cdl7","version":"2.1.273"}',
].join("\n");

const records = jsonRecords(REAL_HEAD);
eq(records.length, 2, "both records parse");
eq(pickStringFrom(records, ["cwd"]), "/home/user/fresh", "the cwd is found in a later record");
eq(pickStringFrom(records, ["gitBranch"]), "claude/serene-heisenberg-u4cdl7", "so is the branch");
eq(
  pickStringFrom([records[0]], ["cwd"]),
  undefined,
  "and the opening record alone genuinely has no cwd — the read-forward is load-bearing",
);

// A truncated final record is skipped rather than throwing.
eq(
  jsonRecords('{"cwd":"/a"}\n{"cwd":"/b"' ).length,
  1,
  "a partial trailing record is skipped, not fatal",
);

process.exit(failures === 0 ? 0 : 1);
