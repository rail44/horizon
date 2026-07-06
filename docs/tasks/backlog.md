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
2. **Insert and F1-F24 send nothing** — `app::keymap` has no wiring for
   them at all (not even legacy bytes). F13+ also needs the kitty PUA
   table (see `KITTY_COMPLIANCE`).
3. **Kitty event types for navigation keys** — release/repeat subfields
   only decorate CSI-u forms today; arrows/Home/End/etc. are an honest
   Unimplemented row in `KITTY_COMPLIANCE`.
4. **OSC 52 clipboard write** — apps cannot copy to the system clipboard;
   the event is currently dropped (needs clipboard access from the
   view/app layer).
5. **Focus events (CSI I/O)** — never sent on pane focus change; needs a
   pane-focus signal wired into the terminal session.
6. **agentd e2e flakiness under load** — `drained_agentd_respawns...` and
   `killed_agentd_respawns...` fail nondeterministically under
   `cargo test --workspace -j4` on a loaded machine; pass standalone.
7. **Theme roles lost two distinctions** — the user message bubble's blue
   tint and the approval banner's amber background have no matching theme
   role (both fell back to neutral surfaces); candidates for a
   `surface_accent_soft`-class role.
8. **ghostty multi-attach corruption** — deliberately deprioritized;
   captured evidence lives in the session transcripts (PTY traces under
   /tmp/horizon-pty-*.jsonl as of 2026-07-05).
9. **floem startup input gap (~0.5s)** — accepted regression of the git
   pin; candidate for an upstream report (5/5 reproducible bisection).
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
13. **headless GUI verification writes into the real agentd state** —
    `check-terminal-visual.sh`/`run-terminal-smoke.sh` runs connect to
    the owner's real `horizon-agentd` socket and persist throwaway test
    sessions into the real event log / DuckDB (observed at ~4k lines,
    plus replay cost on every reconnect). The scripts should isolate
    `HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB` and point agentd
    at a scratch socket per run. Flagged 2026-07-06 during the startup
    focus diagnosis.
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
