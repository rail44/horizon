# 001 — Make the test suite hermetic against personal config

Status: todo

## Problem

Horizon reads one TOML config file (see AGENTS.md "Configuration"). The
test suite currently lets that PERSONAL config leak into assertions about
built-in defaults: on a machine whose `~/.config/horizon/config.toml`
overrides theme colors, a bare `cargo test --workspace` fails at least:

- `ui::theme::tests::terminal_cursor_falls_back_to_accent`
- `terminal::tests::vt_stream_preserves_ansi_foreground_color`

Both pass with `HORIZON_CONFIG` pointed at a nonexistent path. The
pre-commit hook (`hooks/pre-commit`) already exports such a path as a
stopgap, but bare `cargo test` must be green on every machine regardless
of the developer's personal config.

## Constraints

- `cargo run` must keep reading the real config — only tests isolate.
- The config loader caches in a `OnceLock` (`src/config/mod.rs::load`);
  per-test env mutation is race-prone under the parallel test runner, so
  prefer a compile-time or process-wide seam over per-test `set_var`.
  One known-good shape: make the `#[cfg(test)]` build of `load()` resolve
  to built-in defaults (or a fixture path) unconditionally, with a
  test-only override hook for the config tests that genuinely need to
  parse files (`src/config/tests.rs` and the example-file drift guards —
  those must keep reading `config.example.toml` explicitly by path).
- The e2e crates (`crates/horizon-agentd/tests`) already isolate via
  explicit env vars per spawned process; leave that pattern intact.

## Acceptance

- With a populated personal config present at the real XDG path, bare
  `cargo test --workspace` is fully green.
- The example-file drift-guard tests still parse the real
  `config.example.toml`.
- The full quality gate passes; append `## Result` here and flip Status
  per docs/tasks/README.md.
