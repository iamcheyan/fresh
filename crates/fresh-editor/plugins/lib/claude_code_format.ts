/**
 * Claude Code's project-bucket encoding.
 *
 * Apart from the plugin so it can be tested without an editor to talk to.
 *
 * The bucket under `~/.claude/projects/` is the working directory with every
 * non-alphanumeric character replaced by a single dash. Runs are *not*
 * collapsed, so `/home/user/fresh` becomes `-home-user-fresh`.
 *
 * Only this direction is reliable. The transform is not injective — `~/a-b`
 * and `~/a/b` encode identically — so a caller holding a bucket name cannot
 * recover the path, which is why the scanner reads the real `cwd` out of the
 * transcript instead.
 */

/**
 * Encode a working directory the way Claude Code names its project bucket.
 *
 * Exported because it is the one direction of the transform that is reliable:
 * a caller holding a cwd can find its bucket, where a caller holding a bucket
 * cannot recover the cwd.
 */
export function encodeProjectDir(path: string): string {
  const normalised = path.replace(/\\/g, "/");
  // A lone root keeps its slash; anything else loses a trailing one, so
  // `/a/b` and `/a/b/` land in the same bucket.
  const trimmed =
    normalised === "/" || /^[A-Za-z]:\/$/.test(normalised)
      ? normalised
      : normalised.replace(/\/+$/, "");
  return trimmed.replace(/[^A-Za-z0-9]/g, "-");
}

