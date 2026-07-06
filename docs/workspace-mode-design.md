# Workspace Mode and the Cursor

Status: design fully settled 2026-07-06 (two rounds); implementation
has not started. The only remaining item is an empirical check, listed
at the bottom. This document records the design conversation between
the owner and the planning session.

## Problem

Horizon is a terminal-first GUI with faithful key forwarding (see the
kitty-keyboard compliance work). In that world, every *global* chord the
shell steals — `ctrl+shift+p`, `ctrl+;`, anything — is a key some
in-pane TUI could legitimately have received, and each new workspace
operation would demand another stolen chord. The palette-key mismatch
(backlog item 1) is a symptom; the disease is the chord-per-operation
model itself.

## Core decisions

### A persistent workspace mode, one stolen key total

Workspace operations live in a **persistent mode** (vim normal/insert
style, per the owner's preference), entered from a terminal pane by a
single reserved chord. Inside the mode every key belongs to Horizon, so
the vocabulary can grow indefinitely without new theft. The palette
stops being a global chord and becomes a one-key resident of the mode.

A transient tmux-style prefix (auto-return after one command) was
considered: it structurally eliminates mode-confusion accidents but
taxes every repeated operation (move three panes, resize five times).
Persistent mode was chosen; the accident cost is paid with loud
visualization instead (see open questions).

### Two selection concepts: focus and cursor

The owner's articulation, adopted verbatim as the design's spine —
there are two different "selected pane" concepts:

- **focus** — where input flows (the existing concept). A focused
  terminal pane receives everything, kitty-faithfully.
- **cursor** — the pane/tab Horizon operations act on (new). In
  workspace mode, navigation keys move the cursor. `Enter` commits:
  focus follows the cursor and passthrough resumes. `Esc` cancels: the
  cursor snaps back to the focus, which never moved.

Consequences that make the split load-bearing:

1. Cancel is definable (cursor moved, focus didn't — `Esc` is free).
2. Acting on a *remote* pane is expressible: close/terminate that pane
   over there without moving where you type. The command-target rule
   collapses to one sentence: **commands act on the cursor** (normally
   cursor == focus). Existing "active" wording is to be redefined as
   "what the cursor points at".
3. Agents and the future CLI get the same noun: a delegated agent can
   set the cursor and issue commands **without stealing the human's
   focus**. The unified command model takes `cursor` as its target
   argument on both the human and agent sides (Phase-1 delegation
   connection).

### Why this preserves the GUI's collision advantage

A purely temporal, zellij-style global mode would re-import the TUI
constraint (one input stream, timeshared). Horizon's separation is
spatial: focus decides where keys go, the mouse can always change it,
and window-manager focus already isolates separate Horizon instances —
a dev build nested inside a stable session is a separate window and
therefore a separate input stream (owner's correction; the tmux
nested-prefix analogy does not apply here). The mouse remains a
non-keyboard escape hatch from any state, so the one stolen chord is a
convenience for keyboard flow, not a lifeline.

### The escape chord: Super+Esc, tentative, configurable

The only irreducible theft is leaving passthrough by keyboard. Chosen
pocket: **Super+Esc** (tentative). Rationale: TUIs historically could
not receive Super at all, so almost nothing in-pane binds it; the
competition for Super is the window manager, not terminal apps.
Pending: an empirical check that the owner's GNOME session lets
Super+Esc through. The chord is configured via the existing
`[keybindings]` reserved-name mechanism (same pattern as
`"open-palette"`), so a WM collision or nested-instance preference is a
config edit, not a redesign. Machinery note: floem's `meta()` modifier
already maps to SUPER app-side.

### Per-kind asymmetry

- Terminal pane: only the escape chord leaves passthrough (in-pane apps
  need raw `Esc`).
- Agent pane: `Esc` may return to workspace mode directly (a message
  box has no claim on raw `Esc`), except during IME composition. The
  approval banner's key capture (`AgentPaneFocus`) already works as a
  small-scale precedent of mode-as-focus.

## Second-round decisions (settled 2026-07-06)

1. **Creating operations dive; everything else restores.** After a
   split or a new terminal/agent session, focus follows into the new
   pane (you made it to use it). Palette commands that create nothing
   (reload, terminate-detached, ...) return to the state from before
   the palette opened.
   *Amended 2026-07-06 (second revision):* diving is a property of the
   **origin**, not the command. Creating/attaching operations invoked
   from human surfaces (mode, palette) dive — and `AttachSession`
   joins this bucket. The same operations arriving over the control
   plane carry an explicit `activate` argument and default to **not**
   stealing focus (an agent opening views behind the owner's work must
   not grab the keyboard); the CLI exposes `--active` as the opt-in.
   See `docs/cli-control-plane-design.md`.
2. **Visualization uses all three signals**: pane dimming while in
   workspace mode (the accident-killer), a cursor frame visually
   distinct from the focus border, and a status-bar mode chip.
3. **`:` opens the palette** — workspace mode's `:` is vim's
   normal-to-cmdline transition, making the palette the command-line
   analogue outright. The v1 keyset is deliberately minimal: `hjkl`
   movement, `Enter` (commit focus to cursor), `Esc` (cancel), `:`
   (palette). Everything else goes through the palette until
   dogfooding proves a key promotion. Structural principle (owner):
   keep the in-mode key handling shaped for future vim vocabulary —
   interpret key sequences rather than a flat one-key-one-action
   table, so counts/motions can arrive without a rewrite.
4. **Global modifier shortcuts retire when the mode ships.** No
   dual-running period: `ctrl+shift+p`/`ctrl+p` go away, and code,
   status-bar text, smoke scripts, and docs move in the same change
   (absorbing backlog item 1).
5. **No full-passthrough pane lock in v1** (the per-pane "hand even
   the escape key to the app, exit by mouse only" valve). Add it when
   a real need appears.

## Pending verification

- Super+Esc pass-through on the owner's GNOME session — checked on
  first run once implemented; a collision is a config edit (the escape
  key rides the `[keybindings]` mechanism), not a redesign.

## Non-goals for v1

- Multi-pane selection (the cursor is singular; `selection` remains
  available as a future name for multi-select).
- Configurable in-mode keymap (only the escape chord is configurable at
  first).
