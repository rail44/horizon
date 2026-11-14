# Horizon — Agent Guide

Horizon is a desktop shell for tabbed and split-pane applications, drawn
with Blitz (`docs/ui-foundation-migration-design.md`):
a keyboard-first command workspace where terminals, AI agent sessions, and
(future) WASM plugin views run as sessions attached to panes. The product
direction is recorded in `docs/ux-principles.md`; the implementation plan is
`docs/roadmap.md`, a living document — update it when decisions change.

This file records only low-churn facts: commands, conventions, and pointers.
State the repo itself expresses — module contents, public API surfaces, phase
progress — is intentionally not duplicated here; read the source or the
pointed-to document instead.

## Commands

```sh
cargo check
cargo build --workspace
cargo test
cargo run
```

`cargo build --workspace` is the canonical build command: `cargo run` alone
only rebuilds the root `horizon` binary, and Horizon's sessions run inside
**two** daemons Horizon spawns on demand — `horizon-agentd`
(`crates/horizon-agentd`) hosts agent sessions, `horizon-terminald`
(`crates/horizon-terminald`) owns every PTY (see
`docs/agent-runtime-split-design.md` and `docs/terminald-split-design.md`).
If either binary was never built (or is stale after a change on its side),
`cargo run` still starts Horizon but that domain's panes fail to spawn a
runtime — run `cargo build --workspace` first (and again after touching
`crates/horizon-agent`/`crates/horizon-agentd`, or
`crates/horizon-terminal-core`/`crates/horizon-terminald`), then use the
matching reload command from the command palette to retry:
`Reload Agent Runtime` restarts the agent daemon and leaves terminals
alone, `Reload Terminal Runtime` restarts the terminal daemon and is
explicitly destructive (every terminal session, and whatever is running in
it, ends).

**A protocol bump needs a full Horizon restart, not a runtime reload.**
The shell and `horizon-agentd` compare `HORIZON_ACP_EXT_VERSION`
(`crates/horizon-acp`) in `initialize`'s `_meta.horizon`; on a mismatch
the shell drains the daemon over `_horizon/drain` and respawns it, once
per runtime. A shell built before an extension bump paired with a daemon
built after it still needs a full app restart: the respawned daemon
mismatches again. `Reload Agent Runtime` cannot fix this because it only
replaces the daemon side. The terminal hub runs the same lockstep over
its `hello` (`MIN_SUPPORTED == current`). Rebuild, then restart the app
itself.

There is no CI. The local quality gate below is mandatory before finishing
any work that changes something the gate reads — run it yourself and make
sure all five are clean. A change that touches only `AGENTS.md`, `CLAUDE.md`,
`README.md`, or files under `docs/` is exempt: no step reads those.

```sh
cargo fmt
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
./scripts/check-wire-schema.sh
./scripts/check-preview-wasm.sh
```

**Inside a sandboxed agent session, run the `sandboxed` nextest profile
instead of the default one** (`cargo nextest run --profile sandboxed
--workspace --locked`; the other four steps are unchanged). Some tests
verify the host boundary the sandbox enforces — they bind real sockets,
write to literal `/tmp`, need directories outside any repository, or
spawn a real `horizon-agentd` — so they cannot pass from inside
containment, and making them pass would mean removing the containment
they verify. The profile skips exactly those (`.config/nextest.toml`
lists them with reasons); the integrator running the default profile
covers them when the branch lands. Do not hand-build an exclusion
list: a failure *outside* the profile's skip set is a real finding. When
you add a boundary test that cannot pass under containment, also add it to the
`sandboxed` profile's filter in `.config/nextest.toml` — the skip set is
not self-maintaining, and a missed exclusion surfaces as a red test
rather than a skipped one. nextest filter expressions (the string inside
`default-filter` and the like) accept no `#` comments — put each
exclusion's reason in a TOML `#` comment above the section, not inside the
filter string. When a run dumps many failures, parse `cargo nextest run
--format json` with `jq` rather than grepping the prose log — it is more
reliable and cheaper.

The fourth step is the wire skew checker (`docs/remoc-adoption-design.md`
§4). There is one committed wire-schema artifact per wire —
`crates/horizon-acp/schema/acp-ext-wire.json` (the `_horizon/*` and
`_meta.horizon` vocabulary on the ACP line to `horizon-agentd`),
`crates/horizon-terminal-core/schema/terminal-wire.json`, and
`crates/horizon-board/schema/log-wire.json`, each with its own version,
so an agent-side bump never drains `horizon-terminald`'s PTYs — and the
checker diffs each against the merge-base's copy, failing on
any non-additive change that doesn't bump that wire's version
(`HORIZON_ACP_EXT_VERSION` / `TERMINAL_PROTOCOL_VERSION` /
`LOG_PROTOCOL_VERSION`) with it. If you changed a wire type, regenerate
the artifacts first:
`HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-acp
-p horizon-terminal-core -p horizon-board wire_schema` (a stale artifact
is itself a red nextest test).

The fifth step checks that the root package's library still builds for
`wasm32-wasip2`, the target a preview's plugin is compiled for
(`preview-plugin/`, `src/preview.rs`). It needs that target installed (see
Build setup). When it fails, a module
reachable from the plugin started using something a wasm guest does not
have: mark the module `#[cfg(not(target_family = "wasm"))]` at its
declaration in `src/lib.rs`, or move the dependency under the root
`Cargo.toml`'s `cfg(not(target_family = "wasm"))` table.

`--workspace` is load-bearing: bare `cargo clippy`/`cargo nextest run`
from the repo root silently skip the
`horizon-agentd`/`horizon-terminald`/`horizon-agent` crates. nextest runs
each test in its own process (no cross-test env leakage) but does not run
doctests; the workspace currently has none — add `cargo test --doc` here if
that changes.

The same gate runs as a pre-commit hook (`hooks/pre-commit`), which skips
it for a commit that stages only the exempt files above. One-time setup per
clone:

```sh
git config core.hooksPath hooks
```

For refactoring discovery, see [the audit tool](scripts/refactor-audit/README.md)
and [the review guide](docs/refactoring-review.md). They cover reproducible
whole-repository or scoped scans and interpretation of the results. The audit
is an on-demand review aid, separate from the quality gate above.

For where the views are slow, `cargo bench --bench scenarios` times them a
frame at a time with no window (`docs/performance-scenarios.md`). It is
run by hand, not in the gate.

## Build setup

`crates/horizon-agent` links DuckDB dynamically, and the library is a
machine-level install, not a repository one. Install it once with
`scripts/install-duckdb-prefix.sh`, which verifies and unpacks DuckDB 2.0's
prebuilt `duckdb-shared-libs-*` release archive into `~/opt/duckdb/<version>`
(no root) and points `~/opt/duckdb/current` at it. Then put this in
`~/.cargo/config.toml` — the repo itself stays config-free:

```toml
[env]
DUCKDB_LIB_DIR = "/home/<you>/opt/duckdb/current"

[target.'cfg(target_os = "linux")']
rustflags = ["-C", "link-arg=-Wl,-rpath,/home/<you>/opt/duckdb/current"]

[target.'cfg(target_os = "macos")']
rustflags = ["-C", "link-arg=-Wl,-rpath,/Users/<you>/opt/duckdb/current"]
```

The rpath is load-bearing: libduckdb-sys links by name only, so without it
the loader resolves `libduckdb.so` from the system — 1.5.0 on Void — and the
binary runs a different engine than the bindings target. It is scoped to the
host `cfg` on purpose, because a global `RUSTFLAGS` would also reach the
`wasm32-wasip2` preview build. `ldd target/debug/horizon-agentd` must show
the `~/opt/duckdb/current` path, and an rpath also survives a GUI started
from a desktop entry, which inherits no shell environment.

Shared, not static: DuckDB 2.0 ships `core_functions` (`now()`, `range()`, …)
as a *separate* static archive, and libduckdb-sys's static link pulls in only
`libduckdb_static.a` — a `DUCKDB_STATIC=1` build then fails at query time with
`Scalar Function with name "now" is not in the catalog`. The shared library
has those built in, and the full suite passes against it.

Why not the system package: `duckdb`'s crate version encodes the engine its
bindings target (1.20000.0 = DuckDB 2.0.0), while Void's `duckdb-devel` is
1.5.0. Keep `duckdb` declared `default-features = false` for the same
reason: from 1.20000.0 the crate's defaults add libduckdb-sys's
`download-lib`, which resolves *before* pkg-config, fetches a prebuilt
library during the build, and still emits no rpath — `cargo test` then sees
2.0 (cargo puts build-script link dirs on the loader path) while the shipped
binary loads the system 1.5. A test asserting the engine version therefore
proves nothing; check `ldd`.

A machine that cannot fetch the archive can build with
`cargo build --workspace --features horizon-agent/bundled-duckdb`, which
compiles DuckDB from source (slow — the single largest compile unit here,
paid again by every build/check/clippy/test profile).

Storage compatibility, measured rather than assumed (2026-10-23): DuckDB
2.0 reports `PRAGMA version` = v2.0.0, opens the existing
`agent-state.duckdb` (1.5 format) and lists every projection table; after a
2.0 write plus `CHECKPOINT`, DuckDB 1.5.0 still reads the same file. The
upgrade is not one-way, and the projection is rebuildable from the JSONL log
regardless.

The quality gate's last step and the preview plugin build for
`wasm32-wasip2`: install the target once with `rustup target add
wasm32-wasip2`. No WASI C toolchain is needed. `preview-plugin/` is a
workspace member whose crate is empty for any other target.

Blitz is a beta, pinned to one release and patched past it in the root
`Cargo.toml` (`[patch.crates-io]`); `vendor/blitz-shell` is Horizon's own
copy of that revision's `blitz-shell`, changed where its comments say
"Added to upstream" or "Changed from upstream". A Blitz bump moves the pin,
the patch revision and that copy together.

Every worktree builds into its own `target/` — nothing is shared
between checkouts at any level, so cross-worktree correctness is
trivial. What makes a fresh worktree fast is the **target seed hook**
(`hooks/post-checkout`, run automatically by `git worktree add`): it
reflinks the main checkout's `target/` into the new worktree (btrfs
copy-on-write — metadata-only, seconds for a multi-GB tree) and then
strips workspace-member artifacts (`cargo clean -p` per member, names
from `cargo metadata`), so the worktree rebuilds its own code but never
the dependency graph. The member strip is
load-bearing, not an optimization: copied member fingerprints reference
the main checkout's sources and would otherwise validate as fresh,
handing the worktree main's binaries instead of its own. Do not skip it.
`HORIZON_SKIP_TARGET_SEED=1` disables seeding (e.g. to measure cold
builds). The hook degrades to a cold build on any failure. Plain
`git worktree add` runs it directly (agentd session worktrees, review
checkouts). Claude Code's own worktrees do not: Claude Code runs its git
with `core.hooksPath=/dev/null`, so the tracked `.claude/settings.json`
runs the same seed through `hooks/claude-worktree-seed` on
`PostToolUse(EnterWorktree)` and `SubagentStart` (an
`isolation: "worktree"` subagent never calls EnterWorktree). To check a
worktree was seeded, compare its `target/CACHEDIR.TAG` mtime with the
main checkout's: the reflink copy keeps it, a cold build writes a new one.

Two build-sharing schemes preceded this and were both retired; their
history lives in `.cargo/config.toml`'s comment, which is deliberately
config-free today. If phantom errors (E0432 on symbols that exist, a
sibling's binaries running as yours) ever appear, suspect artifact-level
sharing having crept back in.

## Configuration

Horizon reads one optional TOML file: `$XDG_CONFIG_HOME/horizon/config.toml`
(falling back to `~/.config/horizon/config.toml`), overridable via
`HORIZON_CONFIG`. `config.example.toml` at the repo root is the reference:
every key, precedence (env var > config file > built-in default), what
`Reload Config` applies live and what needs a restart, and the
environment-only path overrides. Provider entries are described in
`docs/provider-configuration.md`.

Secrets (`OPENAI_API_KEY`, `EXA_API_KEY`) are environment-only and never
read from the file. Three more settings exist only as environment variables
and are not in the example file, all measurement switches:
`HORIZON_AGENT_CLEARING_THRESHOLD_PCT` (Tier 1 compaction's trigger
percentage, clamped 1..=100, see `docs/agent-compaction-design.md`),
`HORIZON_AGENT_JUDGE_ASK_THRESHOLD` and
`HORIZON_AGENT_CONTINUATION_THRESHOLD` (the decision model's two
probability thresholds, clamped 0..=1, see
`docs/agent-decision-model-design.md`). Model ids come only from the file's
`default_model`/`small_model`/`decision_model` pairs
(`docs/provider-configuration.md`).

`crates/horizon-config` is the loader — the single file-schema, parse, and
path-resolution owner. `horizon-agentd` depends on it directly, and
`horizon-agent` takes resolved named provider values as plain arguments
rather than parsing the file itself (see that crate's `config` module doc).

## GUI Verification

Agents cannot see the window, and must not open one on a desktop a person
is using. A view is checked without a window: the tests under `tests/`
build a view (or the whole shell) into a document, feed it the document's
own input events, and assert on the document; a test that is about what is
drawn paints the page into pixels on the CPU and reads them
(`tests/terminal.rs`, `tests/parts.rs`, `tests/workspace.rs`). The window
itself does not open on a virtual display (Vello's window renderer finds
no adapter that can present there), so what only a window shows is looked
at by a person.

The display's scale matters to what is drawn: a test about pixels at a
scale other than one sets the document's viewport to it and paints at it
(`block_art_has_no_seams_in_either_pane_of_a_split_at_a_fractional_scale`).

`tests/terminald.rs`, `tests/agentd.rs` and `tests/board_logd.rs` run the
shell over real daemons, each started for the test on sockets and a data
home of its own (the agent's provider is the local mock
`scripts/mock-openai-provider.py`): a session created and typed into, the
workspace coming back after the shell is closed, a runtime reloaded.

A view still being written can be shown from a wasm plugin in a pane of
the running Horizon (`horizon preview <plugin.wasm>`,
`docs/preview-pane-design.md`): that is how an agent asks a person to look
at a view. `scripts/check-preview-plugin.sh` builds the plugin and runs
the windowless tests that draw and drive a view through it (not part of
the gate).

Manual smoke after `cargo run`: press `ctrl+'` to enter workspace mode
(`docs/workspace-mode-design.md`), then `:` to open the control surface —
a Commands-only palette. An empty (zero-tab) workspace is an implicit
command surface: `:` works directly there, no `ctrl+'` first. Session
management (attach/terminate detached sessions) is a separate modal
opened via its "Manage Sessions" command. See
README.md for the manual command checklist (`new tab`, `split pane`,
`detached`, `manage sessions`, ...).

## Module Map (`src/`)

Domain responsibilities (stable); browse each directory for its current
contents.

A view is a function that builds elements once and follows signals
afterwards. `crates/horizon-ui` is what a view builds and reads with
(`El`, `Page`) and the only code that names Blitz's document;
`crates/horizon-theme-css` is the theme and the type scale as CSS
variables. Domain logic lives in shared crates; `src/` holds the views and
their wiring:

- `workspace/` — the shell root (`Shell`): draws the shared workspace model
  (tab strip, recursive splits), keeps the sessions (the close-vs-detach
  seam), workspace mode, the key map, command dispatch (`execute`), the
  modals (palette, session manager, view chooser, model picker), the
  control plane's requests, and the runtime reloads. The model itself —
  tabs, panes, layout tree, session attachments, mode state, the pure
  command model, the `workspace.snapshot` payload — is
  `crates/horizon-workspace`.
- `terminald.rs` / `agentd.rs` — the two runtimes as the shell holds them:
  a handle that a reload replaces. The clients themselves are
  `crates/horizon-shell-runtime`, with no UI framework in them (see
  `docs/terminald-split-design.md` for why reloading one must not disturb
  the other).
- `terminal/` — the terminal view: the session's frames painted by a
  widget of its own (grid, box and block characters as shapes, the cursor,
  the selection), key/mouse/IME handling, local scrollback. PTY ownership
  lives in `crates/horizon-terminald`; emulation and the session loop in
  `crates/horizon-terminal-core`; print the kitty conformance matrix with
  `cargo test -p horizon-terminal-core print_compliance_matrix -- --nocapture`.
- `agent/` — the agent view and its live session. The fold from ACP
  session updates to what the pane shows is `horizon-shell-runtime`'s
  `agent_model` and `agent_view`. Contract/providers/tools/persistence
  live in `crates/horizon-agent`, hosted by `crates/horizon-agentd` — see
  `docs/acp-agentd-design.md`.
- `board/` — the task board as two session-less views, the list and the
  thread, over `crates/horizon-board`; what they decide from is
  `crates/horizon-board-view`. See `docs/board-redesign-design.md`.
- `markdown/` — the Markdown view (`horizon markdown <path>`), and the
  single-file watch other views share.
- `wiki/` — a page of the project wiki (`crates/horizon-wiki`) with the
  owner's comment threads anchored in it. See
  `docs/project-wiki-design.md`.
- `theme_settings.rs` / `parts/` — the theme settings view, and the parts
  it is made of (slider, select, color picker).
- `preview.rs` — the preview pane: a view shown from a wasm plugin
  (`preview-plugin/`). The plugin has the view's signals and handlers, the
  document is the shell's. See `docs/preview-pane-design.md`.
- `control.rs` — the bridge from the CLI control plane's listener to the
  UI thread; the transport is `horizon-control::host`, the client is
  `horizon <subcommand>` itself (`crates/horizon-cli`). Panes get
  `HORIZON_SOCKET`/`HORIZON_SESSION_ID` in their environment. See
  `docs/cli-control-plane-design.md`.
- `restore.rs` — the workspace kept from the last run, as far as the
  runtimes still have its sessions.
- `desktop_notify.rs` / `window.rs` / `primary.rs` — what reaches the
  desktop: notifications, whether the window has the focus, the primary
  selection.
- `background.rs` / `wake.rs` — work done off the UI thread, and what
  wakes the page to take its answer in.
- `lib.rs` / `main.rs` — the root package is a library plus a thin binary:
  `lib.rs` declares the modules and marks the ones a preview plugin cannot
  build (`#[cfg(not(target_family = "wasm"))]`); `main.rs` is the
  CLI-vs-window entry.

## Conventions

- **Code and comments are English.** Do not quote task instructions
  (often written in Japanese) verbatim into code comments — restate the
  intent in English. Japanese as *test data* (IME, multibyte-boundary
  cases) is fine and deliberate; docs under `docs/research/` are
  Japanese by choice.
- **Keep module internals crate-local.** Default to `pub(crate)` (or private)
  and re-export a narrow surface from each `mod.rs`.
- **Split modules by responsibility.** Domain directories hold small focused
  files rather than large monolith modules.
- **Operations go through the command model.** User-visible operations are
  `CommandId` variants executed via `Shell::execute`
  (`src/workspace/commands.rs`), or the commands of the plugin a pane
  shows (`src/plugin.rs`'s `Plugin::commands`, run on the cursor's pane by
  `Shell::execute_plugin`). Buttons, keyboard shortcuts, and the palette
  are bindings to commands — do not add ad-hoc behavior in UI handlers.
- **Close vs. terminate are distinct.** Closing a pane/tab detaches sessions;
  `Terminate Active Session` is the explicit destructive command. Preserve
  this separation (see `docs/ux-principles.md`).
- **A view's CSS is a CSS module.** `mod styles { horizon_ui::css_module!
  {"…"} }` beside the view: plain CSS whose classes become constants
  (`el("div").class(styles::ROW)`), with names no other sheet has. The
  macro refuses what is not written from the theme (a color or a type
  size written out, a variable the theme does not have, a property CSS
  does not have); the theme's variables are listed in
  `crates/horizon-theme-css/variables.txt`. What several views draw the
  same is a part in `src/parts/` (chip, button, field, the controls), and
  `parts::gallery` shows them all: `HORIZON_GALLERY_OUT=<file.ppm> cargo
  nextest run -p horizon --test parts --run-ignored only
  paint_the_gallery` paints it in this machine's theme.
- **Tests are colocated** next to the code in the crates (an external
  `tests.rs` declared via `#[cfg(test)] mod tests;`, or an inline
  `#[cfg(test)] mod tests { ... }` block). The root package's views are
  tested from `tests/`, one file per view, through the document.
- **Design decisions are recorded under `docs/`** (e.g.
  `agent-pane-design.md`, `agent-provider-contract.md`,
  `agent-duckdb-state-design.md`). Add or update a doc when making a
  non-obvious architectural decision.
- **Confirm your worktree before editing; keep `main` untouched.**
  Sessions share this filesystem, and the shared main checkout is live
  working state — a session has edited it by mistake before. Before
  editing anything, confirm `git rev-parse --show-toplevel` points at
  your own worktree. Work on a branch; committing to or pushing `main`
  is integration and needs the owner's explicit clearance.

There is deliberately no development-flow spec in this repository
(owner decision 2026-08-09): how work is organized changes too often to
canonize, and a stale flow description misleads every session it is
injected into. Recurring patterns worth keeping are extracted as
loadable skills instead, loaded when the pattern is actually wanted.
