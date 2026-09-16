/**
 * tmux format-output escaping.
 *
 * Pinned on its own because getting it wrong is silent: the first version of
 * this parser split on the escape as a substring and returned zero sessions
 * against a live tmux server while every test it had still passed.
 */
import {
  splitEscapedFields,
} from "../lib/tmux_format.ts";

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

// Written by concatenation rather than as escapes, so what the test means is
// what tmux actually puts on the wire and not what a TS literal turns it into.
const BS = "\\";
const SEP = BS + "037";

eq(
  splitEscapedFields("one" + SEP + "two" + SEP + "three"),
  ["one", "two", "three"],
  "plain separators split",
);

// tmux doubles a literal backslash, so a pane title whose text is `x\037y`
// arrives as `x\\037y`. Splitting on the escape as a substring finds a false
// separator one character in and shifts every later field.
eq(
  splitEscapedFields(
    "$0" + SEP + "0" + SEP + "w" + SEP + "%0" + SEP + "1" + SEP + "/tmp" + SEP +
      "bash" + SEP + "x" + BS + BS + "037y",
  ),
  ["$0", "0", "w", "%0", "1", "/tmp", "bash", "x" + BS + "037y"],
  "an escaped backslash decodes to one backslash and does not split the row",
);

// A control byte inside a field is data: a window name holding a tab.
eq(
  splitEscapedFields("$0" + SEP + "0" + SEP + "a" + BS + "011b" + SEP + "%0"),
  ["$0", "0", "a\tb", "%0"],
  "a tab in a window name decodes rather than splitting",
);

// Escapes tmux would never send are passed through as written rather than
// dropped: data we do not understand is better shown than silently eaten.
eq(splitEscapedFields("a" + BS), ["a" + BS], "a lone trailing backslash survives");
eq(splitEscapedFields("a" + BS + "777b"), ["a" + BS + "777b"], "\\777 exceeds a byte, so it is not an escape");
eq(splitEscapedFields("a" + BS + "08b"), ["a" + BS + "08b"], "8 is not an octal digit, so \\08 is not an escape");

// An empty field is a field: a session with no path must not shift the row.
eq(
  splitEscapedFields("$0" + SEP + SEP + "x"),
  ["$0", "", "x"],
  "an empty field is preserved rather than collapsed",
);

process.exit(failures === 0 ? 0 : 1);
