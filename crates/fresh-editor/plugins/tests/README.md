# Plugin unit tests

Pure-logic tests for plugin code that is worth pinning on its own — parsers,
escaping, path rules — run with Node's built-in TypeScript stripping:

    ./tests/run.sh

Not wired into CI: the editor has no JS test harness, and these cover logic
that is otherwise only exercised by loading the whole editor. Run them when
touching the code they cover.

A test imports the real plugin source. It must not import a plugin's
top-level module, which calls `getEditor()` at load and has no editor to talk
to here; keep the logic under test in a function the test can reach, as
`agent_sessions_tmux.ts` does with `splitEscapedFields`.
