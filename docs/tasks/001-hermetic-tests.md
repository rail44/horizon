# 001 — Make the test suite hermetic against personal config

Status: done

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

## Result

- Made `src/config/mod.rs::load()` hermetic under `#[cfg(test)]`:
  - In test builds it now returns `RawConfig::default()` (built-in defaults)
    unconditionally from the same `OnceLock`, so no personal config at
    `~/.config/horizon/config.toml` can leak into tests.
  - Non-test builds (`cargo run`, release, etc.) still resolve the real
    `HORIZON_CONFIG` / XDG / home path and read the file as before.
  - Path-resolution helpers and env-var constants are `#[cfg(not(test))]`
    so the test build emits no dead-code warnings.
- Verified the previously failing tests now pass with a populated personal
  config present:
  - `ui::theme::tests::terminal_cursor_falls_back_to_accent`
  - `terminal::tests::vt_stream_preserves_ansi_foreground_color`
- Verified config-file parsing is still exercised:
  - `src/config/tests.rs` keeps calling `load_from_path` directly.
  - Drift-guard tests in `src/terminal/config.rs`, `src/app/config.rs`,
    `src/ui/theme/ansi.rs`, and `src/agent/mod.rs` still read
    `config.example.toml` by path.
- Removed the now-redundant `HORIZON_CONFIG=/nonexistent/...` export from
  `hooks/pre-commit`; the compile-time seam makes the stopgate unnecessary.
- Full quality gate passed on this machine with a real personal config in
  place:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace` (266 + 163 + 7 + 19 tests passed, 0 failed)
