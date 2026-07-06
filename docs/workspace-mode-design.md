# Workspace Mode and the Cursor

Status: design settled and implemented 2026-07-06 (v1 core: mode,
cursor, `:`-palette, visualization; phase B: `ctrl+'` default, global
palette chord retired, click-dives). The former pending check
(Super+Esc) is resolved — see "Pending verification". This document
records the design conversation between the owner and the planning
session.

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

### The escape chord: `ctrl+'`, shipped, configurable

The only irreducible theft is leaving passthrough by keyboard. First
choice was **Super+Esc**: TUIs historically could not receive Super at
all, so almost nothing in-pane binds it, and the competition for Super is
the window manager rather than terminal apps. That rationale held up to
reasoning but not to the owner's actual machine: on the owner's real
GNOME session, gnome-shell intercepts Super+Esc before it ever reaches
Horizon's window at all -- confirmed empirically, while Horizon's own
key-handling path was separately shown to be healthy headless (no WM in
the loop). Super+Esc was dropped for the shipped default as a result.

The shipped default is now **`ctrl+'`**: apostrophe has no legacy
terminal encoding, so (almost) no in-pane TUI can plausibly already have
a claim on it, and it sits under a comfortable finger for the owner's
Dvorak layout. As with the rejected Super+Esc, the chord is configured
via the existing `[keybindings]` reserved-name mechanism (same pattern as
`"open-palette"`), so a future collision or a different owner's layout
preference is a config edit, not a redesign.

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

Resolved 2026-07-06: Super+Esc was checked on the owner's real GNOME
session and found to be intercepted by gnome-shell before reaching
Horizon's window (Horizon's own key-handling path was independently
verified healthy headless), so the shipped default was changed to
`ctrl+'` -- see "The escape chord" above. Nothing outstanding here; a
future collision remains a config edit via `[keybindings]`, not a
redesign.

## Non-goals for v1

- Multi-pane selection (the cursor is singular; `selection` remains
  available as a future name for multi-select).
- Configurable in-mode keymap (only the escape chord is configurable at
  first).
