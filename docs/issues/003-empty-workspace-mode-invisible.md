---
id: 003
title: Workspace mode is invisible in an empty workspace — no pane, no cursor frame, no way to tell the mode is active
status: resolved
severity: medium
area: workspace
---

## Resolution (2026-07-19)

The owner's follow-up clarification superseded this issue's premise rather
than asking for a better invisible-mode signal: with zero panes there is
no pane input left to protect, so requiring the `ctrl+'` entry step at
all in an empty workspace was the wrong shape, not merely an
under-signaled one. `Workspace::is_workspace_mode_active`
(`crates/horizon-workspace/src/mode.rs`) now reports `true` whenever the
workspace has zero tabs, regardless of whether the mode was ever
explicitly toggled on — so `:` (opening the palette, the only reachable
path back to `New Tab…`) works directly with no `ctrl+'` first, and
`ctrl+'` itself becomes a harmless no-op in that state (see that
method's doc comment, and `docs/workspace-mode-design.md`'s "Empty
workspace is an implicit command surface" section). This dissolves the
empty-workspace half of what this issue described: there is no longer an
invisible *entered* state to be blind about, because entry is no longer a
separate step.
Mode visibility in a *non-empty* workspace — the cursor-pane border and
the scrim dim (`src/workspace/render.rs`'s pane-chrome functions) this
issue's "Observed" section points at — is completely untouched by this
change and was never in scope here; that pane-bound signal set continues
to work exactly as before whenever at least one pane exists.

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
