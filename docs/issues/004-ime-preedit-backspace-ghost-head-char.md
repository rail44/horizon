---
id: 004
title: Terminal IME — with conversion candidates showing, backspace leaves the preedit's head character as an undeletable ghost that the next input overwrites
status: resolved
severity: high
area: terminal
---

## Resolution (2026-07-19)

Historical note (2026-07-22): the custom platform and its platform-side
regression tests named below were subsequently retired with the move back to
`gpui_platform`; those paths now live only in git history. Horizon's
`ime_marked_text_for` contract tests remain. Native preedit delivery is again
owned by GPUI's maintained per-OS backend.

Root cause was on the winit event-mapping side, not the terminal's
overlay state: `crates/horizon-winit-platform/src/window.rs`'s
`handle_ime` special-cased an empty `Ime::Preedit` by skipping the call
into `replace_and_mark_text_in_range` entirely (reasoning that winit
always emits an empty `Preedit` immediately before the `Commit` that
finalizes a composition, and forwarding it early would make that
`Commit`'s own `replace_text_in_range(None, text)` double-insert). That
reasoning only holds for `unmark_text()` (never called here either way);
`replace_and_mark_text_in_range(None, "", None)` is safe even right
before a `Commit` — verified against both `EntityInputHandler`
implementations this crate drives (this crate's terminal, and
gpui-component's `InputState`) and against gpui_linux's own wayland
`Dispatch` for `zwp_text_input_v3`, which forwards `SetMarkedText`
unconditionally regardless of emptiness. So backspacing a composition to
nothing *without* an immediately-following commit (composition
continues, awaiting more kana — the owner's exact repro) never told the
input handler the preedit had shrunk, stranding the last non-empty text
as a paint-time ghost until the next composing update overwrote it.

Fix: `handle_ime`'s `Ime::Preedit` arm now forwards `text` to
`replace_and_mark_text_in_range` unconditionally, including empty — only
IME candidate-window repositioning stays gated on non-empty text (an
unrelated GNOME feedback-loop guard). The decision is pulled out as a
pure `preedit_forward` function so it's unit-testable without a live
winit window. `src/terminal/mod.rs`'s own state update
(`ime_marked_text_for`) was already correct; it's now also extracted as
a pure function purely to pin the overlay's contract from that side too.

Regression tests (mirroring the owner's exact repro, "あいう" -> "あい"
-> "あ" -> ""): `crates/horizon-winit-platform/src/window.rs`'s
`preedit_forward_never_drops_the_shrink_to_empty_step` and
`preedit_forward_repositions_only_for_nonempty_text`; `src/terminal/tests.rs`'s
`preedit_backspace_to_empty_clears_the_marked_text` and
`cleared_marked_text_paints_nothing`. Only the owner's own live IME
dogfooding can confirm the fix end-to-end through the real winit/IME
pipeline — headless key/IME injection isn't available on this shared
desktop.

## Repro

1. Focus a terminal pane and activate the Japanese IME.
2. Type kana so that a preedit with conversion candidates is showing.
3. Press backspace repeatedly to shrink the composing text.
4. Type again.

## Observed

Backspace appears unable to erase the *first* character of the
composing text: shrinking the preedit deletes the tail characters, but
the head character stays painted at the cursor cell. Typing again then
paints the new composing text over that ghost head character —
"上書きされる" — rather than the display starting from a cleanly
cleared preedit region.

## Expected

The painted composing text always mirrors the IME's actual preedit
string: backspacing to a shorter preedit (including all the way to
empty) fully clears what was previously painted, leaving no residual
head character.

## Pointers (recorded, not diagnosed)

The preedit is client-side only — painted as an overlay at the cursor
cell and never sent to the PTY (`src/terminal/mod.rs`, the IME preedit
handling around its input path and the overlay paint site). The winit
side delivers `Ime::Preedit` updates via
`crates/horizon-winit-platform/src/window.rs`. Whether the ghost is a
stale overlay clear (paint path) or a dropped/short preedit update
(event path) is the first thing to establish —
`HORIZON_INPUT_TRACE` traces the key/IME pipeline hops.

Filed 2026-07-19 from owner dogfooding, relayed through the project
session.
