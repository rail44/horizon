# Workspace Mode and the Cursor

Status: design settled and implemented 2026-07-06 (v1 core: mode,
cursor, `:`-palette, visualization; phase B: `ctrl+'` default, global
palette chord retired, click-dives). The former pending check
(Super+Esc) is resolved — see "Pending verification". Amended
2026-07-19: an empty workspace is now an implicit command surface, no
`ctrl+'` needed — see "Empty workspace is an implicit command surface"
below. This document records the design conversation between the owner
and the planning session.

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

## Empty workspace is an implicit command surface (2026-07-19)

`5d62143` (2026-07-18) made a zero-tab workspace a valid, persistable
state and kept the mode reachable from it by decoupling
`workspace_mode_active` from the cursor (a cursor needs a pane to seed
it; the "is the mode active at all" flag doesn't). That still required
the owner to press `ctrl+'` *first*, then `:` — a real two-step dance
issue 003 separately flagged as invisible (no pane exists to carry any
of the mode's visual signals, so the owner had to press `:` blind to
find out whether the entry chord had taken effect).

The owner's follow-up clarification cuts deeper than "make the entry
step visible": workspace mode exists to separate "keys go to the
focused pane" from "keys command the workspace" (see "Problem" above).
With zero panes there is simply no pane input left to protect, so
requiring the entry chord in that state protects nothing — it's not an
under-signaled state, it's an unnecessary one. An empty workspace is
therefore **implicitly always** a command surface: `:` (opening the
palette, the only reachable path back to `New Tab…` once every pane is
gone) works directly, no `ctrl+'` needed first.

**Seam chosen: the getter, not a separate context.** Two shapes were on
the table — (a) make `Workspace::is_workspace_mode_active()` report
`true` unconditionally whenever the workspace has zero tabs, regardless
of the raw "explicitly toggled" bookkeeping, or (b) introduce a second,
distinct key context carrying just the pane-independent subset of the
mode's bindings for the empty case. (a) won: it falls directly out of
the existing decoupling `5d62143` already did (the raw flag and "is the
mode reachable" were already separate concerns; this just adds one more
condition to the read side) and is airtight *by construction* in both
transition directions — no call site has to remember to flip a flag on
the way into or out of empty, since the answer is recomputed fresh from
`tab_count()` every time. (b) would have needed a parallel, mostly-
duplicate binding table for the pane-independent subset (in practice
just `:`, since `t`/`a`/`s`/`x`/`tab`/hjkl/Enter/Esc are all either
already pane-independent through the *same* mechanism (`t`/`a` open the
view chooser, which doesn't need an existing pane either) or naturally
inert with zero tabs through their own existing guards (`s`/`x`/`tab`),
so there was nothing left for a separate context to carry that the
existing one doesn't already cover once it's reachable.

**What `is_workspace_mode_active()` now means.** `true` because the mode
was explicitly toggled on (the raw field, exact for a non-empty
workspace) *or*, unconditionally, because the workspace has zero tabs.
The raw field keeps its narrower, exact meaning; the getter layers the
zero-tab bypass on top. A direct, harmless consequence: `ctrl+'` on an
empty workspace becomes a no-op, because `toggle_mode`
(`src/workspace/render.rs`) always sees this method return `true` there
and takes its "cancel" branch, which is already idempotent when the
cursor is `None`. No visual change either way, since `render_node`
(the only place that paints the scrim/cursor-border chrome) never runs
without a pane to paint it on.

**The one hazard this reopened, and how it's closed.** Every
modal-opening handler (`open_palette`/`open_view_chooser`/
`open_session_manager`) calls `Workspace::exit_workspace_mode` before
the modal takes focus, specifically so the mode's own fixed hjkl/Enter/
Escape bindings stop competing with the modal's typed search keys (see
`effective_scrim_pattern`'s doc comment for the identical scrim-side
concern). The zero-tab bypass doesn't care about that flag, so on an
empty workspace `is_workspace_mode_active()` would otherwise keep
reporting `true` right through that exit call — reopening exactly the
hazard the exit call exists to close. `src/workspace/render.rs`'s
`mode_key_context_active(is_workspace_mode_active, modal_open)` is the
fix: the render's key-context decision suppresses the mode context
outright whenever any control-surface modal is open, independent of
what the getter itself reports. The render additionally suppresses the
mode context while a workspace restore is in flight (unless it failed,
mirroring `workspace_mode_blocked_by_restore`'s existing exception for
reaching `Reload Session Runtime`) — a persisted zero-tab workspace
still runs a real background round trip to `horizon-sessiond` before the
restore barrier lifts, so without this the bypass could let `:` open the
palette mid-restore.

**Transitions.** Both directions are airtight by the same construction:
closing the last tab makes `tab_count() == 0` true, so the very next
render sees the bypass fire with no explicit hand-off needed; creating
the first tab (from the palette's `New Tab…`, or any other path) makes
it false again, so the bypass stops applying and ordinary raw-field
gating resumes immediately — matching "non-empty behavior changes not
at all."

Explicitly out of scope here (per issue 003's own resolution note): the
pane-bound visual signals themselves (cursor-pane border, scrim dim) for
a *non-empty* workspace are untouched.
