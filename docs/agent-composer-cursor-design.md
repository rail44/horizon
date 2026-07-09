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

The composer originally rendered as five fixed children in one `h_stack`
(row) — `[before cursor][preedit][caret bar][after cursor][placeholder]` —
placing the caret at the right character boundary via each label's own
intrinsic text width. That worked for a single line, but had no way to
wrap long lines or grow past one line for a manually-entered newline: flex
placement of intrinsically-sized labels only ever produces one visual
line, and a hard-coded fixed height capped the box regardless of content.
Two follow-on bugs came from exactly this: long lines ran off the right
edge of the window instead of wrapping, and `Enter` submitted
unconditionally with no way to type a literal newline at all.

The fix (`workspace::view::composer_text::ComposerTextView`) replaces the
five-label row with a single custom `View` that owns one
`floem_renderer::text::TextLayout` for the whole composer and draws both
the glyphs and the caret from it:

- `draft.text` with any IME `preedit` spliced in at `draft.cursor`
  (`composer_text::splice_preedit`, unit-tested directly) becomes the
  content string, laid out via `TextLayout::set_text` with no width
  constraint to get its natural (unwrapped) size.
- Once taffy resolves the view's own assigned width (a two-pass technique
  copied from floem's vendored `Label`'s `TextOverflow::Wrap` handling,
  `views/label.rs`: size off the natural layout first, then in
  `compute_layout` compare against the real assigned width and re-wrap +
  request another layout pass if narrower), a second `TextLayout` is
  cloned from the first, `set_wrap(Wrap::WordOrGlyph)`, and
  `set_size(available_width, f32::MAX)`. `TextLayout::size()` after that
  gives the wrapped content's true height, which becomes the view's
  height — so the composer's minimum height (`agent_controls::
  COMPOSER_MIN_HEIGHT`) is a floor, not a cap; it grows for wrapped or
  multi-line (real `\n`) drafts.
- The caret is drawn (only while `active()`) via
  `TextLayout::hit_position(byte_offset)` against that *same* wrapped
  layout — the same primitive floem's own `text_input` uses for its
  cursor x (`views/text_input.rs`'s `clip_text`). Because it's one
  `TextLayout` used for both painting the glyphs and hit-testing the
  cursor position, there is no second, independently laid-out buffer that
  could drift out of sync with the visible wrap — the alternative
  considered (a wrapping `label` plus a separately positioned
  absolute-offset caret overlay, built from its own matching
  `TextLayout`) would have needed to keep two layouts' width/attrs in
  exact lockstep by hand.
- The placeholder ("Message agent...") substitutes for `text` when both
  `draft.text` and `preedit` are empty, with `theme::text_subtle()`
  instead of `theme::text_primary()` — it renders through the exact same
  `TextLayout`/caret path, so `active()` still draws a caret at its start.

`ViewId::new_taffy_node()`/`set_taffy_style()`/`taffy_layout()` (all public
on `floem::ViewId`) are what make this kind of custom `View`, with its own
manual, measured-content leaf node, possible from outside the `floem`
crate itself — the crate-internal widgets (`Label`, `TextInput`, `Img`)
reach for the private `ViewId::taffy()` directly, which app code cannot
call.

`Shift+Enter` now inserts `"\n"` via the existing
`AgentDraftAction::Insert`/`insert_agent_draft_text` path
(`app::keymap::agent_draft_action`, checked before the plain-`Enter`
arm since both match `NamedKey::Enter`) instead of being indistinguishable
from plain `Enter`. Cursor movement, Backspace, and insertion already
treated the cursor as a byte offset with no awareness of what character
sits at any position, so `\n` needed no special-casing there — it moves
and deletes exactly like any other single-byte character (see
`workspace::input::tests::newline_from_shift_enter_behaves_like_an_ordinary_character`).

This intentionally does not support mouse click-to-position-cursor or text
selection — out of scope for the bugs above, and both are still absent
from the composer today. `floem::views::text_input` remains unused for the
same two reasons as before (IME, single-target keyboard dispatch) —
neither reason was about single-line-vs-multiline, so nothing about
wrapping or `Shift+Enter` changes that conclusion.
