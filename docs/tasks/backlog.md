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
8. *(closed 2026-07-18 — long-deprioritized and the captured evidence is
   gone from /tmp; re-file via docs/issues/ if it recurs)*
   **ghostty multi-attach corruption** — deliberately deprioritized;
   captured evidence lives in the session transcripts (PTY traces under
   /tmp/horizon-pty-*.jsonl as of 2026-07-05).
9. *(closed 2026-07-18 — moot: the Floem shell and its startup gap
   retired with the GPUI migration)*
   **floem startup input gap (~0.5s)** — accepted regression of the git
   pin, compensated by `HORIZON_INPUT_SETTLE` in the verification
   scripts. Whether to report it upstream is the owner's own call and
   act, not something this repo's sessions do.
10. *(closed 2026-07-18 — speculative knob; never needed in 12 days of
    dogfooding)* **Test knob for sync-update pump** — the 150ms failsafe constant is
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
15. *(closed 2026-07-18 — moot: the hazard was floem_reactive scope
    lifetime; the GPUI shell's reload path shares none of it)*
    **reload_agent_runtime's responder/status effects when invoked over
    the CLI** — a latent reactive-lifetime hazard sibling to the three
    fixed in the plan-03 E2E (detached-scope creation): documented in
    docs/agent-roles-and-skills-design.md but deliberately not fixed
    there. Flagged 2026-07-06 by the agent-foundation session.
16. *(resolved 2026-07-12)* **Turn metadata in agent frames** — the
    transcript's turn footer wants model id and turn duration, but the
    contract's ProviderRequest* events are timing markers that never
    reach the frame. Resolved as stage A of the turn-receipts work:
    `Event::TurnEnded` now folds into a new `AgentFrameItem::TurnEnded
    { reason, model, elapsed }`, with `model` derived from the turn's
    most recent `ProviderRequestSent` and `elapsed` from a reducer-side
    `TurnClock` sidecar (exact for a live fold, a near-zero
    approximation for cold replay — see `docs/agent-output-ui-
    amendment.md`'s Contract addendum for the trade-off). No UI change;
    the receiving end (turn receipts, running-card footer) is the next
    stage.
17. *(closed 2026-07-18 — moot: the color-grid smoke script retired with
    the Floem-era verification scripts; scripts/ now holds only the GPUI
    checks)* **color-grid smoke fails on xdotool quoting/spacing** — pre-existing
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
20. *(closed 2026-07-18 — owner decision: heavy independent capability;
    the motivating need, UI-crash survival, is already met by the
    sessiond process split)*
    **Live PTY hand-off across a sessiond binary update** — keeping a
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
22. *(closed 2026-07-18 — moot: the floem bridge consumer is gone with
    the GPUI migration; `in_place_mutable_item_indices` survives only as
    dead code plus its own tests — removal queued as refactoring)*
    **Airtight in-place mutation tracking (reducer reports the mutated
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
24. *(closed 2026-07-18 — moot: the Floem composer and `agent_controls.rs`
    retired with the GPUI migration; IME placement is gpui-component
    `Input`'s own concern now)*
    **Composer IME candidate-window placement** — the multi-line wrapping
    composer (`44f2dd7`) still positions the IME preedit/candidate window
    at a fixed `Point::new(10.0, 6.0)` in `agent_controls.rs`, inherited
    from the single-line composer. With wrapping and multi-line drafts the
    caret is rarely there, so the candidate window detaches from the actual
    insertion point — more visibly wrong than before. Fix: track the
    caret's `hit_position` from `composer_text.rs`'s `TextLayout` (the same
    hit the caret rect already uses) and feed it to the IME position. Small,
    self-contained; deferred from the composer fix to keep that scope to the
    two reported bugs. Recorded 2026-07-09.
25. *(resolved 2026-07-15, no code fix needed)* **`Reload Config` live
    re-theme doesn't recolor already-drawn terminal rows** — this item's
    original analysis (recorded 2026-07-09, the same day as backlog 23's
    `45acf81`) described the Floem shell's row cache
    (`terminal::view::layout::build_span_cells`/`update_line_layouts`/
    `set_state`). That shell was retired two days later (`04d9f0e`,
    2026-07-11); re-verified against today's GPUI-only paint path
    (`src/terminal/mod.rs::paint_terminal`) and the bug no longer exists
    there. `paint_terminal` is a fully immediate-mode repaint: it calls
    `theme::resolve`/`theme::to_hsla` fresh for every visible span on
    every invocation, straight off `scheme()` (a plain `RwLock` read) —
    no row/span cache sits between them. Its `canvas()` element has no
    `ElementId` (`gpui::elements::canvas::Canvas::id` returns `None`), so
    it can never be reuse-painted, and `TerminalView` itself (a `Render`
    impl) has its own prepaint-cache bypassed whenever `window.refreshing`
    is set (`gpui::view::AnyView::prepaint`'s cache-key check), which is
    exactly what `Window::refresh()` sets — already called by
    `CommandId::ReloadConfig` (`src/workspace.rs`) right after
    `theme::reload_from`. So `Reload Config`'s existing `window.refresh()`
    call is already sufficient; there is nothing left to invalidate.
    Regression guard added: `theme.rs`'s
    `resolve_reflects_a_reload_immediately_with_no_separate_cache_to_invalidate`
    asserts `resolve()` (the exact function `paint_terminal` calls) picks
    up a `reload_from` change on the very next call, with no intervening
    step. The `session-daemon-design.md` note this item cited is about a
    different, still-real gap (`TerminalCoreOptions`'s OSC 4/10/11/12
    query-reply defaults, sessiond-core-side) — left as is, out of this
    item's scope.
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
27. **[RESOLVED 5c3f725] `horizon-sessiond` respawn/replay e2e tests
    flake under the full parallel nextest run** — Post-sessiond-merge names:
    `killed_sessiond_respawns_and_replays_transcript_with_open_turn_cancelled`
    and `drained_sessiond_respawns_and_preserves_a_completed_session`
    (`crates/horizon-sessiond/tests/e2e.rs`; this entry originally named the
    pre-merge `horizon-agentd` crate/test names). Root cause found: a real
    product race, not just test flakiness. `Control::SessionList`/
    `SessionLoad`'s readiness gate (`SessiondState::mark_resume_ready`) only
    proves a resumed session's `SessionEntry` exists in the session map —
    not that its dedicated OS thread has reached the loop that answers a
    replay request. That thread does real work first, including blocking on
    `SessiondState::wait_for_duckdb_store()` (a DuckDB rebuild-or-open wait
    that is deliberately *not* ordered against the readiness gate). Under
    parallel-suite contention that wait can genuinely exceed the old 5s
    `REPLAY_TIMEOUT` (`crates/horizon-sessiond/src/session.rs`), which
    silently defaulted to an empty `Vec` on timeout — indistinguishable from
    a genuinely empty session — producing exactly the observed `got: []`.
    Fixed by raising `REPLAY_TIMEOUT` to 60s (a rare-to-ever-hit safety net,
    not a hot path) and by fixing the test harness's own
    `collect_replayed_events`, which independently had a 500ms
    quiescence-window read *on its first read too* — tighter than even the
    old server-side budget — now split into a generous first-read timeout
    (60s, covering real contention) followed by the original tight 500ms
    quiescence window for the rest of the burst (events after the first
    arrive back-to-back with no material gap). Also added a nextest
    test-group (`.config/nextest.toml`) serializing the whole
    `horizon-sessiond::e2e` binary (`max-threads = 1`) as belt-and-braces
    against self-contention, given the repeated merge-tax history.
28. **[PARTIALLY RESOLVED 5c3f725, e478e6e] `horizon-sessiond` socket e2e
    flakes under the full parallel nextest run** — `terminal_create_diff_
    reconnect_attach_and_shutdown_over_the_real_socket` (`crates/
    horizon-sessiond/tests/e2e.rs`) spawns a real PTY backed by a real
    interactive shell (`/bin/sh -i`). Under **realistic** load -- a plain
    `cargo nextest run --workspace` with no extra synthetic stress, the
    actual shape of the original flake reports -- this is dramatically
    better but not literally zero: 30/30 clean runs across two rounds
    before the retry fix below, then 14/15 in a third round run after a
    follow-up correctness fix to that same retry (same failure signature:
    `terminal_create_diff_...` at exactly the 120s ceiling). One failure
    in 45 realistic-mode runs is a large improvement over the original
    "6+ times in one day" merge-tax rate, but the honest statement is
    "much rarer," not "eliminated." Under a **deliberately extreme**
    synthetic stress (a tight loop of `cargo build -p horizon --release`
    + `cargo clean`, sustained for many minutes, well beyond "another
    worker's live build") a residual failure rate persists -- roughly
    10-20% per run across several measurement rounds -- and traces to a
    genuine, well-evidenced (though not live-captured) upstream hazard
    rather than plain scheduling slowness: see the `portable-pty`
    fork-safety backlog entry. Two rounds of raising `read_terminal_
    update`'s fixed timeout (10s to 60s to 120s, `TERMINAL_UPDATE_TIMEOUT`
    in `tests/e2e.rs`) each got defeated by a failure landing at *exactly*
    the new ceiling -- the signature of a genuine stall, not merely
    "slower under load" -- which is what prompted the deeper
    investigation. Landed fixes: (1) the timeout raise above, generous
    for the realistic case; (2) a nextest test-group
    (`.config/nextest.toml`) serializing every test in the `horizon-
    sessiond::e2e` binary against each other (`max-threads = 1`), removing
    self-contention as a variable; (3) a production fix in `crates/
    horizon-sessiond/src/terminal.rs`'s `TerminalHost::create` --
    previously a stuck PTY spawn could wedge that connection's entire
    message loop forever (`Command::spawn` blocks its calling thread with
    no way to interrupt it); now each spawn attempt is bounded to 10s
    (`TERMINAL_SPAWN_TIMEOUT`) on its own thread with up to 3 retries
    before reporting a `TerminalUpdate::Error`, converting an unrecoverable
    freeze into a bounded, retriable failure; (4) a follow-up correctness
    fix to (3) caught in review: the original retry design let a late,
    abandoned attempt's success unconditionally overwrite a session an
    earlier retry had already installed, and both attempts' threads would
    then call `forward_updates` for the same id, interleaving two
    different shells' output into one pane. `TerminalHost::install_if_
    vacant` now makes installation first-wins (`HashMap::Entry`, checked
    under the lock) and kills/shuts down a losing late duplicate rather
    than letting it live on unobserved (`HostedTerminal` has no `Drop`
    impl, so this must be explicit) -- covered by two unit tests
    (`terminal::tests::install_if_vacant_*`). None of this is a full fix
    for the root cause -- which, per the residual failure rate above, is
    not eliminated under the extreme synthetic case, and now demonstrably
    not fully eliminated under realistic load either. Left open for a
    follow-up: see the `portable-pty` backlog entry for options (upgrade,
    vendor patch, or accept the retry mitigation as the practical ceiling
    given how rare it now is).
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
    Small mechanical task. Recorded 2026-07-12. **Resolved 2026-07-12**
    with a narrower shape than written: `gpui`/`gpui_platform` cannot carry
    a manifest `rev` (it splits the graph against gpui-component's own
    unpinned edge; `[patch]` to the same source is rejected by Cargo —
    both verified empirically). Landed instead: explicit rev pins on the
    gpui-component family, a recovery-command comment in Cargo.toml, and
    `--locked` on the gate's clippy/nextest invocations (AGENTS.md +
    hooks/pre-commit) so silent lock re-resolution fails loudly.
30. **Possible double-Enter after confirming an IME composition in the
    terminal** — found while implementing IME for the winit backend spike
    (leg 2, `docs/research/winit-backend-spike.md` §16 Q2), but the code
    shape it points at is gpui_linux's wayland backend, which Horizon's
    production terminal already runs on today, so it's plausibly live now.
    Wayland's text-input-v3 protocol (unlike X11's XIM) never lets the
    compositor consume keys on the client's behalf: a physical Enter that
    confirms an IME conversion still arrives as an independent
    `KeyboardInput` event, *after* the `CommitString`/`replace_text_in_range`
    call already cleared `ime_marked_text`. `TerminalView::on_key_down`'s
    IME guard (`self.ime_marked_text.is_some()`) checks state at the time
    each event arrives, so it can't see that the commit and this keydown
    belong to the same user action — the physical Enter falls through to
    normal key handling and sends an extra `\r` to the PTY. Confirmed by
    direct log evidence in the spike's own `EntityInputHandler` (same call
    shape as `TerminalView`'s), not yet reproduced against the real
    terminal. Needs: reproduce with a real Japanese IME against
    `TerminalView`, then likely fix via a one-frame "just committed"
    suppression flag (set in `replace_text_in_range` when
    `was_composing`, cleared at the next `on_key_down` regardless of
    outcome) rather than relying solely on `ime_marked_text.is_some()`.
    Recorded 2026-07-12.
    *(fixed 2026-07-12)* Confirmed vulnerable: `TerminalView::handle_key`
    in `src/terminal/mod.rs` did exactly what the analysis predicted —
    once `replace_text_in_range` clears `ime_marked_text` on commit, the
    following phantom `KeyDownEvent` for Enter falls through the guard
    and sends `\r`. Fixed with a pure `ImeCommitGuard` (armed by
    `replace_text_in_range` on `was_composing`, consumed unconditionally
    by the next `handle_key` call, suppressing only when that key is
    Enter *and* it arrived within a 100ms window of the commit —
    review feedback caught that a composition committed by mouse click on
    the candidate window produces no phantom key at all, so an unbounded
    guard would swallow a later genuine Enter, e.g. compose → click
    candidate → press Enter to send the line) — covered by unit tests in
    `src/terminal/tests.rs` for the single-suppression, rapid-typing,
    Space/candidate-commit, consecutive-composition, and
    within-window/after-window cases. Live repro with a real IME was out
    of scope (native Wayland blocks key injection); final visual
    confirmation is deferred to owner dogfooding. The agent composer
    (`src/agent/view.rs`) uses gpui-component's `Input`/`InputState`
    widget rather than a hand-rolled `EntityInputHandler`, so this guard
    doesn't apply there — left as-is. Known residual, not handled
    speculatively: an IME configured to auto-commit on a punctuation key
    would deliver that punctuation as its own phantom key within the
    window, which this guard intentionally passes through (only
    Enter/Return is treated as a plausible confirming key) — revisit only
    if dogfooding observes doubled characters.
31. **Suspected upstream fork-safety hazard in `portable-pty` 0.9.0's PTY
    spawn — can wedge a terminal spawn under extreme concurrent load, at a
    rate a bounded retry only partially masks** — found while validating
    the backlog-27/28 fix (raised e2e timeouts to a generous 120s, then hit
    *exactly* that ceiling twice in a row -- once at 60.071s against the
    old ceiling, again at 120.084s against the new one -- while
    stress-testing with a continuous `cargo build -p horizon --release` +
    `cargo clean` loop). Hitting the ceiling exactly, on the very first PTY
    update after `Create`, both times is the signature of a genuine hang,
    not merely "slower under load" (a scaling delay would show up at
    varying points below the ceiling, not pinned to it).
    `crates/horizon-sessiond/src/terminal.rs`'s `spawn_terminal` calls
    `portable_pty`'s `MasterPty::spawn_command`
    (`portable-pty-0.9.0/src/unix.rs:228`), which sets a `pre_exec` closure
    run in the fork()'d child *before* `execve`. That closure calls
    `close_random_fds()` (`unix.rs:152`), which does `std::fs::
    read_dir("/dev/fd")` — a heap-allocating, non-async-signal-safe
    operation. Rust's own `pre_exec` docs warn exactly about this: if
    another thread in the (heavily multi-threaded, Tokio + per-session +
    per-terminal OS threads) parent held e.g. glibc's malloc arena lock at
    the instant of `fork()`, the child inherits that lock permanently
    locked (the thread that would release it doesn't exist in the child's
    copy), so any allocation in the child -- exactly what `read_dir` does --
    blocks forever, and the process never reaches `execve`.
    **Not confirmed live**: a follow-up run instrumented with per-step
    diagnostic logging (entry to `create`, before/after each spawn thread,
    before/after `recv_timeout`) failed to reproduce the hang across 25
    consecutive tries under similar stress, so the exact stall point was
    never actually observed mid-hang -- this remains a well-evidenced
    hypothesis from the code and Rust's documented hazard, not a proven
    live capture.
    **Mitigation landed, not a full fix**: `TerminalHost::create` now
    bounds each spawn attempt to 10s on its own thread and retries up to 3
    times before reporting a `TerminalUpdate::Error`, so a stuck spawn can
    no longer wedge a connection's message loop forever (see backlog-28).
    A review pass on that mitigation caught a second, distinct bug it
    introduced (fixed in the same follow-up): the original design let a
    late, abandoned attempt's success unconditionally overwrite a session
    an earlier retry had already installed, so both attempts' `forward_
    updates` loops could end up running for the same `session_id`,
    interleaving two different shells' output into one pane.
    `TerminalHost::install_if_vacant` now makes installation first-wins
    and kills a losing late duplicate instead. Measured with the full
    mitigation in place: focused stress runs of just this test under the
    same sustained synthetic load still showed a residual failure rate
    around 10% (4/40 in one round), i.e. the retry reduces but does not
    eliminate the observed rate under *extreme* synthetic contention.
    Under realistic load (plain `cargo nextest run --workspace`, no extra
    synthetic stress -- the actual shape of the original flake reports)
    it held at 30/30 clean across two rounds before the install-race
    follow-up, then hit once in 15 runs after it (same failure signature,
    same test, same ~120s ceiling) -- so "rare," not "eliminated," is the
    honest characterization even under realistic load; see backlog-28 for
    the combined count.
    Out of scope to fix at the root here (patching/vendoring a third-party
    crate is a separate, larger decision) -- options for a follow-up:
    upgrade `portable-pty` if a fixed release ever ships, `[patch]` a local
    fork that drops or reworks `close_random_fds`, increase `MAX_SPAWN_
    ATTEMPTS`/add inter-attempt backoff in `terminal.rs` if the residual
    rate ever proves disruptive in practice, or get a live capture (attach
    `strace`/`gdb` to a hung `horizon-sessiond` test process before its
    `Drop`-triggered cleanup fires) to actually confirm or rule out the
    fork-safety hypothesis. Worth tracking because this isn't just a test
    hazard -- if real, it means a real user's terminal spawn could
    occasionally still fail (now reported as an error rather than freezing
    forever, per the mitigation above) under heavy host load. Recorded
    2026-07-12.
32. **DuckDB projection rebuilds from scratch on every real-world boot —
    the currency check exists but never passes on real data** — reframed
    2026-07-12 after owner review (the original "retention policy" framing
    was wrong: past-session searchability is a deliberate agent-facing
    feature; retention is a given). The intended design is already the
    right one: the writer keeps the `Store` open and projects live, and at
    boot `duckdb_projection_is_current` (event_log/writer.rs) skips the
    rebuild when the store's `max_last_sequence` matches the log tail —
    isolated runs do print "already current, skipping rebuild". On the
    owner's real data it printed "projection rebuilt (16,337 record(s))"
    on every boot instead. Suspected desync causes, in likelihood order:
    (a) records the live projection skips (the "skipped 8 corrupt lines"
    and per-event "TurnEnded ... has no turn_id; skipping agent_turns
    projection" paths) may not advance the high-water mark, so the mark
    can never catch the log tail once one exists; (b) unclean daemon death
    losing the WAL/mark flush. Work items: (1) root-cause with the real
    corpus — `~/.local/share/horizon/agent-events.jsonl.archived-20260712`
    (13MB, 16,337 records, includes the corrupt lines and legacy no-turn_id
    events) is a ready-made fixture; (2) when the mark IS behind, ingest
    only the tail beyond it instead of a full rebuild (owner's proposal:
    persisted projection + incremental catch-up); (3) quiet the resume
    noise (one summary line instead of per-session/per-event lines).
    Note the rebuild already runs off the readiness path (test hook
    proves it), so this is waste + noise, not the startup hang — that was
    the winit configure stall, fixed separately. Recorded 2026-07-12.

    **Resolved 2026-07-12.** Root cause confirmed against the real corpus,
    and it was neither suspected mechanism: (a) does not exist in the code
    — `Store::append_record` updates `agent_events`/`agent_sessions`
    unconditionally before `project_event` ever runs, so a skipped
    projection still advances the mark; a completed rebuild's mark was
    verified (via an external `duckdb -readonly` CLI read) to exactly
    match the log's true tail. The actual cause was two compounding gaps:
    (1) no incremental catch-up existed, so *any* log growth — even a
    handful of records from a resumed session's own live turn-cancellation
    fixups — forced a full rebuild of the entire history; (2) the full
    rebuild had no surrounding transaction (each record's several
    statements auto-committed, and fsynced, individually), taking minutes
    against ~16k records. Fixed: `event_log::writer::
    rebuild_and_open_duckdb_projection` now decides between three
    outcomes (`ProjectionCurrency::{Current, Behind, RebuildNeeded}`) —
    current (skip), behind (incremental catch-up via `Store::
    catch_up_from_event_log_records`, projecting only `sequence > mark`),
    or rebuild-needed (mark ahead of the tail, absent while the log is
    non-empty, or a schema migration). Both apply paths now run inside one
    DuckDB transaction. A second, independent atomicity bug surfaced
    while testing the incremental path and was fixed alongside it:
    `Store::append_record`'s own several statements were not themselves
    transactional, so a process killed mid-append could leave
    `agent_events` with a row `agent_sessions.last_sequence` didn't yet
    reflect — harmless to a full rebuild but fatal to incremental catch-up
    (a duplicate-key error), reproduced by `horizon-sessiond`'s own e2e
    suite. Resume noise (item 3) collapsed into one summary line per class
    (already-terminated sessions, legacy no-turn_id `TurnEnded` events).
    Real-corpus validation (16,417 records, 3 boots in a row): boot 1
    (first-ever, full rebuild) 175s; boot 2 and boot 3 (incremental
    catch-up of 1 new record each, from a resumed session's own live
    turn-cancellation fixup) 1s each — versus every boot taking ~238s
    before this fix. See `docs/agent-duckdb-state-design.md`'s
    2026-07-12 addendum for the full writeup. Not fixed (flagged as
    future work, not urgent since the full rebuild is now a rare
    fallback): the full-rebuild path itself is still slow in absolute
    terms (per-statement ad-hoc SQL compilation, not just the
    now-eliminated fsync cost) — a prepared-statement or DuckDB `Appender`
    bulk-insert pass could speed up the first-ever-boot/post-migration
    case further if it becomes a practical problem.

33. *(closed 2026-07-18 — owner decision: unconfirmed hypothesis; re-file
    with an input trace if the desync is ever observed)*
    **Unconfirmed: does a resumed/respawned terminal session's kitty
    keyboard mode (`TermMode::REPORT_ALL_KEYS_AS_ESC`,
    `TerminalFrame::keys_as_escape_codes`) survive correctly, or can a
    live shell end up with the flag desynced from what the shell itself
    believes?** Surfaced investigating the direct-ASCII-mode input-loss
    bug (fixed on `main` — see `docs/winit-backend-design.md`'s
    "Resolved incidents" -> "Keyboard input pipeline" -> Stage 3): the
    owner's `HORIZON_INPUT_TRACE`
    capture showed `keys_as_escape_codes=false` in their actual fish
    session, which normally enables kitty mode at startup. Investigated
    only enough to characterize, not resolve (task brief: file, don't
    fix): terminal sessions in `horizon-sessiond` are create-or-attach,
    not replay-from-log — `TerminalHost::attach` reconnects a client to
    an already-live `wezterm_term::Term` (`crates/horizon-sessiond/src/
    terminal.rs`), and `keys_as_escape_codes` is read fresh off that same
    live `Term`'s mode bits on every render
    (`crates/horizon-terminal-core/src/core/render.rs:68`) — so plain UI
    attach/detach/reattach (the daemon itself staying up) shouldn't lose
    it. The more likely culprit, unconfirmed: if `horizon-sessiond`
    itself restarts (crash/respawn — a path the e2e suite already
    exercises, e.g. `killed_sessiond_respawns_and_replays_transcript...`),
    the PTY and its `Term` are process-owned and can't survive that; the
    terminal session would need a full shell respawn, and whether the new
    shell's startup re-runs fish's kitty-mode-enable the same way (or
    whether fish's own enablement is itself conditional on something that
    differs for a respawned vs. an originally-launched shell) wasn't
    checked. Next step if picked up: reproduce with
    `HORIZON_INPUT_TRACE` plus a deliberate `horizon-sessiond` kill/respawn
    against a fish session, and diff `keys_as_escape_codes` before/after.

34. **[RESOLVED da7128d] `SessionState` reports `WaitingForUser` while a
    tool-call approval is still pending.** Found root-causing the
    2026-07-13 flat-render regression (session `3fe93cdb…`, "Agent
    #30"): with two bash approvals outstanding, approving the first
    moved the daemon's state to `WaitingForUser` for a real 36 seconds
    while the second request sat unresolved — `WaitingForApproval`
    should arguably hold until the actionable queue is empty. The UI
    no longer breaks on this (dangling spans always render as turns
    since `e7ba824`), but two visible symptoms remain rooted here: the
    status line goes blank and the status-row stop button disappears
    mid-turn (both key off `state_indicates_turn_in_flight`).
    Fixed: `crates/horizon-sessiond/src/session.rs::fold_bash_completion`
    (the only async-tool completion path — `fs.write`/`fs.edit`/
    `config.write` all resolve synchronously and already avoided this,
    see `agent::tools::approval::synchronous_result`'s own
    no-trailing-`WaitingForUser` comment) now checks the session's live
    frame (`AgentFrame::actionable_pending_approval_call_ids`, excluding
    the call just finishing) before choosing its trailing
    `StateChanged`: `WaitingForApproval` if another approval-gated call
    from the same turn is still outstanding, `WaitingForUser` only once
    none is. Covered by a new unit test in that file exercising two
    approval-gated bash calls. The status-line/stop-button gating itself
    was left unchanged (already correct once the state stopped lying) —
    re-check it separately if either still misbehaves.

35. *(resolved 2026-07-14)* **UI-side agent-session commands silently
    swallow a dead-sessiond channel error.** `src/agent/session.rs`'s
    approve/deny/send_user_message/cancel/shutdown paths all did `let _ =
    self.commands.send(...)` — if `horizon-sessiond` had died (crashed,
    killed, socket closed) the send failed and every click or keystroke
    in that pane became a silent no-op, with nothing surfaced to the
    user. Noticed auditing that file's command dispatch while fixing
    backlog #34.
    Fixed: all five now funnel through one `AgentSession::dispatch`,
    backed by a small pure `RuntimeReachability` state machine
    (`after_send`/`after_event_received`) wrapped in a `Cell` for
    interior mutability, since every call site only ever holds `&self`
    (`Entity::read`, never `update`) — kept that seam untouched rather
    than widening the public API across `workspace.rs` and the
    rows/receipts/composer call sites. The first failed send flags
    `runtime_unreachable` and wakes a tiny dedicated notify-pump task
    (`futures::channel::mpsc` + a `cx.spawn` loop, symmetrical with the
    existing event-pump task) that calls `cx.notify()` on the entity,
    which the view's already-existing `cx.observe(&session, ...)` turns
    into a re-render; every subsequent send short-circuits without
    touching the channel again. The agent view's status line
    (`AgentView::status_line`) shows "session runtime unreachable — try
    Reload Session Runtime" in `theme::danger()` while the flag is set,
    overriding the normal state-derived text. Recovery is automatic: the
    existing per-event pump clears the flag on the next event that
    actually arrives (stale-death recovery), so a respawned/reconnected
    runtime silently un-flags itself. Covered by 6 colocated unit tests
    on `RuntimeReachability`'s pure transitions (`src/agent/session.rs`).
    Aside: `session.rs`'s `use gpui::*` glob-imports `gpui::test` (the
    GPUI-aware async-test attribute), which shadows the standard
    `#[test]` in any descendant module that does `use super::*` — hit
    this as a genuine stack overflow inside `gpui_macros` before tracing
    it down; the new `tests` module imports only the specific items it
    needs instead.

36. *(resolved 2026-07-14)* **Shared build-dir flake: `CARGO_BIN_EXE_horizon-sessiond`
    spawn fails with NotFound while sibling worktrees build concurrently.**
    Observed 2026-07-13 in the integration worktree with three worker
    worktrees building in parallel: `horizon-sessiond::e2e`'s
    `a_hello_with_the_wrong_contract_version_is_rejected_with_a_reason`
    panicked at spawn ("No such file or directory") although
    `target/debug/horizon-sessiond` existed; the same test passed on
    an isolated rerun immediately after.
    Root cause **established** (not just hypothesis): cargo's own
    artifact-uplift step is non-atomic. `cargo-util` 0.2.28's
    `paths::link_or_copy` (the function cargo's `link_targets` compiler
    step calls to copy/hardlink every unit's output from its build-dir
    location into the final `target/<profile>/` path) unconditionally
    `remove_file`s the destination before re-linking it, and per
    `link_targets`'s own doc comment this runs on *every* cargo
    invocation for *every* unit -- including a "fresh" one where nothing
    recompiled -- not only on an actual rebuild. `target/debug/
    horizon-sessiond` (what `CARGO_BIN_EXE_horizon-sessiond` bakes into
    the test binary at compile time) is therefore never guaranteed
    stable across a cargo invocation's lifetime, full stop -- this isn't
    specific to the shared `build-dir`. Locally reproduced without any
    other worktree involved: a tight existence-poller on that exact path
    (62M checks over 241s) caught exactly one transient absence, landing
    during a `cargo nextest run --workspace -p horizon-sessiond` that
    forced a rebuild, while a real sibling `cargo nextest run -p
    horizon-agent` was compiling concurrently on the same machine (this
    machine's own worktrees do share the build-dir's advisory lock for
    real -- a later `cargo clippy` in the same session independently hit
    "Blocking waiting for file lock on build directory"). What stays
    **hypothesis** (plausible, not independently isolated): that
    cross-worktree lock contention *specifically* is what widens this
    window enough to matter in practice, versus the window already being
    wide enough on a busy single-worktree machine. Either way the fix is
    the same. Fixed in `crates/horizon-sessiond/tests/e2e.rs`: all four
    `Command::new(env!("CARGO_BIN_EXE_horizon-sessiond"))` call sites now
    go through `resolve_sessiond_binary()` (verifies the baked-in path
    exists, one bounded wait and re-check on miss, then an independent
    fallback derived from the test binary's own `current_exe()`) and
    `spawn_sessiond()` (retries the actual `spawn()` once more on a
    `NotFound` at the syscall level). Both panic with every path probed
    (not a bare ENOENT) if they're still missing after that. No
    sleep-loops -- one bounded wait per layer.

37. *(closed 2026-07-18 — owner decision: upstreaming is the owner's own
    act if ever; the macOS-gated seed in horizon-winit-platform already
    documents the workaround)*
    **Upstream the gpui_wgpu Metal backends fix to zed.**
    `WgpuContext::instance` hardcodes `Backends::VULKAN | GL` (gpui_wgpu
    is zed's Linux renderer; upstream main unchanged as of 2026-07-14),
    which is why `horizon-winit-platform/src/window.rs` carries a
    macOS-gated `GpuContext` seed. A one-line upstream PR (add METAL
    under `cfg(target_os = "macos")`, or switch to
    `Backends::from_env_or_default()`) would let us delete the seed
    entirely on the next gpui bump. See
    `docs/winit-backend-design.md` "macOS bring-up".

38. *(closed 2026-07-18 — owner decision: no Horizon surface feeds it;
    revisit only when a find/search field lands in the shell)*
    **macOS find pasteboard is stubbed.** `Platform::
    read/write_from_find_pasteboard` in `horizon-winit-platform` return
    `None`/no-op; gpui_macos implements them against the real
    `NSPasteboard` find pasteboard (system-wide "Use Selection for
    Find", cmd-E). No Horizon surface feeds it today, so no user-visible
    gap yet — implement via a small objc2-app-kit call when a find/search
    field lands in the shell.

39. *(closed 2026-07-18 — validated: `cargo nextest run -p
    horizon-winit-platform` on this Linux host, 33/33 green including
    `dispatcher::tests::concurrent_main_thread_posts_all_get_processed`)*
    **Vendored dispatcher queue not yet test-run on Linux.**
    `horizon-winit-platform/src/queue.rs` (vendored from gpui because
    the queue isn't compiled on macOS) now backs the dispatcher on every
    OS, but its regression test
    (`dispatcher.rs::concurrent_main_thread_posts_all_get_processed`) is
    Linux-gated and the change was made on macOS. Compile-checked
    everywhere; the first Linux `cargo nextest run --workspace` after
    the 2026-07-14 merge is the actual validation — run it and close
    this item.

40. *(resolved 2026-07-15)* **Shared build-dir: stale e2e test binary bakes
    a CARGO_BIN_EXE path into a deleted worktree.** Observed 2026-07-15 by
    a worker (theme trio branch): `horizon-sessiond::e2e`'s
    `CARGO_BIN_EXE_horizon-sessiond` pointed at a sibling worktree that had
    already been merged and removed — a *permanent* miss, unlike item 36's
    transient non-atomic-uplift window (36's retry-on-miss thinking
    doesn't apply when the baked path can never exist again). Mechanism:
    the compiled e2e test artifact itself is reused across worktrees via
    the shared `build.build-dir`, carrying the env-baked absolute path
    from whichever worktree compiled it last. Workaround used: `touch
    crates/horizon-sessiond/tests/e2e.rs` to force a real recompile in the
    current worktree. Candidate fixes considered: resolve the sessiond
    binary at test *runtime* relative to `current_exe()` instead of the
    compile-time env; or exclude the e2e test unit from the shared
    build-dir. **Chosen fix, and a correction to the first candidate
    above:** `current_exe()` turned out not to work at all under this
    repo's `build.build-dir` split — test/bench binaries (unlike `[[bin]]`
    targets) are never uplifted out of the shared build-dir into a
    worktree's own `target/`, so `current_exe()` for the e2e test binary
    always resolves *inside the shared build-dir*, common to every
    worktree, and can't anchor a per-worktree path at all (confirmed
    empirically: `target/debug/deps/e2e-*` does not exist in any
    worktree's own `target/`). The actual fix (`crates/horizon-sessiond/
    tests/e2e.rs`, `resolve_sessiond_binary`): read `CARGO_BIN_EXE_
    horizon-sessiond` as a *runtime* environment variable
    (`std::env::var`) instead of the same-named compile-time `env!()`
    bake — cargo and cargo-nextest both document (and this was confirmed
    against this repo's toolchain) re-injecting that var fresh into the
    test process's own environment on every invocation, computed from the
    *current* worktree's `cargo metadata`, regardless of whether the test
    binary itself was recompiled or reused stale from the shared
    build-dir. The `env!()` bake is kept only as a defensive fallback for
    direct (non-cargo/nextest) invocation of the test binary.

41. *(closed 2026-07-18 — moot per the 2026-07-15 note below; nothing to
    do unless the tab strip switches variant again)*
    **`equal_tab_width` chrome allowances are stale for `TabVariant::Tab`.**
    Observed 2026-07-15 during the design-C tab-strip switch: `src/
    workspace.rs`'s `EQUAL_WIDTH_CHROME_ALLOWANCE_PX` (24px) and
    `EQUAL_WIDTH_GAP_PX` (2px) were tuned for the retired `Segmented`
    variant's inner padding + gap; the default `Tab` variant has zero
    inner padding/gap (adjacent tabs touch, 1px side borders), so
    computed widths are now a knowable amount too narrow — cosmetic,
    not broken. **Moot again (2026-07-15):** the owner reverted the
    design-C switch after seeing the result (`docs/theme-design.md`'s
    "design C for chrome — REVERTED" note); the tab strip is back to
    `Segmented`, whose allowances were never stale. Only relevant again
    if the strip switches variant a second time.
