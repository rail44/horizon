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
./scripts/extensions.sh build
cargo test
cargo run
```

`cargo build --workspace` is the canonical build command: `cargo run` alone
only rebuilds the root `horizon` binary, and Horizon's sessions run in
**two** other processes — `horizon-agent-acp` (`crates/horizon-agent-acp`)
hosts agent sessions as Horizon's child, speaking ACP over its standard
input and output (`docs/agent-stdio-child-design.md`), and
`horizon-terminald` (`crates/horizon-terminald`), a daemon Horizon spawns
on demand, owns every PTY (`docs/terminald-split-design.md`).
If either binary was never built (or is stale after a change on its side),
`cargo run` still starts Horizon but that domain's panes fail to spawn a
runtime — run `cargo build --workspace` first (and again after touching
`crates/horizon-agent`, `crates/horizon-agent-host`, the flows composed
into it (`crates/horizon-board-flow`, `crates/horizon-wiki-flow`) or
`crates/horizon-agent-acp`, or
`crates/horizon-terminal-core`/`crates/horizon-terminald`). The agent's
child is started again whenever it ends, from the binary as it is then,
and the panes it had open are attached to their sessions again with
`session/resume`: to have a rebuilt agent used, end the child (`pkill -f
horizon-agent-acp`), or restart Horizon. For the terminals, `Reload
Terminal Runtime` from the command palette has the terminal daemon hand
its terminals to the rebuilt binary (`docs/terminald-handover-design.md`):
the sessions, and whatever runs in them, go on. A terminal daemon built
before handovers is drained instead, once, and its terminals end.

**A terminal protocol bump needs a full Horizon restart, not a runtime
reload.** The terminal hub compares its protocol version in its `hello`
(`MIN_SUPPORTED == current`). Rebuild, then restart the app itself. The
line to `horizon-agent-acp` is standard ACP v2 with no extension of
Horizon's, and has no version of its own to compare.

The Markdown view, the wiki, the theme settings, the list of sessions,
the image viewer and the status bar are extensions (`plugins/*`),
wasm components `cargo build --workspace` does not build: the second
command above does (`--release` for a release Horizon), and Horizon reads
each component from beside its executable or from where that command
leaves it (`target/wasm32-wasip2/<profile>`, for the executable in
`target/<profile>`; `src/extension.rs`'s `own`). Without one Horizon
starts and does not have that view. A component built again is read again
by the running Horizon, with no reload.

There is no CI. The local quality gate below is mandatory before finishing
any work that changes something the gate reads — run it yourself and make
sure every step is clean (`scripts/gate.sh` runs them in order and stops
at the first that fails). A change that touches only `AGENTS.md`,
`CLAUDE.md`, `README.md`, or files under `docs/` is exempt: no step reads
those.

```sh
cargo fmt
CARGO_BUILD_WARNINGS=deny cargo clippy --workspace --all-targets --locked -- -D warnings
CARGO_BUILD_WARNINGS=deny ./scripts/extensions.sh clippy --locked -- -D warnings
./scripts/extensions.sh build --locked
cargo nextest run --workspace --locked
./scripts/check-wire-schema.sh
```

**Inside a sandboxed agent session, run the `sandboxed` nextest profile
instead of the default one** (`cargo nextest run --profile sandboxed
--workspace --locked`; the other steps are unchanged). Some tests
verify the host boundary the sandbox enforces — they bind real sockets,
write to literal `/tmp`, need directories outside any repository, or
spawn a real `horizon-agent-acp` — so they cannot pass from inside
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

`CARGO_BUILD_WARNINGS=deny` is for Cargo's own lints, which `-D warnings`
does not reach: a dependency a member declares and does not use
(`cargo::unused_dependencies`, on for every member through the workspace's
`[workspace.lints]`) fails the step with it.

The two steps before the tests are the extensions' (`plugins/*`): their
code is built for a guest only, so the workspace's clippy sees empty
crates and they are linted for their own target, and the tests that show
them in a pane load the components the build leaves
(`tests/extension.rs`). Both need the `wasm32-wasip2`
target installed (see Build setup).

`check-wire-schema.sh` is the wire skew checker (`docs/remoc-adoption-design.md`
§4). There is one committed wire-schema artifact per wire —
`crates/horizon-terminal-core/schema/terminal-wire.json` and
`crates/horizon-board/schema/log-wire.json`, each with its own version,
so a bump of one never drains the other's daemon — and the
checker diffs each against the merge-base's copy, failing on
any non-additive change that doesn't bump that wire's version
(`TERMINAL_PROTOCOL_VERSION` / `LOG_PROTOCOL_VERSION`) with it. If you
changed a wire type, regenerate the artifacts first:
`HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-terminal-core
-p horizon-board wire_schema` (a stale artifact is itself a red nextest
test).

`--workspace` is load-bearing: bare `cargo clippy`/`cargo nextest run`
from the repo root silently skip the
`horizon-agent-acp`/`horizon-terminald`/`horizon-agent` crates. nextest runs
each test in its own process (no cross-test env leakage) but does not run
doctests; the workspace currently has none — add `cargo test --doc` here if
that changes.

Nothing runs the gate for you: there is no CI, and no hook runs it on a
commit. A commit on a work branch need not pass it. It is run at two
points:

- before finishing work, by whoever did the work (above);
- where a branch lands on `main`, by whoever integrates it: with the
  default profile, outside any sandbox, on the commits as they will be on
  `main` (after a rebase, on the rebased branch). `main` is not moved until
  every step is clean.

The second is what the `sandboxed` profile's skipped tests rely on: nothing
else runs them.

Between those points, while working, test what the change touches with
nextest's filters rather than running the whole gate: `cargo nextest run
--workspace -E 'package(<crate>)'` for the crates changed (`rdeps(...)`
adds the crates that use them). clippy over the whole workspace stays
cheap and catches what no longer compiles in the crates that use the
changed ones. Some links are outside the dependency graph,
so a filter by crate does not see them: the extensions (`plugins/*`) are
tested by the root package's tests that load them (`tests/extension.rs`,
`tests/wiki.rs`, ...), and the processes (`horizon-agent-acp`,
`horizon-terminald`, `horizon-logd`) by the root package's tests that
spawn them (`tests/agent_acp.rs`, `tests/terminald.rs`, `tests/board_logd.rs`).
A change to `Cargo.lock`, `.cargo/` or `.config/nextest.toml` reaches
everything.

For refactoring discovery, see [the audit tool](scripts/refactor-audit/README.md)
and [the review guide](docs/refactoring-review.md). They cover reproducible
whole-repository or scoped scans and interpretation of the results. The audit
is an on-demand review aid, separate from the quality gate above.

For where the views are slow, `cargo bench --bench scenarios` times them a
frame at a time with no window (`docs/performance-scenarios.md`). It is
run by hand, not in the gate.

## Build setup

The agent's `bash` calls run contained by Fence
(https://github.com/fencesandbox/fence, `docs/sandbox-runtime-design.md`),
a machine-level install like the ones below: Homebrew on macOS (`brew tap
fencesandbox/tap && brew install fencesandbox/tap/fence`), the release
binary or `go install github.com/fencesandbox/fence/cmd/fence@latest` on
Linux, which also needs `bwrap` and `socat`. Without it Horizon runs and
every `bash` call asks to run outside a sandbox; the tests that start it
(`crates/horizon-sandbox/tests/fence.rs` and a few in `horizon-agent` and
`horizon-agent-acp`) fail with a message saying so. `config.toml`'s
`[sandbox] fence` names the binary when it is not on the daemon's `PATH`.

`crates/horizon-terminal-core` builds its terminal engine, libghostty-vt,
from source with Zig 0.17 (the `libghostty-vt-sys` crate's build script
calls `zig build`): install it once, e.g. `proto install zig 0.17.1 --pin
global`. The repo's `.cargo/config.toml` has the engine built optimized in
every profile (`LIBGHOSTTY_VT_SYS_OPTIMIZE`); its comment says why.

`crates/horizon-agent` links DuckDB dynamically, and the library is a
machine-level install, not a repository one. Install it once with
`scripts/install-duckdb-prefix.sh`, which verifies and unpacks DuckDB 2.0's
prebuilt `duckdb-shared-libs-*` release archive into `~/opt/duckdb/<version>`
(no root) and points `~/opt/duckdb/current` at it. Then put this in
`~/.cargo/config.toml` — the paths are this machine's, so they stay out of
the repo's own `.cargo/config.toml`:

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
`wasm32-wasip2` preview build. `ldd target/debug/horizon-agent-acp` must show
the `~/opt/duckdb/current` path, and an rpath also survives a GUI started
from a desktop entry, which inherits no shell environment.

Shared, not static: DuckDB 2.0 ships `core_functions` (`now()`, `range()`, …)
as a *separate* static archive, and libduckdb-sys's static link pulls in only
`libduckdb_static.a` — a `DUCKDB_STATIC=1` build then fails at query time with
`Scalar Function with name "now" is not in the catalog`. The shared library
has those built in, and the full suite passes against it.

Why not the system package: `duckdb`'s crate version encodes the engine its
bindings target (1.20001.0 = DuckDB 2.0.1), while Void's `duckdb-devel` is
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

From 2.0.0 to 2.0.1 (2026-11-27), measured the same way: DuckDB 2.0.1 reads a
file DuckDB 2.0.0 wrote, and after a 2.0.1 write plus `CHECKPOINT` 2.0.0
still reads it. A machine moving to 2.0.1 runs
`scripts/install-duckdb-prefix.sh` again; `~/opt/duckdb/current` then
points at it.

The extensions (`plugins/*`) build for `wasm32-wasip2`: install the
target once with `rustup target add wasm32-wasip2`. No WASI C toolchain is
needed. Each is a workspace member whose crate is empty for any other
target; the root package builds for the host alone.

Blitz is a beta, pinned to one release in the root `Cargo.toml`, and
patched past it there (`[patch.crates-io]`) only while a fix Horizon
needs is not released. What Horizon needs of the window
beyond `blitz-shell` is done around it, not in a copy of it:
`src/application.rs` wraps its application and holds copies of its
`View::redraw` and `View::complete_resume` at the pinned version. A
Blitz bump moves the pin (and a patch's revision, when there is one)
together, compares those copies with the new version's, and moves the
workspace's `taffy` to the version the new Blitz takes: `horizon-ui` rounds blitz-dom's layout again to the
display's pixels with it (`crates/horizon-ui/src/display_scale.rs`), and
its types must be blitz-dom's own.

Every worktree builds into its own `target/` — nothing is shared
between checkouts at the artifact level, so cross-worktree correctness
is trivial. What makes a fresh worktree fast is **kache**, a machine's
own install rather than this repository's: a `rustc-wrapper` that
caches compiler invocations by their inputs and restores their outputs
into each worktree's own `target/`, hitting across worktrees at
different paths, build-script crates included. Its default
`seed_new_targets` also copies registry and git dependency units — never
workspace members or path dependencies — into a target that has not
built yet. Once per machine:

```sh
cargo install kache --locked
kache init
```

`kache init` sets `rustc-wrapper = "kache"` in `~/.cargo/config.toml`
(and a Cargo shim and a shell line; see kache's own docs). Without it
Cargo builds as it always did, each worktree from scratch. Measured
2026-11-21 on a fresh worktree of main, the gate's four builds (the
workspace, the extensions, clippy, the tests) took about 230-270 s with
a warm cache, against about 425 s with the reflink seed hook that
preceded it, which never held clippy's or the tests' dependency units.
On this machine's `noatime` volume kache cannot tell which units no
build uses, so a long-lived `target/` keeps growing until it is cleaned.

The build-sharing schemes before this — a shared build-dir, sccache, the
reflink seed — and why each was retired are in `.cargo/config.toml`'s
comment. The one sharing setting that file holds is kache's
`KACHE_BUILD_SCRIPT_HERMETIC`: a build script's run is kept once in
kache's cache, read-only, and each target links to it, so a new worktree
does not clone and build the terminal engine again; a target built with
it needs `cargo clean` before a build without kache. If phantom errors
(E0432 on symbols that exist, a sibling's binaries running as yours)
ever appear, build with `KACHE_DISABLED=1` to tell kache's restores
from the code.

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
path-resolution owner. `horizon-agent-acp` depends on it directly, and
`horizon-agent` takes resolved named provider values as plain arguments
rather than parsing the file itself (see that crate's `config` module doc).

## GUI Verification

Agents cannot see the window, and must not open one on a desktop a person
is using. A view is checked without a window: the tests under `tests/`
build a view (or the whole shell) into a document, feed it the document's
own input events, and assert on the document; a test that is about what is
drawn paints the page into pixels on the CPU and reads them
(`tests/terminal.rs`, `tests/workspace.rs`,
`crates/horizon-components/tests/components.rs`). The window
itself does not open on a virtual display (Vello's window renderer finds
no adapter that can present there), so what only a window shows is looked
at by a person.

The display's scale matters to what is drawn: a test about pixels at a
scale other than one sets the document's viewport to it and paints at it
(`block_art_has_no_seams_in_either_pane_of_a_split_at_a_fractional_scale`).

`tests/terminald.rs`, `tests/agent_acp.rs` and `tests/board_logd.rs` run the
shell over the real runtimes, each started for the test with a data
home of its own (the agent's provider is the local mock
`scripts/mock-openai-provider.py`): a session created and typed into, the
workspace coming back after the shell is closed, a runtime reloaded.

What the agent itself does with a build -- its tools, the sandbox, the
approvals, with the real provider -- is seen without the running Horizon
and without restarting it: `cargo build --workspace`, then
`./target/debug/horizon-agent-probe --dir <directory> "<prompt>"
["<prompt>"...]` starts the `horizon-agent-acp` beside it as its child, with a data
home of its own, runs one session there, and prints every message, tool
call and approval as a JSON line (approvals are denied unless `--approve`;
`crates/horizon-agent-probe/src/main.rs` says the rest). It reads the
user's `config.toml`, and a session that enters a worktree makes it in the
real repository, ending it as a terminated session does.

An extension -- a view built as a wasm component with its manifest in
it, written as one module under `horizon_ui::plugin` (`plugins/markdown`),
and either one Horizon comes with or one named by its file in
`config.toml`'s `extensions` (`src/extension.rs`) -- is checked through a
pane of the shell (`tests/extension.rs`, `tests/wiki.rs`, in the gate).
One still being written is shown to a person in a preview pane of the
running Horizon, from its component's file: `horizon new preview
<extension.wasm> <arguments>` (`docs/preview-pane-design.md`), which reads
the file again whenever it is built again. That is how an agent asks a
person to look at a view; the components' gallery (`plugins/gallery`) is
looked at the same way. A preview pane lends an extension nothing beyond
the document, so one that asks for the sessions or the theme is tried in
place of the one of its name instead: `horizon extension use
<extension.wasm>`, until `horizon extension reset` or a restart
(`docs/cli-control-plane-design.md`). What the host does for a guest is tested through
the preview pane with the fixture extension (`plugins/fixture`,
`tests/preview.rs`). A view of the shell's own (the board, the terminal,
the agent) is not shown this way.

Manual smoke after `cargo run`: press `ctrl+'` to enter workspace mode
(`docs/workspace-mode-design.md`), then `:` to open the control surface —
a Commands-only palette. An empty (zero-tab) workspace is an implicit
command surface: `:` works directly there, no `ctrl+'` first, and it
shows the view chooser, which what is typed searches. Session
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
`crates/horizon-components` is what several views draw the same (chip,
button, field, the controls, Markdown as elements), over `horizon-ui`
alone;
`crates/horizon-theme-css` is the theme and the type scale as CSS
variables. Domain logic lives in shared crates; `src/` holds the views and
their wiring:

- `workspace/` — the shell root (`Shell`): draws the shared workspace model
  (one tree of panes, splits and tab groups, each group with its strip --
  `docs/workspace-layout-design.md`), keeps the sessions (the close-vs-detach
  seam), workspace mode, the key map, command dispatch (`execute`), the
  sheet over the workspace (`over.rs`) and what the shell puts in it
  (palette, view chooser, model picker; an extension puts its own view
  there, as the session manager is), the
  control plane's requests, and the runtime reloads. The model itself —
  the workspace's tree, panes, session attachments, mode state, the pure
  command model, the `workspace_snapshot` payload — is
  `crates/horizon-workspace`.
- `terminald.rs` / `agent_acp.rs` — the two runtimes as the shell holds them:
  a handle that a reload replaces (terminald's), and the agent's child. The clients themselves are
  `crates/horizon-shell-runtime`, with no UI framework in them (see
  `docs/terminald-split-design.md` for why reloading one must not disturb
  the other).
- `terminal/` — the terminal view: the session's frames painted by a
  widget of its own (grid, box and block characters as shapes, the cursor,
  the selection), key/mouse/IME handling, local scrollback. PTY ownership
  lives in `crates/horizon-terminald`; emulation and the session loop in
  `crates/horizon-terminal-core`, over libghostty-vt, whose encoder also
  writes the keys.
- `agent/` — the agent view and its live session. The fold from ACP
  session updates to what the pane shows is `horizon-shell-runtime`'s
  `agent_model`, `agent_view` and `agent_display`. Contract/providers/tools/persistence
  live in `crates/horizon-agent`; the sessions are hosted by
  `crates/horizon-agent-host` (with the board and the wiki composed in as
  extensions, `crates/horizon-board-flow` and `crates/horizon-wiki-flow`),
  served by `crates/horizon-agent-acp` over its standard input and output — see
  `docs/acp-agentd-design.md` and `docs/agent-stdio-child-design.md`.
- `board/` — the task board as two session-less views, the list and the
  thread, over `crates/horizon-board`; what they decide from, with no
  UI in it, is beside them (`model.rs`, `activity.rs`, `chrome.rs`,
  `measure.rs`). See `docs/board-redesign-design.md`.
- `finder/` — a file found by typing: the list, the matching, and the
  listing of a directory's files off the UI thread.
- `watch.rs` — the watch on the one file a view shows, which a preview,
  the extensions' loading and what the host watches for an extension
  share. The Markdown view (`horizon new markdown <path>`), the wiki
  (`horizon new wiki [<page>]`; a page of the project wiki,
  `crates/horizon-wiki`, with the owner's comment threads anchored in
  it, `docs/project-wiki-design.md`), the theme settings
  (`horizon new theme-settings`) and the list of sessions
  (`horizon new sessions`, and over the workspace by its own command,
  `Manage Sessions`), the image viewer (a `file://` link to an image
  followed in a terminal, `docs/image-viewer-design.md`) and the status
  bar (at the bottom of the window, `docs/status-bar-design.md`) are not
  here: they are extensions, whole in `plugins/markdown`, `plugins/wiki`,
  `plugins/theme-settings`, `plugins/sessions`, `plugins/image` and
  `plugins/status-bar`, over `horizon-ui` and `horizon-components`.
- `preview.rs` — the preview pane: an extension still being written,
  shown from its component's file. See `docs/preview-pane-design.md`.
- `control.rs` — the bridge from the CLI control plane's listener to the
  UI thread; the transport is `horizon-control::host`, the client is
  `horizon <subcommand>` itself (`crates/horizon-cli`). Panes get
  `HORIZON_SOCKET`/`HORIZON_SESSION_ID` in their environment. See
  `docs/cli-control-plane-design.md`.
- `restore.rs` — the workspace kept from the last run, as far as the
  runtimes still have its sessions.
- `application.rs` — the window's event loop: `blitz-shell`'s
  application, wrapped for what Horizon needs of the window that it does
  not do (the frame laid out by `horizon-ui`, the input method's cursor
  area, the window's focus, the primary selection).
- `desktop_notify.rs` / `window.rs` / `primary.rs` — what reaches the
  desktop: notifications, whether the window has the focus, the primary
  selection.
- `background.rs` / `wake.rs` — work done off the UI thread, and what
  wakes the page to take its answer in.
- `lib.rs` / `main.rs` — the root package is a library plus a thin binary:
  `lib.rs` declares the modules; `main.rs` is the CLI-vs-window entry.

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
  (`src/workspace/commands.rs`), the commands of the plugin a pane shows
  (`src/plugin.rs`'s `Plugin::commands`, run on the cursor's pane), or an
  extension's own commands, which need no pane. `Shell::commands` lists
  the three as one and `Shell::run` carries one out: buttons, keyboard
  shortcuts, the palette and the command line are bindings to that list —
  do not add ad-hoc behavior in UI handlers.
- **A view's keys are registered with `El::keys`.** Each key with what it
  does (`horizon_ui::keys::Key`), once on the element that hears it: the
  bar at the bottom of the window says the keys of the elements the focus
  is in, from that registration (`docs/status-bar-design.md`). A key a
  `keydown` handler of its own hears is not said there.
- **Close vs. terminate are distinct.** Closing a pane/tab detaches sessions;
  `Terminate Active Session` is the explicit destructive command. Preserve
  this separation (see `docs/ux-principles.md`).
- **A view's CSS is a CSS module.** `mod styles { horizon_ui::css_module!
  {"…"} }` beside the view: plain CSS whose classes become constants
  (`el("div").class(styles::ROW)`), with names no other sheet has. The
  macro refuses what is not written from the theme (a color or a type
  size written out, a variable the theme does not have, a property CSS
  does not have), and what would be over more than the view's own
  elements (a rule whose selector names no class of the sheet, an
  `@layer`): every view's sheet, an extension's too, is in the one
  document. The theme's variables are listed in
  `crates/horizon-theme-css/variables.txt`. What several views draw the
  same is a component in `crates/horizon-components` (chip, button, field,
  the controls), and its `gallery` shows them all:
  `HORIZON_GALLERY_OUT=<file.ppm> cargo nextest run -p horizon-components
  --test components --run-ignored only paint_the_gallery` paints it in
  this machine's theme, and `horizon new preview
  target/wasm32-wasip2/debug/horizon_gallery_plugin.wasm` shows it in a
  pane.
- **Tests are colocated** next to the code in the crates (an external
  `tests.rs` declared via `#[cfg(test)] mod tests;`, or an inline
  `#[cfg(test)] mod tests { ... }` block). The root package's views are
  tested from `tests/`, one file per view, through the document.
- **A test's own values reach the code as arguments; an environment
  variable only crosses a process.** A limit a test shortens, or a step
  it holds until it lets it go, is passed in where the code is built or
  opened (`Opening::read_extensions`, `WriterHandle::open_silently`'s
  `before_rebuild`, `AgentdHandle::start_within`'s `Timeouts`, the
  preview's `Limits`), not set with `std::env::set_var` in the test's
  process. Only a process the test spawns (a daemon) is reached through
  its environment, and its own binary turns the variable into the
  argument (`HORIZON_AGENT_TEST_*_GATE` in `horizon-agent-acp`'s
  `main.rs`). Owner decision 2026-11-20.
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
