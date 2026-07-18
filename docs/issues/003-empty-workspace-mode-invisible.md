---
id: 003
title: Workspace mode is invisible in an empty workspace — no pane, no cursor frame, no way to tell the mode is active
status: open
severity: medium
area: workspace
---

## Repro

1. Close or terminate every tab so the workspace is empty (valid state
   since `5d62143`).
2. Press `ctrl+'`.
3. Press `:` — the palette opens, proving the mode was active.

## Observed

Step 2 produces no visual change whatsoever. Every signal workspace
mode has — the cursor pane's border role, the scrim dim over non-cursor
panes (`src/workspace/render.rs`'s pane-chrome functions) — is painted
*on panes*, so with zero panes there is no drawing surface left that
reflects the mode. The owner had to press `:` blind to find out whether
`ctrl+'` had taken effect. Entering the mode is also the *only* path to
act on an empty workspace (the palette is the way back to `New Tab…`),
which makes the blindness worse: the one affordance the state has is
undiscoverable-in-progress.

## Expected

Some pane-independent indication that workspace mode is active — the
empty state (and arguably any state) should reflect the mode without
depending on a pane existing to carry the chrome. What that signal is
(window-chrome tint, a status element on the empty surface, something
else) is a design decision; this issue only records that the current
signal set is pane-bound and therefore empty-blind.

Filed 2026-07-19 from owner dogfooding of the empty-workspace change,
relayed through the project session.
