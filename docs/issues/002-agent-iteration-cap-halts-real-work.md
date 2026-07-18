---
id: 002
title: Agent halts mid-task at "25 consecutive tool-driven turns" with no way to continue but retyping
status: resolved
severity: high
area: agent
---

## Repro
1. Open an agent pane.
2. Give it a task that genuinely needs a lot of tool calls (read several
   files, grep, edit, run a build).
3. Let it work without interrupting.

## Observed
After 25 tool-driven turns the session stops and the transcript shows an
error:

> Stopped after 25 consecutive tool-driven turns without a new user
> message. The agent may be stuck in a loop — send a new message to
> continue.

The agent was not stuck — it was making progress. The work is abandoned
mid-task, the halt is presented as an error (indistinguishable from a real
failure), and the only way forward is to type another message.

## Expected
A task that is making progress should not be cut off. If a runaway-loop
guard has to exist, it should not fire on ordinary work: the limit should
be high enough (or adaptive), the halt should read as a pause rather than
an error, continuing should take one action rather than composing a
message, and the limit should be raisable without restarting the agent
runtime.

## Notes

This is not a code defect — it is the turn-loop iteration cap doing exactly
what it is designed to do (`docs/agent-tools-design.md`, "Error Model and
Loop Guards"). The problem is that the design's safety net is tuned so
tightly that it catches normal agentic work. Filing it here because the
behavior, as shipped, makes long tasks unusable.

Mechanism, for triage:

- `TurnLoopGuard::record_tool_turn`
  (`crates/horizon-agent/src/providers/rig/session.rs:577`) increments a
  counter once per *tool-driven turn* — one increment per tool batch, not
  per tool call — and halts on the `iteration_cap + 1`-th. The counter is
  reset only by `Command::UserMessage`
  (`session.rs:196`) and by the halt itself (`session.rs:658`); nothing
  else clears it. So the cap is "tool batches in a single interaction",
  and 25 of those is an ordinary amount of work for a coding agent.
- `halt_turn_loop` (`session.rs:633`) records the tool result that arrived,
  emits `Event::Error`, cancels any *other* still-pending calls in the
  batch (synthesizing cancelled results so `rig_history` stays API-valid),
  emits `TurnEnded(TurnEndReason::Halted)`, and returns the session to
  `WaitingForUser`. On the iteration-cap path there are no other pending
  calls (the cap is only recorded once the whole batch has landed), so
  nothing in flight is lost — but on the doom-loop path there can be.
- The halt is surfaced to the UI as `Event::Error` only.
  `TurnEndReason::Halted` exists and is persisted (the DuckDB projection
  maps it to `"halted"`), but no view distinguishes a guard halt from a
  provider error, so the pane reads as "the agent failed".
- Continuing works — a new user message resets the guard and the last tool
  result is already in `rig_history` — but there is no `CommandId` for it.
  The user must compose and send text.
- `iteration_cap` is configurable (`[agent] iteration_cap`, default 25 via
  `DEFAULT_ITERATION_CAP`; see `config.example.toml:76`), but only
  `horizon-agentd` reads it, once, at process start
  (`crates/horizon-agentd/src/main.rs:96`). `Reload Config` does not apply
  it — per `AGENTS.md`, that command applies `[theme]` and `[keybindings]`
  only. Raising the cap therefore needs `Reload Agent Runtime` (which
  respawns the daemon) or an app restart. `role_adjusted_config`
  (`crates/horizon-agent/src/providers/rig/mod.rs:66`) can override a
  role's model and allowed tools but not its loop-guard limits.

Adjacent, same design section and worth deciding together: the doom-loop
guard's default `doom_loop_window = 3` halts after three identical
`(tool, args, output)` fingerprints. Three is aggressive — e.g. a `bash`
tool re-running the same command with the same output three times in one
interaction is not obviously a loop.

## Triage

Priority: **high** — the owner hit this mid-task during real use, which is
exactly the dogfooding signal; as shipped it makes long agent tasks
unusable. But it is **larger and not mechanically dispatch-ready**: not a
code defect, it is the turn-loop guard's tuning catching normal agentic
work, and the fix spans five things that want to be decided together:

- (a) the `iteration_cap` value / whether it becomes adaptive;
- (b) surfacing `TurnEndReason::Halted` distinctly from `Event::Error` so a
  guard pause does not read as a provider failure (UI);
- (c) a first-class **Continue** command (`CommandId`) so resuming is one
  action, not composing a message;
- (d) making `iteration_cap` apply without a runtime respawn — today only
  `horizon-agentd` reads it once at start and `Reload Config` does not
  apply it;
- (e) the adjacent `doom_loop_window = 3`, aggressive for legitimate
  repeated commands.

(a)/(c)/(d)/(e) are design judgments that belong in a **short owner
consult** plus an update to `docs/agent-tools-design.md` ("Error Model and
Loop Guards"); (b) is UI. Recommendation: a design pass **before** dispatch,
not handing the raw issue to a worker. Touches `crates/horizon-agent`
(`providers/rig/session.rs`), `horizon-agentd` (`main.rs`), and agent UI
views; no conflict with the in-flight terminal-cwd work.

Worker dispatch waits on the owner's timing, and after the design pass.

**Dispatched 2026-07-18** after the owner approved the design in the
config-narrowing consultation: guard thresholds become built-in
constants (consecutive tool turns 25→100, doom-loop window 3→5 — the
`[agent]` config keys are being retired in the same wave), a guard halt
renders as a neutral "paused" receipt row instead of an error, and a
parameterless Continue command (+ paused-row button) resumes without a
new user message. Decisions (a)/(d)/(e) above were resolved by the
retirement (no runtime knob remains); (b)/(c) are the dispatched work.

## Resolution (2026-07-18)

Owner-approved design implemented directly (items a-d above; e folded into
a); see `docs/agent-tools-design.md`'s "Error Model and Loop Guards" for
the durable record.

- **(a) Thresholds.** `iteration_cap` 25 → 100; `doom_loop_window` 3 → 5.
  Both are now fixed built-in constants
  (`crates/horizon-agent/src/config.rs`'s `DEFAULT_ITERATION_CAP`/
  `DEFAULT_DOOM_LOOP_WINDOW`), no longer read from `[agent]
  iteration_cap`/`doom_loop_window` in the config file — those keys stay in
  the file schema for now (parse harmlessly, do nothing) since retiring
  them outright is a separate follow-up wave. This also resolves (d): a
  fixed constant needs no runtime-respawn story.
- **(b) Halt reads as a pause.** `TurnEndReason` gained
  `HaltedByIterationCap`/`HaltedByDoomLoop` (the old bare `Halted` stays,
  for pre-resolution persisted logs, and still renders calmly). A guard
  halt no longer emits `Event::Error` at all — only `Event::TurnEnded`
  with the specific reason. The transcript's receipt row
  (`src/agent/turns/receipt.rs`) renders it with the same calm, non-error
  styling as a cancelled turn, with text naming the guard and its
  threshold (e.g. "paused after 100 consecutive tool-driven turns").
- **(c) Continue is one action.** New parameterless `CommandId::
  ContinueAgentTurn` (mirrors `CancelAgentTurn`), a button on the paused
  receipt row itself, palette entry, and CLI/control-plane parity
  (`horizon continue-turn <session-id>`, mirroring `cancel-turn`). Wire:
  `Command::ContinueTurn`, handled by `providers::rig::session::
  run_session_loop`: a halt stashes the real tool result that tripped it
  (`pending_halt_result`) instead of folding it into `rig_history`
  immediately; `ContinueTurn` consumes it, resets the guard, and resumes
  exactly as if the guard had never tripped (same code path a normal
  tool-result-driven turn takes); a plain new user message flushes the
  same stash into history first, so typing past a halt still works.
  Replay safety: `pending_halt_result` is pure in-memory session-loop
  state, never persisted, so a resumed/replayed halted session sits at
  `WaitingForUser` without auto-continuing.

Touched: `crates/horizon-agent` (`config.rs`, `contract.rs`,
`providers/rig/session.rs`, `providers/mock.rs`, `frame.rs`),
`crates/horizon-workspace` (`commands.rs`), `src/agent/session.rs`,
`src/agent/turns/receipt.rs`, `src/agent/view.rs`, `src/workspace.rs`,
`src/control_plane.rs`, `crates/horizon-cli` (`cli.rs`, `commands.rs`),
`docs/agent-tools-design.md`, `config.example.toml`.

Merged to main by the project session 2026-07-18.
