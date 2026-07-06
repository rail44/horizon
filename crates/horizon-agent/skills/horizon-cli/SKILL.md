---
name: horizon-cli
description: Operate Horizon itself via the `horizon` CLI -- open panes/terminals, attach sessions, and run commands in the workspace this agent lives in. Use whenever the task calls for changing the workspace around you (new terminal/agent pane, split, attach, terminate, approve/deny a pending tool call) rather than just editing files.
---

# Operating Horizon via its own CLI

You are running inside a Horizon agent pane. The `horizon` binary doubles as
a control-plane client: `horizon <subcommand> ...` talks to the running
Horizon instance over its control socket and exits — it does not launch a
second GUI.

## Environment is already set up

Your `bash` tool inherits this pane's process environment, which already
has:

- `HORIZON_SOCKET` — the control socket path. `horizon` finds it
  automatically; you never need to pass `--socket` yourself.
- `HORIZON_SESSION_ID` — this pane's own session id, the "here" target for
  `--split`.

So every command below can be run as plain `horizon <subcommand> ...` with
no extra flags for discovery.

## Orientation first

Before creating or targeting anything, look at what's already there:

- `horizon sessions` — lists every known session: id, kind (terminal/agent),
  attached/detached, title.
- `horizon state` — workspace-level facts: tab count, visible pane count,
  whether something is active, how many sessions are detached, whether a
  turn/approval is in flight, and `destructive_commands` (which subcommands
  need `--yes` — see below).

Add `--json` to either for the raw structured payload instead of the
human-readable summary.

## Creating panes

- `horizon new-terminal [--split [<session-id>]] [--active]` — a new
  terminal pane.
- `horizon new-agent [--prompt <text>] [--split [<session-id>]] [--active]` —
  a new generic agent session, optionally seeded with an initial prompt.
- `horizon new-config-agent [--prompt <text>] [--split [<session-id>]] [--active]` —
  a new configuration-agent session (the `config` role: edits Horizon's
  theme/keybindings via its own `horizon-config` skill). Use this instead of
  `new-agent` when the task is specifically about Horizon's own config file.

`--split` placement:

- Bare `--split` means "next to this pane" — resolved from
  `HORIZON_SESSION_ID`, i.e. split off the pane you're running in.
- `--split <session-id>` splits off a specific session instead.
- Omitted entirely, the new pane opens wherever Horizon's default placement
  puts it (not necessarily next to you).

`--active` focuses the new/attached pane for the human at the keyboard.
Leave it off for a pane you're creating for your own use — stealing focus
from someone actively working is disruptive; only pass it when the task is
specifically about surfacing something for the user to look at right now.

## Attaching, terminating, and turn control

- `horizon attach <session-id> [--active]` — attach a detached session to
  the visible layout.
- `horizon terminate-session <session-id>` — close and delete one session.
  **Destructive** — pass `--yes` to confirm non-interactively (there is no
  TTY to prompt from bash), or the command refuses to run.
- `horizon terminate-all-detached` — same, for every detached session at
  once. Also destructive; also needs `--yes`.
- `horizon cancel-turn <session-id>` — cancel a session's in-flight turn.
- `horizon approve <session-id> <call-id>` / `horizon deny <session-id> <call-id>` —
  resolve another session's pending tool-call approval (the call id comes
  from that session's own event stream/state, not from this skill).

Check `horizon state`'s `destructive_commands` list if unsure whether a
subcommand needs `--yes` — the server, not this skill, is the source of
truth for which ones are destructive in this build.

## Runtime/config reload

- `horizon reload-agent-runtime` — respawn `horizon-agentd` (recovers from a
  stale/rebuilt agent binary).
- `horizon reload-config` — re-read Horizon's config file (theme/keybindings
  apply live; everything else needs a restart regardless).

## Exit codes and `--json`

Exit 0 on success, 2 on a usage error (bad flags/arguments), 1 on a
connection/server failure or a destructive command that was refused for
lacking `--yes`. Pass `--json` on any subcommand to get the raw contract
payload instead of the formatted summary — useful when you need to parse a
field programmatically rather than read it.
