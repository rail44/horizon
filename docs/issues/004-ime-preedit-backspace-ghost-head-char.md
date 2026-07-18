---
id: 004
title: Terminal IME — with conversion candidates showing, backspace leaves the preedit's head character as an undeletable ghost that the next input overwrites
status: open
severity: high
area: terminal
---

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
