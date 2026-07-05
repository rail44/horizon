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
