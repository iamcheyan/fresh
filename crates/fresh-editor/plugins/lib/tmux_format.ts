/**
 * tmux format-output parsing.
 *
 * Separate from the plugin so it can be tested on its own (`tests/run.sh`):
 * the plugin module talks to the editor at load, this does not.
 *
 * tmux escapes its format output — a control byte becomes `\nnn` octal and a
 * literal backslash is doubled — so the fields cannot be recovered with a
 * substring split. A pane title whose text is `\037` arrives as `\\037`,
 * where splitting on the escape finds a false separator one character in.
 * The scanner below consumes each escape atomically and is the only thing
 * that decides what is a separator.
 */

/** The byte tmux's octal escape encodes as the field separator. */
const SEP_BYTE = 0x1f;
const SEP = "\x1f";

export const SESSION_FORMAT =
  `#{session_id}${SEP}#{session_name}${SEP}#{session_created}${SEP}` +
  `#{session_attached}${SEP}#{session_path}`;
export const PANE_FORMAT =
  `#{session_id}${SEP}#{window_index}${SEP}#{window_name}${SEP}#{pane_id}${SEP}` +
  `#{pane_pid}${SEP}#{pane_current_path}${SEP}#{pane_current_command}${SEP}#{pane_title}`;

/** Foreground commands worth reporting as a probable agent. */
export const AGENT_COMMANDS = new Set([
  "claude",
  "codex",
  "gemini",
  "goose",
  "aider",
  "cursor-agent",
  "copilot",
  "qwen",
  "droid",
  "opencode",
  "amp",
]);

/** Three octal digits to a byte, or null when they are not three octal
 *  digits — which means tmux did not write an escape here. */
function decodeOctal(text: string): number | null {
  if (text.length !== 3) return null;
  let value = 0;
  for (const ch of text) {
    if (ch < "0" || ch > "7") return null;
    value = value * 8 + (ch.charCodeAt(0) - 48);
  }
  return value <= 0xff ? value : null;
}

/**
 * Split one line of tmux format output into fields, undoing the escaping.
 *
 * Escapes are consumed atomically, so a field whose *text* is `\037` (which
 * tmux sends as `\\037`) cannot be mistaken for a separator. An escape tmux
 * would never produce — a trailing lone backslash, `\9` — passes through as
 * written rather than being dropped: data we do not understand is better
 * shown than silently eaten.
 */
export function splitEscapedFields(line: string): string[] {
  const fields: string[] = [];
  let current = "";
  let i = 0;

  while (i < line.length) {
    if (line[i] !== "\\") {
      current += line[i];
      i += 1;
      continue;
    }
    const next = line[i + 1];
    if (next === "\\") {
      // An escaped backslash: one literal backslash, never a separator.
      current += "\\";
      i += 2;
      continue;
    }
    if (next !== undefined && next >= "0" && next <= "9") {
      const value = decodeOctal(line.slice(i + 1, i + 4));
      if (value !== null) {
        if (value === SEP_BYTE) {
          fields.push(current);
          current = "";
        } else {
          current += String.fromCharCode(value);
        }
        i += 4;
        continue;
      }
    }
    current += line[i];
    i += 1;
  }
  fields.push(current);
  return fields;
}

