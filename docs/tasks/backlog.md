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
