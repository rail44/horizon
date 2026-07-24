---
id: 005
title: Reload Session Runtime discards every existing agent session
status: open
severity: high
area: agent, sessiond
---

## Repro

1. Keep one or more existing agent sessions in the workspace.
2. Run `Reload Session Runtime`.
3. Inspect the workspace and session list after the runtime reconnects.

## Observed

The old agent sessions are all discarded. In the observed run, the terminal
session remained but none of the pre-reload agent sessions remained available
in the workspace session list.

## Expected

Reloading the session runtime should replace the runtime process without
unconditionally throwing away every existing agent session. Sessions that can
be restored should remain available after the reload.

## Notes

Filed 2026-07-25 from owner dogfooding while reloading the runtime to verify an
agent-provider fix.
