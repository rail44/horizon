# Workspace Mode and the Cursor

Status: core decisions settled 2026-07-06; open questions listed at the
bottom. This document records the design conversation between the owner
and the planning session; implementation has not started.

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

## Open questions (agenda for the next session)

1. Dive or stay: after a split, and after executing a palette command,
   does focus follow into the pane or stay in workspace mode?
2. Visualization: which combination of pane dimming, cursor frame, and
   a status-bar mode chip. Must be loud enough to pay for persistence.
3. In-mode key principle: vim-spatial (`hjkl`, counts?) vs mnemonic;
   then the initial keyset (movement, split, tab nav, palette, close).
4. Migration of existing chords: retire `ctrl+shift+p`/`ctrl+p` into
   the mode (this absorbs backlog item 1 — code/docs/status-bar/smoke
   scripts move together).
5. Full-passthrough pane lock (hand even the escape chord to the app;
   mouse-only exit): deferred, not in v1.
6. Super+Esc pass-through confirmation on the owner's desktop.

## Non-goals for v1

- Multi-pane selection (the cursor is singular; `selection` remains
  available as a future name for multi-select).
- Configurable in-mode keymap (only the escape chord is configurable at
  first).
