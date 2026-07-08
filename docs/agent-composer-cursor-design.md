# Agent Composer Cursor Design

Records why the agent pane's message-box composer (`workspace::view::
agent_controls::agent_composer`) got a real cursor through a hand-rolled
`AgentDraft { text, cursor }` model instead of floem's built-in
`views::text_input`, and how the resulting rendering trick works.

## Problem

The composer was a read-only `label` rendering the whole draft string, fed
by an append-only key handler (`workspace::input::handle_agent_key`): typed
characters were `push_str`-ed onto the end, Backspace popped the last
grapheme off the end, and there was no cursor state at all. Two consequences
followed directly from that: no cursor was ever drawn, and the arrow keys
had nothing to move.

## Why not `floem::views::text_input`

The obvious fix is to swap the `label` for floem's real `text_input`, which
owns a cursor and handles arrow keys/insert/backspace internally. This was
tried first and rejected after reading the vendored floem source
(`floem` pinned via git rev in the workspace `Cargo.toml`), for two
independent reasons:

1. **IME is not implemented.** `TextInput::event_before_children` matches
   `PointerDown`/`PointerMove`/`KeyDown` only — there is no handling of
   `Event::ImePreedit`/`Event::ImeCommit` anywhere in the widget. Horizon's
   own IME plumbing (`app::input::AppInput::handle_ime_preedit`/
   `handle_ime_commit`, the `ime_composing`/`ime_preedit` signals) would
   still have to run in parallel, and there is no public API on `TextInput`
   to insert committed IME text at its internal cursor position (the
   `cursor_glyph_idx` field is private, with no getter/setter). Composed
   Japanese/CJK text could only ever be appended at the tail, reintroducing
   the exact bug this work fixes, just for IME input specifically.

2. **Keyboard dispatch is single-target, not bubbling.** Floem sends a
   keyboard event straight to whichever one `ViewId` currently holds
   `app_state.focus` (`EventCx::unconditional_view_event` is called with
   `directed = true` for `needs_focus()` events, which skips descending into
   children entirely). Horizon's pane view is one large `keyboard_navigable`
   container whose own `KeyDown` handler does all the routing today
   (palette/workspace-mode chords, the inline approval row's `y`/`n`,
   terminal vs. agent dispatch). Making a nested `text_input` receive key
   events at all means moving real floem focus onto it, which in turn means:
   the container's existing `PointerDown` handler (pane activation +
   workspace-mode commit-on-click, `workspace::view::pane`) would stop
   firing on a click into the composer, because `TextInput::
   event_before_children` unconditionally consumes its own `PointerDown`
   and stops propagation before it reaches the container; and every global
   chord currently caught by the container's `KeyDown` handler would need
   a second copy wired directly onto the `text_input` view instead.

Both are fixable in principle (patch/fork floem, or re-plumb focus and
duplicate the chord handling) but are large, risky changes to the pane's
whole input-routing architecture for what the two reported bugs actually
need. The chosen design gets both fixes with no change to focus, IME, or
approval-key routing at all.

## Design

`AgentDrafts` (`workspace::input`) changed element type from `RwSignal<
String>` to `RwSignal<AgentDraft>`:

```rust
pub(crate) struct AgentDraft {
    pub(crate) text: String,
    pub(crate) cursor: usize, // byte offset into `text`, always a char boundary
}
```

Every write path goes through one of two primitives that keep `cursor`
consistent with `text`:

- `insert_agent_draft_text(draft, text)` — inserts at `cursor`, advances
  `cursor` past the inserted bytes. Used by ordinary character insertion,
  clipboard paste, IME commit (`app::input::AppInput::handle_ime_commit`),
  and the inline approval row's soft-redirect path (`workspace::view::pane`'s
  `ApprovalKeyAction::Redirect`) — previously all four `push_str`-ed onto the
  tail regardless of where the user had last left the cursor.
- `apply_agent_draft_action` (`workspace::input`, private) — the Backspace/
  MoveLeft/MoveRight/Submit cases, using `prev_grapheme_boundary_approx`/
  `next_grapheme_boundary_approx` (`app::keymap`) to find the byte offset to
  delete to or move to. These generalize the old `pop_last_grapheme_approx`
  (which only ever worked at the end of the string) to an arbitrary cursor
  position, keeping the same "step back over combining marks" approximation.

`agent_draft_action` (`app::keymap`) gained `MoveLeft`/`MoveRight` variants
for `ArrowLeft`/`ArrowRight`, gated by the same `agent_accepts_text_input`
modifier check as ordinary character insertion (no Ctrl/Alt/Meta held).

### Rendering the cursor without a real text-input widget

`agent_composer` splits `draft.text` at `draft.cursor` and lays out five
fixed children in one `h_stack` (row):

```
[ text before cursor ][ preedit ][ caret bar ][ text after cursor ][ placeholder ]
```

Flex row layout places the caret bar exactly at the right character
boundary using each label's own intrinsic text width — no glyph-position
math needed. IME preedit is spliced in right at the cursor (previously it
was always appended at the tail, which only ever looked wrong once the
cursor could be moved). The placeholder ("Message agent...") is the fifth
segment, rendered only when both `text` and `preedit` are empty; since the
other four segments are empty strings in that state, it reads as `|Message
agent...` with the caret at the very start. The caret itself is a plain
`empty()` view styled as a narrow filled rect, shown only while `active()`.

This intentionally does not support mouse click-to-position-cursor or text
selection — out of scope for the two reported bugs (cursor not shown, arrow
keys inert), and both are still absent from the old label-based composer
today.
