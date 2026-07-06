# Backlog — small known issues, not yet missions

Discovered during dogfooding; promote to a numbered mission when picked up.

1. **Palette-open key mismatch** — code default is `ctrl+p`; the status
   bar, AGENTS.md, and the verification scripts all claim `Ctrl+Shift+P`;
   `ctrl+p` also collides with shell history. Blocked on the owner's
   choice of default (`ctrl+shift+p` vs `ctrl+;`). Fix must align code,
   status-bar text, scripts, and docs together (this also un-breaks the
   `new-terminal-focus` / `split-pane` headless smoke scenarios).
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
