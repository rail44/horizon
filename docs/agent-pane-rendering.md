# Agent Pane Rendering

What the agent pane draws and how it is fed. The code is `src/agent/view/`.

## What the pane draws

The pane is one scrolling column above a composer. The column is a run of
units, with no speaker labels:

- **Message** — something sent into the session: by the owner (`You`), by a
  background task (`Task`), or as an automatic continuation (`Continue`).
  Plain text, indented, on a grey tint; task and continuation messages are
  muted.
- **Turn** — one agent turn as one bordered card. Its receipts merge into
  one line at the card's head (`Read 6 files · Edited 3 files · Ran 2
  commands · 2 searches`, with an `{n} failed` chip when a call failed),
  followed by its text as Markdown. A click on the receipt line opens its
  calls under a left rule: one row per call, a status dot, the verb muted
  and the target in the monospace family. A click on a row opens its body:
  a diff with its `+`/`−` in a gutter cell, a command's output unboxed, or
  one line of text. A pending approval is one line under the call that asks
  for it: `Approval needed`, the reason, `Enter` `Approve`, `Esc` `Deny`.
  While a turn runs it carries `Thinking` dots when it has no text yet or a
  thought is streaming.
- **Notice** — a plain line in a tone: errors and exits (danger), rate
  limits and a missed memory checkpoint (warning), compaction and memory
  digests (muted).

State and controls live on the running turn: its receipt line (or, with no
receipt yet, its thinking line) opens with the session's state chip
(`Running` / `Waiting for approval`) and ends with `Stop`, and an accent
bar runs down the card's left edge. A finished turn's line ends with a
`Stopped` / `Failed` chip when it did not complete and its elapsed time.
The column ends with a `Session ended` / `Runtime unreachable` notice when
the session can no longer run, then one line per background task.

The composer is one bordered box holding the input field (one row, growing
to eight), the model chip (a click switches the model), and `Enter`
`to send`. Enter sends; with an empty field and a pending approval, Enter
approves and Escape denies it.

## How it is fed

- `display.rs` is the display model: plain, already-worded data. The view
  (`AgentView`, `mod.rs` + `units.rs`) draws only that model and its own UI
  state (which receipts and call bodies are open), and builds for the
  preview plugin's wasm target.
- Everything the owner asks for leaves the view as an `AgentViewEvent`
  (`Send`, `Approve`, `Deny`, `Stop`, `SwitchModel`).
- The only native code is `adapter.rs` and `pane.rs`. The adapter builds the
  display model from the session's folded frame (`agent::model`'s turn
  spans, bursts, call views and receipt aggregate; `agent::turns`' per-tool
  bodies) and maps each approval id back to its tool-call identity.
  `AgentPane` re-adapts on every session notify (and once a second while a
  background task runs), sends messages and approvals to the session, and
  dispatches `Stop` / `SwitchModel` as `CancelAgentTurn` / `SwitchModel`
  commands.

## Previews

`previews.rs` registers two named previews over one sample transcript:
`agent` (a turn running with an approval pending at its tail) and
`agent-idle` (the session idle, the last turn's receipt, first diff and
failed command output open). `scripts/check-preview-plugin.sh` checks them
windowlessly.

## Retired rendering

`docs/agent-output-ui-amendment.md` and the transcript parts of
`docs/agent-pane-design.md` ("UI And UX") describe the rendering this pane
replaced (labelled messages, running card, per-burst receipts, changes
overview bar, virtualized message scroller).

## Follow-up

The column is eager: every unit is built on every render, unlike the
virtualized scroller it replaced. Long sessions will need virtualization.
