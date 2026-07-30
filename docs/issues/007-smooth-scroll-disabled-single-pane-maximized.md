---
id: 007
title: Terminal smooth scrolling falls back to line steps in a maximized single-pane tab
status: resolved
severity: medium
area: terminal
---

## Repro

1. Open a tab containing a single terminal pane.
2. Maximize the Horizon window.
3. Produce enough terminal history to scroll.
4. Scroll through the terminal.

## Observed

Smooth scrolling no longer works in this layout. The terminal moves in
discrete line-sized steps.

## Expected

Terminal scrolling should remain smooth when a single-pane tab occupies a
maximized window, just as it does in other window and pane layouts.

## Notes

Filed 2026-07-25 from owner dogfooding.

## Resolution

Root cause: `max_window_rows` in `crates/horizon-terminal-core/src/core/render.rs`
computed the byte-safe row count as `(EVENTS_ITEM_CAP_BYTES / 2) / (columns *
128)` and then applied `.max(screen_lines)` as a floor. When a single pane
was tall enough that `screen_lines` exceeded the byte-budget row count (e.g.
>= 136 rows at 120 cols, >= 81 at 200 cols), the floor forced
`max_window_rows == screen_lines`, so the served window had zero overscan
margin — the viewport was pinned at `viewport_offset == 0` with no look-ahead
rows above or below. Every wheel tick then re-fetched a fresh window
(edge fetch), regressing smooth scroll to line steps.

Fix: the byte budget now uses the entire events cap (`EVENTS_ITEM_CAP_BYTES`,
not half) — the ~20 % headroom between the conservative `WORST_CASE_BYTES_PER_CELL`
(128) and the measured real worst case (~107 B/cell) is the framing headroom
the half-budget used to provide. On top of that, a fixed `OVERSCAN_ROWS = 64`
margin is guaranteed above and below the viewport: the formula is
`(screen_lines + OVERSCAN_ROWS).min(budget_rows).max(screen_lines)`, which
serves full overscan when the budget allows it, clips to partial overscan
when the viewport is tall, and collapses to zero overscan only when a single
screen alone fills the cap (the extreme-pane fallback — no less safe than the
live-frame watch, which carries one screen at the same cap).

Tests: `snapshot_window_serves_overscan_for_a_tall_single_pane`,
`snapshot_window_drops_overscan_when_the_viewport_fills_the_byte_budget`
(`crates/horizon-terminal-core/src/tests.rs`),
`a_worst_case_scroll_window_stays_under_the_events_cap`
(`crates/horizon-session-protocol/tests/limits.rs`, updated to the new
budget). GUI verification pending by the integrator.
