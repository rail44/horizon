---
id: 005
title: Reload Session Runtime discards every existing agent session
status: resolved
severity: high
area: agent, agentd
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

## Correction (2026-07-25, live verification)

A reload observed today behaved opposite to the Observed section above:
the **agent** sessions survived (still listed and attachable afterwards)
and the **terminal** sessions were discarded (a Claude Code instance
running in a terminal pane was killed mid-session; its pane reopened with
a fresh terminal and a stale `HORIZON_SESSION_ID` in the environment).
Either the behavior changed between filing and today (session
resume/re-adoption work landed 2026-07-21+), or the original observation
misattributed which kind was lost. The issue remains real — reload still
destroys live sessions a user cares about — but the fix target is the
terminal side, and the title's claim about agent sessions no longer
reproduces.

## Resolution (2026-07-30, verified 2026-09-13)

The runtime split merged in `5a98b2c` resolved both versions of the finding.
`Reload Agent Runtime` now drains and respawns only `horizon-agentd`, replays
and re-adopts its persisted agent sessions, and leaves every terminal session
untouched. Terminal ownership moved to `horizon-terminald`; replacing that
binary is now a separate `Reload Terminal Runtime` command whose destructive
effect on PTYs is explicit in the command model and UI.

The original agent-loss claim was already contradicted by the correction
above, and the later-observed terminal collateral damage no longer occurs on
an agent-runtime reload. A deliberately requested terminal-runtime reload is
a different, explicitly destructive operation rather than hidden collateral
damage from the command described by this issue.
