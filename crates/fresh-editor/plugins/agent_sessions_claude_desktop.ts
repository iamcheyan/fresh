/// <reference path="./lib/fresh.d.ts" />

/**
 * The Claude desktop application, for the agent-sessions hub.
 *
 * # What it does and does not hold
 *
 * Claude Desktop is not a coding-agent session store: conversations live
 * server-side, and what it keeps locally is application state — MCP server
 * configuration, an Electron cache, logs. So this reports *presence and
 * configuration*, not sessions, and says so rather than returning an empty
 * list that reads like "no conversations".
 *
 * The one genuinely useful artefact is `claude_desktop_config.json`, whose
 * `mcpServers` map names every MCP server the app launches. Those are
 * long-running child processes of a session-multiplexing tool, which puts
 * them squarely in scope.
 *
 * # Paths
 *
 *     macOS   ~/Library/Application Support/Claude/claude_desktop_config.json
 *     Linux   ~/.config/Claude/claude_desktop_config.json
 *     Windows %APPDATA%/Claude/claude_desktop_config.json
 */

import {
  isAbsoluteOn,
  joinPath,
  registerScanner,
  type ScannerReport,
} from "./lib/agent_scanner.ts";

const editor = getEditor();

/** The application-support directory for `app`, per platform. */
async function appSupportDir(machine: FreshMachine, app: string): Promise<string> {
  if (machine.platform === "macos") {
    return joinPath(machine, machine.home, "Library", "Application Support", app);
  }
  if (machine.platform === "windows") {
    const env = await machine.env(["APPDATA"]);
    const appdata = env["APPDATA"];
    if (appdata && isAbsoluteOn(machine, appdata)) return joinPath(machine, appdata, app);
    return joinPath(machine, machine.home, "AppData", "Roaming", app);
  }
  const env = await machine.env(["XDG_CONFIG_HOME"]);
  const xdg = env["XDG_CONFIG_HOME"];
  if (xdg && isAbsoluteOn(machine, xdg)) return joinPath(machine, xdg, app);
  return joinPath(machine, machine.home, ".config", app);
}

registerScanner({
  id: "claude-desktop",
  displayName: "Claude Desktop",

  async scan(machine: FreshMachine): Promise<ScannerReport> {
    const problems: string[] = [];
    const dir = await appSupportDir(machine, "Claude");

    const walk = await machine.walkTree(dir, {
      includeHidden: true,
      includeDirs: false,
      maxDepth: 1,
      maxEntries: 200,
    });
    if (walk.entries.length === 0) {
      return { sessions: [], installed: false, problems };
    }

    const config = walk.entries.find((entry) => entry.rel === "claude_desktop_config.json");
    if (!config) {
      // Present, but nothing configured to report.
      return { sessions: [], installed: true, problems };
    }

    const [prefix] = await machine.readFilePrefixes([
      { path: config.path, maxBytes: 1024 * 1024 },
    ]);
    if (prefix?.error) {
      problems.push(`${config.path}: ${prefix.error}`);
      return { sessions: [], installed: true, problems };
    }

    let servers: string[] = [];
    try {
      const parsed = JSON.parse(prefix?.text ?? "") as {
        mcpServers?: Record<string, unknown>;
      };
      servers = Object.keys(parsed.mcpServers ?? {});
    } catch (e) {
      problems.push(`${config.path}: ${e instanceof Error ? e.message : String(e)}`);
      return { sessions: [], installed: true, problems };
    }

    // Reported as a problem rather than as sessions on purpose: these are
    // configuration, not conversations, and inventing a session row per MCP
    // server would put things in a session picker that cannot be resumed.
    problems.push(
      servers.length === 0
        ? `${config.path}: Claude Desktop is installed; no MCP servers configured, and conversations live server-side so none are discoverable here`
        : `${config.path}: Claude Desktop launches ${servers.length} MCP server(s): ${servers.join(", ")}`,
    );

    return { sessions: [], installed: true, problems };
  },
});

editor.debug("agent-sessions: Claude Desktop scanner registered");
