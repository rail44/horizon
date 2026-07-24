---
id: 007
title: Terminal smooth scrolling falls back to line steps in a maximized single-pane tab
status: open
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
