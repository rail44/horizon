---
id: 008
title: Smooth terminal scrolling makes abnormal jumps only when moving upward
status: resolved
severity: medium
area: terminal
---

## Repro

1. Open a terminal with enough scrollback to move in both directions.
2. Scroll smoothly downward through the history.
3. Reverse direction and scroll upward.

## Observed

Upward scrolling makes abnormal large jumps. The corresponding downward
movement does not show the same behavior.

## Expected

Upward and downward smooth scrolling should advance continuously without
direction-specific jumps.

## Notes

Filed 2026-07-25 from owner dogfooding.

## Resolution

Root cause: the same zero-overscan window that caused issue 007. With
`viewport_offset == 0` and `max_top == 0`, the client's `on_wheel` handler
(`src/terminal/session.rs`) tripped into the top-edge branch on every upward
tick (`new_position < 0`), setting `*offset = 0; *fractional_row = 0.0` and
issuing an edge fetch — the hard reposition to the top of a freshly-fetched
window is the visible jump. Downward scrolling did not jump because
reaching the live edge (`below == 0`) drops the window and resumes the live
frame, which is already available locally — a smooth transition.

The client-side fractional-offset preservation (prefetch install, edge-fetch
rebase) was already correct; the bug was purely the absence of overscan to
absorb the gesture locally. Fixing 007 (serving windows with a fixed
`OVERSCAN_ROWS` margin) resolves 008: with `viewport_offset > 0` and
`max_top > 0`, a small upward scroll stays inside the block as a local move
(no edge fetch, no snap).

Tests: `upward_scroll_in_a_tall_panes_overscanned_window_is_local`
(`src/terminal/session.rs`) pins the upward/downward symmetry. GUI
verification pending by the integrator.
