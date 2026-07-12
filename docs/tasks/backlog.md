# Backlog — small known issues, not yet missions

Discovered during dogfooding; promote to a numbered mission when picked up.

1. *(resolved 2026-07-06)* **Palette-open key mismatch** — resolved by
   retiring the global chord outright rather than picking a winner between
   the mismatched candidates: workspace mode shipped with `:` as the
   palette's one-key resident (`docs/workspace-mode-design.md`), so
   `open-palette`'s built-in default binding is gone (the `[keybindings]`
   mechanism for an explicit override is unchanged). Status-bar text now
   renders the actual configured workspace-mode chord instead of a
   hardcoded hint, and the headless smoke scripts move to the mode-based
   flow (`ctrl+'` then `:`) in the same change, un-breaking the
   `new-terminal-focus` / `split-pane` scenarios.
2. *(resolved 2026-07-06)* **Insert and F1-F24 send nothing** — fixed by
   wiring both into `app::keymap::terminal_key_from_input` (`NamedKey::
   Insert`/`F1`..`F24` -> `TermKeyCode`), so they now route through the
   terminal's live Kitty state exactly like arrows/Home/End already did.
   Insert and F1-F12 already had spec-legal legacy forms (reused
   unconditionally, matching the Kitty spec's own alternate numeric forms
   for F1-F12); F13-F24 had none (the spec's own "Functional key
   definitions" table gives them only Private-Use-Area `CSI u` codepoints),
   so `kitty_keyboard::kitty_override` now promotes them to their dedicated
   PUA codes (`57376`-`57387`) once any Kitty flag is negotiated, while
   keeping termwiz's existing xterm/rxvt-style legacy numbers when no flag
   is active at all — a deliberate, documented deviation from kitty's own
   reference (which always emits PUA codes for these, even at zero flags),
   chosen for legacy-program compatibility and explicitly permitted by the
   spec text itself. F25-F35 remain both unimplemented (no PUA table entry)
   and unwired at the app layer, matching this item's own original scope
   (`KITTY_COMPLIANCE`'s "Functional key definitions: F25-F35" row).
3. *(resolved 2026-07-06)* **Kitty event types for navigation keys** —
   fixed by a new `kitty_keyboard::navigation_key_event_override` that
   decorates arrows/Home/End/PageUp/PageDown/Insert/Delete's own legacy
   `CSI` forms with the same modifiers:event-type sub-field the `CSI u`
   forms already carried, once `REPORT_EVENT_TYPES` is negotiated — verified
   against the spec's own generic "sub-field of the modifiers field"
   wording (not `CSI u`-specific) plus two real implementations the spec
   names (kitty's own `key_encoding.c` and alacritty's `keyboard.rs`, both
   of which decorate every functional key uniformly regardless of
   terminator), so this lands as `Compliant` in `KITTY_COMPLIANCE`, not a
   deviation from the earlier, narrower reading.
4. *(resolved 2026-07-06)* **OSC 52 clipboard write** — wired
   `Event::ClipboardStore` (`core::events::EventSink::send_event`) through a
   new `TerminalEvents::clipboard_writes: Vec<String>` field, drained
   alongside `title`/`bell_count` in both of `session::runtime`'s
   event-processing arms (`pty_rx` and the sync-update failsafe) and
   forwarded as the *existing* `TerminalUpdate::Clipboard(String)` variant
   the interactive selection-copy path already produces (`SelectionCommand::
   Copy`) — same downstream handling in `app::runtime::terminal`
   (`floem::Clipboard::set_contents`, plus the `HORIZON_CLIPBOARD_DUMP` test
   hook), no new contract variant needed. Write-only by design: `TerminalCore
   ::new` now sets `osc52: Osc52::OnlyCopy` explicitly (matches
   alacritty_terminal's own default, but spelled out so the security
   decision is visible at the call site rather than resting on an upstream
   default) — a query (`Event::ClipboardLoad`) never reaches Horizon at all,
   since the parser itself refuses to emit one in that mode; letting a
   terminal app read the system clipboard is the standard OSC 52
   exfiltration hazard, so read access is refused outright rather than
   gated some other way. Both targets alacritty_terminal parses (`c`
   clipboard, `p`/`s` selection) land in the same `clipboard_writes` bucket
   uniformly: Horizon exposes one system clipboard, no separate primary-
   selection buffer, so there's nothing to distinguish. A capped size
   (`OSC52_CLIPBOARD_WRITE_CAP = 256 KiB`, `core/events.rs`) drops an
   oversized payload silently before it ever reaches the clipboard — no OSC
   52 "too large" reply exists to send back, matching how a real terminal
   just ignores a request it doesn't like. Tests: core-level event firing
   and cap enforcement (`terminal::tests::osc52_clipboard_write_*`,
   `osc52_clipboard_read_query_is_refused`), plus an end-to-end
   `run_terminal_core` test proving the channel wiring
   (`terminal::session::runtime::tests::
   run_terminal_core_forwards_osc52_clipboard_writes_as_updates`) — the real
   clipboard write itself (`app::runtime::terminal`) is outside this
   module's test boundary by construction, so no clipboard mock was needed.
5. *(resolved 2026-07-06)* **Focus events (CSI I/O)** — added
   `TerminalCore::focus_input(focused: bool) -> Option<Vec<u8>>` (mirrors
   `paste_input`'s bracketed-paste gate: `None` unless the attached app
   negotiated mode 1004/`TermMode::FOCUS_IN_OUT`, otherwise `CSI I`/`CSI O`),
   a new `TerminalCommand::Focus(bool)` routed through `session::runtime`
   exactly like `Mouse`/`Paste` (writer thread -> core thread -> back out as
   `TerminalCommand::Input` when the mode gate passes). The pane-focus
   signal source ended up needing no new hook into `workspace::view::pane`
   at all: rather than floem's raw per-widget `FocusGained`/`FocusLost`
   (which don't fire on window blur and would have needed a second signal
   just to track "which pane"), `app::runtime::wire_focus_reporting` reads
   the workspace's own already-reactive `active_visible_index`/
   `visible_terminal_session_id` directly, composed with a new
   `window_focused: RwSignal<bool>` in `AppState` (set from floem's
   `WindowGotFocus`/`WindowLostFocus` via two `AppInput` handlers). One
   `create_effect` diffs the previously-notified session against the
   current one on every change to either input, sending focus-out to the
   session that lost it and focus-in to the one that gained it — never both
   to the same session, nothing at all when unchanged. Composition rule
   (checked against kitty/ghostty): losing OS-level window focus reports
   focus-out for the active terminal even though no pane-internal focus
   changed, and regaining window focus reports focus-in again for whichever
   pane is still active. Tests: `terminal::tests::focus_input_*` (mode
   on/off), an end-to-end `run_terminal_core` test proving the command
   silently no-ops until mode 1004 is negotiated
   (`run_terminal_core_reports_focus_only_once_mode_1004_is_enabled`), and
   `app::runtime::tests::{switching_the_active_pane_sends_focus_out_then_
   focus_in, window_losing_focus_reports_focus_out_even_though_the_pane_did_
   not_change}` for the composition effect itself.
6. *(resolved 2026-07-06)* **agentd e2e flakiness under load** —
   verified gone: 5 consecutive `cargo nextest run -p horizon-agentd`
   runs all green, including the two historical flakes individually.
   Attributed to nextest's per-test process isolation plus the drain
   event-log flush fix.
7. *(resolved 2026-07-07)* **Theme roles lost two distinctions** — restored
   as dedicated roles rather than a shared `surface_accent_soft`-class one
   (each tint belongs to a specific, distinct piece of UI, not a general
   soft-accent surface): `user_message_surface`/`user_message_border`
   (the blue bubble, `agent::view::style::block_colors`'s `User` arm) and
   `approval_surface`/`approval_border` (the amber pending-approval
   transcript block, that match's `Approval` arm) — both back to their
   exact pre-regression colors (`Source agent transcript colors from the
   theme`'s diff). The approval banner's Approve/Deny button fills
   (`workspace::view::agent_controls`, previously hardcoded
   `Color::from_rgb8`) got their own `approval_confirm_surface`/
   `approval_deny_surface` roles alongside, and the message composer's
   remaining hardcoded colors were swept onto their already-existing
   matching roles (`accent`, `border_default`, `text_subtle`,
   `text_primary`, `surface_base`) in the same pass — no hardcoded colors
   remain in `agent::view`/`workspace::view::agent_controls`.
8. **ghostty multi-attach corruption** — deliberately deprioritized;
   captured evidence lives in the session transcripts (PTY traces under
   /tmp/horizon-pty-*.jsonl as of 2026-07-05).
9. **floem startup input gap (~0.5s)** — accepted regression of the git
   pin, compensated by `HORIZON_INPUT_SETTLE` in the verification
   scripts. Whether to report it upstream is the owner's own call and
   act, not something this repo's sessions do.
10. **Test knob for sync-update pump** — the 150ms failsafe constant is
    vte's; if TUIs ever need tuning here it should join `[terminal]`.
11. *(resolved 2026-07-06)* **bash tool truncation hides the head of long
    outputs** — ground truth turned out to be a 50/50 head+tail split whose
    head budget was eaten by compile spew, plus a spill path hidden in a
    JSON field. Fixed by skewing the split to tail 2/3 and inlining the
    spill path into the truncation notice.
12. **agentd leaks its `cargo run` environment into tool processes** —
    when Horizon is launched via `cargo run`, agentd (and thus every
    bash tool call an agent makes) inherits `CARGO_*`, `RUSTUP_*`, and
    an `LD_LIBRARY_PATH` pointing into `target/debug/build/...`
    (verified via `/proc/<agentd>/environ`, 2026-07-06). Harmless for
    cargo fingerprints (tested), but processes the agent spawns can
    resolve stale shared libraries from build dirs. agentd should
    sanitize these when spawning tool processes; launching the built
    binary directly instead of `cargo run` also sidesteps it.
13. *(resolved 2026-07-06)* **headless GUI verification writes into the
    real agentd state** — `check-terminal-visual.sh`/`run-terminal-smoke.sh`
    runs connected to the owner's real `horizon-agentd` socket and
    persisted throwaway test sessions into the real event log / DuckDB
    (observed at ~4k lines, plus replay cost on every reconnect). Fixed
    by making `check-terminal-visual.sh` hermetic by default: each run
    computes a scratch `XDG_RUNTIME_DIR` under its own artifact dir (the
    working recipe already noted below — `XDG_RUNTIME_DIR` isolates both
    the control socket and agentd, since a fresh agentd binds under it
    and `horizon_agent::socket::default_socket_path` is
    `$XDG_RUNTIME_DIR/horizon/agentd.sock`) and points
    `HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB` at scratch files
    alongside it, so a run never touches `~/.local/share/horizon` or the
    owner's real agentd connection slot. `run-terminal-smoke.sh` needed
    no changes: each scenario already gets its own `HORIZON_ARTIFACT_DIR`,
    so hermetic mode composes automatically per scenario. Since Horizon
    exiting doesn't kill agentd (sessions survive by design), the
    script's cleanup trap now also finds and kills *this run's* scratch
    agentd — identified by grepping for the run-unique scratch socket
    path in agentd's own `--socket <path>` argv, never a bare
    process-name kill — so hermetic runs don't leak an agentd per
    invocation. `HORIZON_REAL_RUNTIME=1` opts back into the real
    environment when needed. *Prior working recipe (2026-07-06):*
    overriding `XDG_RUNTIME_DIR` to a scratch dir isolates both the
    control socket and agentd (a fresh daemon spawns there) in one move —
    verified in a live CLI E2E. The missing piece for a per-knob fix is
    an agentd socket override (`HORIZON_AGENTD_SOCKET`, already
    anticipated as a code comment) — still not implemented; not needed
    since the `XDG_RUNTIME_DIR` recipe covers this fix.
14. **headless GUI scripts hung/failed under shared-machine load,
    2026-07-06** — every `check-terminal-visual.sh` scenario (including
    ones that don't touch workspace mode at all, and reproduced against
    unmodified `origin/main`) failed with `X_GetImage`/`BadMatch` from
    `xwd`; the Horizon window was created (visible to `xdotool search`)
    but never reached `IsViewable` (`xwininfo`'s Map State) even after
    60s+, with the process idle in `futex_do_wait` and no `horizon-agentd`
    child ever spawned, even pointed at a fully isolated scratch
    `HORIZON_SOCKET`/`HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB`.
    Observed on the owner's real desktop under heavy concurrent load
    (several other agent worktrees running their own Horizon/agentd
    instances and cargo builds at the same time) — likely a scheduling/
    contention effect on the software (`LIBGL_ALWAYS_SOFTWARE=1`)
    rendering path rather than anything in the app itself, but not root
    caused. Distinct from and more severe than item 13 (that one is about
    state pollution and replay cost on an otherwise-working connect; this
    one is the window never appearing at all). Revisit if it recurs
    outside a loaded shared machine.
    *Root cause identified (2026-07-06, later the same day):* the shared
    agentd accepts ONE connection at a time, and Horizon's startup
    connects to it synchronously before the first frame — while the
    owner's desktop instance holds that connection, any headless boot
    stalls pre-map with no agentd child spawned (exactly the recorded
    signature; `HORIZON_SOCKET`/event-log isolation can't help because
    the agentd socket path has no env override). Fix directions:
    (a) scratch `XDG_RUNTIME_DIR` per run (works today, see item 13),
    (b) add `HORIZON_AGENTD_SOCKET`, (c) product-level: make the
    startup agentd connect non-blocking so the UI maps even when agentd
    is busy or absent — agent panes can degrade gracefully instead of
    the whole window stalling.
    *(b) and (c) resolved (2026-07-06):* `horizon_agent::socket::
    default_socket_path` now resolves `$HORIZON_AGENTD_SOCKET` first
    (falling through to the existing `$XDG_RUNTIME_DIR`/`/tmp` rule when
    unset or empty) — shared code, so `horizon-agentd`'s own `--socket`-
    absent fallback and every Horizon-side client call honor it
    identically with no extra wiring on either side. `app::state::
    AppState::new` no longer blocks on the startup connect at all: the
    connect/handshake/`session_list` sequence
    (`agent::agentd_runtime::connect_agentd_at_startup_async`) now runs
    entirely on a background thread from the start (the same shape
    `reload_agent_runtime` already used for `Reload Agent Runtime`,
    factored into a shared `connect_and_discover_sessions` helper), with
    progress/outcome applied back through a `create_effect` callback.
    `agentd_connection` starts `None`; the window maps unconditionally,
    and any agent pane spawned before the connect resolves takes the
    pre-existing "Agent runtime unavailable" fallback frame/status —
    reused as-is, no new UI. Verified headlessly both ways: (1) a real
    `horizon-agentd` on a scratch socket with a dummy client holding its
    one connection slot open — Horizon's window still mapped and a
    terminal pane still worked, with `agent runtime: spawning…` visibly
    stuck in the status bar the whole time (via
    `scripts/check-terminal-visual.sh`); (2) the `horizon-agentd` binary
    entirely absent — window still mapped, terminal pane #1 unaffected,
    and a freshly opened agent pane showed the ordinary "Agent runtime
    unavailable -- use \"Reload Agent Runtime\" to reconnect." error
    frame. Full gate green (`cargo fmt`, `cargo clippy --workspace
    --all-targets -- -D warnings`, `cargo nextest run --workspace`: 672
    passed including all 19 `horizon-agentd` e2e tests).
15. **reload_agent_runtime's responder/status effects when invoked over
    the CLI** — a latent reactive-lifetime hazard sibling to the three
    fixed in the plan-03 E2E (detached-scope creation): documented in
    docs/agent-roles-and-skills-design.md but deliberately not fixed
    there. Flagged 2026-07-06 by the agent-foundation session.
16. **Turn metadata in agent frames** — the transcript's turn footer
    wants model id and turn duration, but the contract's
    ProviderRequest* events are timing markers that never reach the
    frame. Needs a small contract-level addition (agent-foundation);
    the UI receiving end is trivial. Proposed by application-ui
    slice 2 (2026-07-07).
17. **color-grid smoke fails on xdotool quoting/spacing** — pre-existing
    environment quirk, unrelated to the placement-first change (fails
    identically standalone); distinct from the backlog-14 Xvfb family.
    Reported by application-ui (2026-07-07).
18. **Web search tool** — give the agent outward web search (the "search
    tool" the owner originally meant, 2026-07-07). Needs its own
    consultation: provider choice (crush shells out to DuckDuckGo Lite
    HTML scraping = free but brittle; opencode uses Exa/Parallel over
    MCP endpoints = API key required; or a plain search API), the
    trust-boundary/network-access approval design, and whether it sits
    behind a crush-`agentic_fetch`-style throwaway subagent (one outer
    approval, inner search/fetch chain) — a shape close to Horizon's
    delegation + skill mechanism. See docs/research on crush/opencode
    tools (in the session transcript, not yet a doc).
19. **Public-code / symbol search** — crush exposes `sourcegraph`
    (public GitHub via Sourcegraph GraphQL, no API key) and
    `lsp_references` (LSP-backed symbol references); opencode has an
    experimental `lsp` tool (default off). Separate discussion from web
    search — LSP integration is a larger commitment (language-server
    lifecycle) and overlaps with future viewer/plugin work. Recorded
    2026-07-07.
20. **Live PTY hand-off across a sessiond binary update** — keeping a
    terminal session's PTY master fd and child processes alive while
    the session daemon's binary is replaced (execve re-exec or
    systemd-style socket-activation fd passing). Deliberately split out
    of the session-daemon migration (consultation 2026-07-07, agenda
    item 5): UI-crash survival — the actual motive — is already met by
    sessiond being a separate process, so this is an independent,
    heavier capability (a reliability requirement agentd's drain has
    never proven). First migration form accepts "sessiond reload
    terminates terminal sessions; agent sessions restore from the log."
    See docs/research/session-daemon.md §2.E.
21. *(resolved 2026-07-08)* **Dead `TranscriptTone::Status` match arms** —
    after leg 1 moved the status line out of the block pipeline into
    `status_indicator_view` chrome (`c4e3478`), `style.rs`'s
    `block_label_size`/`block_text_color`/`block_colors` still carried
    explicit `TranscriptTone::Status` arms that the `dyn_stack` never
    reaches any more (the variant is still constructed by `status_block`,
    just not rendered as a block); those three arms are now removed,
    falling through to each function's existing catch-all (`block_colors`
    had none, so it gained one). `labels.rs`'s `shows_label`/`block_label`
    turned out to have no literal `TranscriptTone::Status` arm to begin
    with (`shows_label` is a boolean expression over `User`/`Assistant`
    only; `block_label` matches on `BlockKind`, not `tone`) — no change
    needed there.
22. **Airtight in-place mutation tracking (reducer reports the mutated
    index)** — leg 1's `in_place_mutable_item_indices`
    (`crates/horizon-agent/src/frame.rs`) is a stopgap: it re-derives the
    small set of indices a next fold *could* mutate, correct for every
    real-provider sequence but with two documented latent gaps (concurrent
    multi-key `ToolCallPreparing` byte counts; and its growth-branch
    correctness resting on one-event-per-fire delivery, no `batch()`). The
    airtight form has `apply_agent_event_to_frame` report exactly which
    index it mutated/appended, threaded to the bridge as the single source
    of truth — removing both gaps. Wants doing before any change that
    batches frame delivery or a provider that interleaves tool-arg
    streaming. Recorded 2026-07-08 (leg-1 review Observation + design doc).
23. *(resolved 2026-07-09)* **OSC palette-override narrowing (reclaimed in
    daemon migration)** — Foundation 4's color cut (`10eae86`,
    session-daemon-design.md decision 8) moved cell-color resolution
    UI-side, and as a side effect a running program's live OSC 4/10/11/12
    palette overrides stopped reaching cell rendering (only the crate's own
    OSC query *replies* still honored them). Owner asked to resolve it in
    the same work rather than defer to the daemon slice. Closed by `45acf81`:
    `TerminalFrame::palette_overrides` carries the override table as a sparse
    logical-index → literal-RGB list, populated from `Term::colors()` at
    snapshot time and consulted by `terminal::view::color::resolve_color`
    before the theme (a literal override wins for its slot; the theme governs
    only non-overridden slots — coherent with decision 8's per-client
    theming). The incremental row-diff in `set_state` is bypassed with a full
    rebuild when the override table changes, so an app repainting its palette
    onto an already-drawn screen actually recolors. Forward-compatible with
    sessiond: the table rides the frame and will cross the wire unchanged.
24. **Composer IME candidate-window placement** — the multi-line wrapping
    composer (`44f2dd7`) still positions the IME preedit/candidate window
    at a fixed `Point::new(10.0, 6.0)` in `agent_controls.rs`, inherited
    from the single-line composer. With wrapping and multi-line drafts the
    caret is rarely there, so the candidate window detaches from the actual
    insertion point — more visibly wrong than before. Fix: track the
    caret's `hit_position` from `composer_text.rs`'s `TextLayout` (the same
    hit the caret rect already uses) and feed it to the IME position. Small,
    self-contained; deferred from the composer fix to keep that scope to the
    two reported bugs. Recorded 2026-07-09.
25. **`Reload Config` live re-theme doesn't recolor already-drawn terminal
    rows** — the twin of the OSC-override repaint gap fixed in `45acf81`
    (backlog 23). Terminal cells carry logical colors (decision 8);
    `terminal::view::layout::build_span_cells` resolves them against
    `resolved_colors()` only when a row is (re)built, and the incremental
    `update_line_layouts` skips rows whose `TerminalLine` is unchanged. A
    palette *override* change now forces a full rebuild via `set_state`, but
    a *theme* change (`Reload Config`) arrives on a different path (a config
    event, not a frame diff) and nothing invalidates the view's row cache, so
    a static screen keeps its old RGB until content changes. Already noted in
    session-daemon-design.md ("Reload Config does not push a live theme update
    into already-spawned sessions"). Fix: on theme reload, invalidate the
    terminal view's cached rows (clear + full rebuild, the same move
    `set_state` makes for palette_overrides) so the live re-resolve decision 8
    promised actually happens. Small, self-contained. Recorded 2026-07-09.
26. **[RESOLVED 22a4f47] "Terminate detached session" tore down the ACTIVE
    agent pane** — the owner ran Terminate on a detached session in the
    session-manager modal and it also terminated the live/active agent.
    Actual root cause was not a `session_id` mismatch but the **selection
    index going stale after the list mutates**: `ConfirmTerminate` derived
    its target from the currently-highlighted *row index*, and a background
    change to the session list (a session detaching/attaching) shifted the
    rows under a fixed index, so the highlighted row — and thus the killed
    session — was no longer the one the owner had selected. Fix (`22a4f47`):
    bind terminate to session *identity*, not index — a `selected_id:
    RwSignal<Option<SessionId>>` tracks the chosen `SessionId`, a pure
    `terminate_target(items, pending)` dispatches that id (cancelling if it
    has vanished from the list), and a `reanchor_selection` effect keeps the
    highlight following the same `SessionId` across list changes. The
    id-targeting hypothesis below was the pre-investigation guess; kept for
    the record. The terminate machinery
    itself is correctly id-targeted (`session_manager.rs` dispatches
    `TerminateSession { session_id: row.session_id }`; `Workspace::
    terminate_session` removes only that id and detaches only panes whose
    `session_id == Some(that id)`), so the fault is almost certainly upstream
    in WHICH `session_id` the "detached" row carries — i.e. the active pane's
    own agentd session being surfaced as a detached row. `attach_sessions`
    calls `register_detached_session(PaneKind::Agent, session_id)` for every
    id in agentd's `session_list` (`src/agent/agentd_runtime.rs`); a mismatch
    between the `SessionId` a live pane holds and the id agentd reports (a
    pane created before connect vs. the id agentd resumed, or an
    `agent::SessionId`↔`session::SessionId` conversion seam) would list the
    active session as detached, so terminating that row tears down the live
    pane. Also rule out a stale session-manager selection index after the
    list mutates. Start by logging `detached_session_summaries()`'s id set
    against the active pane's session id at terminate time. Separate from the
    bash-approval wedge (backlog: the registry panic-safety fix). Recorded
    2026-07-09.
27. **`horizon-agentd` respawn/replay e2e tests flake under the full parallel
    nextest run** — `killed_agentd_respawns_and_replays_transcript_with_open_
    turn_cancelled` and `drained_agentd_respawns_and_preserves_a_completed_
    session` (`crates/horizon-agentd/tests/e2e.rs`) intermittently fail with
    `the pre-crash user message must survive replay, got: []` when run inside
    the whole `cargo nextest run --workspace` suite, yet pass deterministically
    in isolation and on retry. The empty replay suggests a respawn races the
    transcript flush (or a shared-resource contention — sockets, DuckDB
    projection rebuild, the real gpt-4o-mini call) when many e2e tests run
    concurrently. Disruptive because it fails the pre-commit integration gate
    for unrelated (e.g. docs-only) commits — hit exactly that during the
    backlog-26 doc commit 2026-07-09. Fix options: make the replay assertion
    wait for the flush to settle (poll with a timeout rather than reading once),
    or serialize these respawn e2e tests via a nextest test-group so they don't
    contend. Recorded 2026-07-09.
28. **`horizon-sessiond` socket e2e flakes under the full parallel nextest
    run** — `terminal_create_diff_reconnect_attach_and_shutdown_over_the_
    real_socket` (`crates/horizon-sessiond`, e2e) intermittently fails on a
    ~10s timeout inside `cargo nextest run --workspace`, yet passes
    instantly (~0.08s) standalone and on full-suite re-runs. Hit 3-4 times
    on 2026-07-12 across worker gates and integration-merge gates; an early
    "another live Horizon instance caused contention" theory was
    disproven when it fired with no concurrent instance running. Same
    shape as backlog-27 (agentd respawn e2e flakes): parallel-suite
    resource contention around real sockets, with the same fix options —
    poll-with-timeout instead of a fixed deadline, or a nextest
    test-group serializing the socket e2e tests. Disruptive for the same
    reason: it fails integration gates for unrelated merges. Recorded
    2026-07-12.
29. **Git dependencies carry no `rev` pins in Cargo.toml — Cargo.lock is
    the only pin** — the root `Cargo.toml` declares `gpui`, `gpui_platform`,
    `gpui-component`, and `gpui-component-assets` as bare `git = ...` deps
    with no `rev`/`tag`. Any `cargo generate-lockfile` / `cargo update`
    therefore silently re-resolves every git dep to its current HEAD, as
    observed during the gpui-ce drop-in spike
    (`docs/research/gpui-ce-drop-in-spike.md` §3): non-patched zed crates
    jumped from the `5f8a741` pin to zed's 2026-07-12 HEAD mid-experiment.
    Given the known rev×toolchain coupling pain (gpui-migration-design.md's
    termy/pathfinder note), pin explicit `rev =` values in Cargo.toml so
    the pin survives lockfile regeneration and is visible in review diffs.
    Small mechanical task. Recorded 2026-07-12.
