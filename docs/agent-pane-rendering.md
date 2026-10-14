# Agent Pane Rendering

What the agent pane draws and how it is fed. The code is `src/agent/view/`.

## What the pane draws

The pane is one scrolling column above a composer. The column is a run of
units, with no speaker labels:

- **Message** — something sent into the session: by the owner (`You`), by a
  background task (`Task`), or as an automatic continuation (`Continue`).
  Plain text, indented, on a grey tint; task and continuation messages are
  muted.
- **Turn** — one agent turn as one bordered card. Its parts sit in the
  order they happened: each piece of text as Markdown, each receipt as one
  line where its tool calls ran, and the thinking state last. A receipt
  line reads its burst's aggregate (`Ran 3 commands · 1 search`, `Edited 2
  files`), with an `{n} failed` chip when a call failed; consecutive bursts
  with no text between them are merged by the adapter into one receipt
  (the calls appended, the aggregate recomputed over all of them, the
  first one's key kept), so a run of bursts reads as one line. While a
  call in the burst is in flight, the line names it instead — the last one
  in flight that is not waiting on an approval, as `Running cargo test`,
  `Reading src/a.rs` — and switches to the aggregate once the burst ends.
  Nothing opens or closes by itself: a receipt line is collapsed until the
  owner clicks it, and its open state is held per receipt key, so it
  survives re-adaptation. An open line shows its calls under a left rule:
  one row per call, a status dot, the verb muted and the target in the
  monospace family. A click on a row opens its body: a diff with its
  `+`/`−` in a gutter cell, a command's output unboxed, or one line of
  text. A pending approval is one line, `Approval needed`, the reason,
  `Enter` `Approve`, `Esc` `Deny`: under its call when the receipt line is
  open, under the line itself while it is collapsed. While a turn runs it
  carries `Thinking` dots when it has no text yet or a thought is
  streaming.
- **Notice** — a plain line in a tone: errors and exits (danger), rate
  limits and a missed memory checkpoint (warning), compaction and memory
  digests (muted).

State and controls live on the running turn: its running receipt line
(the last receipt, while a call in it is in flight) or, with no running
receipt, its thinking line opens with the session's state chip (`Running`
/ `Waiting for approval`) and ends with `Stop`, and an accent bar runs down
the card's left edge. A finished turn ends with a right-aligned muted line
after its last part holding a `Stopped` / `Failed` chip when it did not
complete and its elapsed time; a card with no parts shows only that line.
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

`previews.rs` registers two named previews over one sample transcript, a
long turn of text and tool calls interleaved (two of its bursts merged
into one `Ran 4 commands · 1 search` line) followed by the owner's next
request: `agent` (a turn running, its receipt line naming the call in
flight and an approval pending under it) and `agent-idle` (the session
idle, the receipt holding the long turn's edit open with its diff and its
failed command's output). `scripts/check-preview-plugin.sh` checks them
windowlessly.

## Retired rendering

`docs/agent-output-ui-amendment.md` and the transcript parts of
`docs/agent-pane-design.md` ("UI And UX") describe the rendering this pane
replaced (labelled messages, running card, per-burst receipts, changes
overview bar, virtualized message scroller).

## Follow-up

The column is eager: every unit is built on every render, unlike the
virtualized scroller it replaced. Long sessions will need virtualization.
