---
id: 001
title: Agent composer ignores a trailing Shift+Enter — box does not grow and the caret stays on line 1
status: triaged
severity: high
area: agent
---

## Repro
1. Open an agent pane and focus the composer.
2. Type `abc`.
3. Press `Shift+Enter`.

Variant (does not self-heal):

1. Type `abc`, press `Shift+Enter` twice, then type `x`.

## Observed
After step 3 nothing visibly changes: the composer box keeps its
single-line height, and the caret stays at the end of `abc` on the first
line. Typing any further character makes the box grow to two lines and
moves the caret onto the second line.

In the variant, the box does grow to three lines, but the caret still
renders at the end of `abc` on the first line — and stays there no matter
what is typed afterwards.

## Expected
`Shift+Enter` inserts a newline (it does — the draft text is correct), so
the composer should immediately grow by one line and put the caret at the
start of the new, empty last line. The caret should track `draft.cursor`
on any line, including empty ones.

## Notes

Environment: any; reproduces deterministically. The draft model is fine —
`agent_draft_action` (`src/app/keymap.rs:61`) maps `Shift+Enter` to
`AgentDraftAction::Insert("\n")` and `insert_agent_draft_text`
(`src/workspace/input.rs:135`) advances `draft.cursor` past it. The bug is
purely in how that text is laid out and how the caret is placed:
`ComposerTextView` (`src/workspace/view/composer_text.rs`) sizes itself
from `TextLayout::size()` and draws the caret at
`TextLayout::hit_position(cursor)`.

Both of those `floem_renderer::text::TextLayout` primitives mishandle
empty lines. Measured directly against the pinned floem revision
(`renderer/src/text/layout.rs`, cosmic-text 0.14.2), with the composer's
own font size and line height:

| text | `lines()` | `size().height` | `hit_position(len)` |
| --- | --- | --- | --- |
| `"abc"` | 1 | 14.40 | (19.87, 11.86) |
| `"abc\n"` | 1 | 14.40 | (19.87, 11.86) |
| `"abc\nx"` | 2 | 28.80 | (6.35, 26.26) |
| `"abc\n\n"` | 2 | 28.80 | (19.87, 11.86) |
| `"abc\n\nx"` | 3 | 43.20 | (19.87, 11.86) |

Two independent defects, each mapping onto one half of the report:

1. **A trailing newline produces no line.** `TextLayout::set_text` builds
   its `BufferLine`s from cosmic-text's `LineIter`, which yields
   `("abc", Lf)` for `"abc\n"` and then stops — there is no final empty
   range. So `"abc\n"` lays out byte-for-byte like `"abc"`: the height
   never grows, and there is no line for the caret to sit on. This is why
   the box only grows once a *further* character creates a real second
   line.

2. **`hit_position` cannot advance past a glyph-less run.** Its
   `offset` accumulator only advances via `offset += last_end + 1`, where
   `last_end` comes from the previous run's last *glyph*. An empty line
   contributes no glyphs, so `offset` is never advanced for it and stays
   stale for every run after it. Any cursor index that falls past an empty
   line (or past the last glyph entirely) hits the `if idx > 0` fallback
   at the end and resolves to "just after the last glyph seen" — the end
   of line 1. This is the variant that never recovers, and it also means
   the caret is misplaced anywhere after a blank line in a multi-paragraph
   draft.

Both are upstream (`lapce/floem` @ `31fa8f4`), not in Horizon's code, so a
fix here has to work around them — e.g. lay out a sentinel-terminated
string when the draft ends in `\n`, and derive the caret's line/x from
`lines_range()` + `layout_runs()` rather than trusting `hit_position` on
text with empty lines. `docs/agent-composer-cursor-design.md` should be
updated with whichever workaround is chosen, since it currently records
`hit_position` as the trusted primitive.

## Triage

Priority: **high.** The composer is a core, constantly-used surface; the
repro is deterministic and any multi-line draft (Shift+Enter) is visibly
broken — the box does not grow and the caret desyncs from `draft.cursor`.

**Dispatch-ready.** The filing session already isolated the two upstream
floem defects and named a concrete workaround path (lay out a
sentinel-terminated string when the draft ends in `\n`; derive the caret's
line/x from `lines_range()` + `layout_runs()` instead of trusting
`hit_position` on text with empty lines), so a worker can take this without
a further design pass. The fix must update
`docs/agent-composer-cursor-design.md`, which currently records
`hit_position` as the trusted primitive.

**Pairs with backlog 24** (composer IME candidate-window placement) — same
file `src/workspace/view/composer_text.rs`, same `hit_position` primitive;
doing them together avoids touching that layout code twice. No conflict
with the in-flight terminal-cwd work.

Worker dispatch waits on the owner's timing (per the issues flow: the
project session triages; the owner directs when a fix worker launches).
