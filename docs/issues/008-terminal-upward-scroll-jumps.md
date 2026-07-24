---
id: 008
title: Smooth terminal scrolling makes abnormal jumps only when moving upward
status: open
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
