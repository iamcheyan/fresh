/**
 * Properties every plugin source must hold, regardless of what it does.
 *
 * A literal NUL byte in a source file makes the plugin loader refuse the whole
 * file — and with it every test that starts an editor. It is invisible in a
 * file viewer, which renders it as a space, and `tsc` accepts it, so nothing
 * else here catches it.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const PLUGINS = join(import.meta.dirname, "..");

let failures = 0;
function check(name: string, ok: boolean, detail: string): void {
  if (ok) {
    console.log(`ok   ${name}`);
  } else {
    console.log(`FAIL ${name}\n  ${detail}`);
    failures++;
  }
}

function sources(): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(PLUGINS, { withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith(".ts")) out.push(join(PLUGINS, entry.name));
  }
  for (const entry of readdirSync(join(PLUGINS, "lib"), { withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith(".ts")) out.push(join(PLUGINS, "lib", entry.name));
  }
  return out;
}

const offenders: string[] = [];
for (const path of sources()) {
  const bytes = readFileSync(path);
  const at = bytes.indexOf(0);
  if (at >= 0) offenders.push(`${path} (byte ${at})`);
}

check(
  "no plugin source contains a literal NUL byte",
  offenders.length === 0,
  `the loader refuses these files outright:\n  ${offenders.join("\n  ")}`,
);

process.exit(failures === 0 ? 0 : 1);
