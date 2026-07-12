# Agent Output UI — Transcript Revision (Amendment)

Status: owner-approved 2026-07-12, decided in a Claude Design review
session. Amends `docs/agent-output-ui-design.md` (the approved
transcript redesign): everything not restated here — per-tool
renderers, diff theme roles, follow-scroll, density rules — stays as
designed there. Decision 8 of that doc (inline approval, slice 4) is
**superseded** by decision 4 below.

Mocks: the review canvas is vendored at
`docs/assets/agent-ui-options/agent-ui-options.html` (open locally in
a browser; `support.js` sits next to it). The source project, including
per-decision PNG snapshots, is
<https://claude.ai/design/p/41be601d-2ef0-4ac1-9d82-97eaef777f3c>.
Option ids referenced below (2a, 3b, 4b, 5a, 6a, 7a…) are anchors in
that canvas. The mocks are drawn on a light theme for review
legibility only — implementation goes through theme roles as usual
(no hardcoded colors, per the base doc's invariants).

## Current-state note (GPUI shell, 2026-07-12)

This amendment was written against the Floem-era base design, but the
GPUI migration (see `docs/gpui-migration-design.md`) rebuilt the agent
pane lean and did **not** port base slices 1–5. Read the decisions
with these corrections (owner-confirmed 2026-07-12):

- "Reuses the base slice-1 renderers / turn footer / Changes
  overview": those exist only in the retired Floem shell. They are
  built **new**, directly in this amendment's final shape (per-tool
  renderers become the running card's rows and the receipt's expansion
  bodies) — no intermediate always-visible-tool-block stage is built.
- "+N more indicator, as today": the oldest-first queue *logic*
  survives (`frame::pending_approval_call_ids_in`); the indicator UI
  was Floem-only and is built new in the approval-mode composer.
- "AgentPaneFocus still applies": no such focus context exists in
  GPUI. Approval-mode key capture lives in the composer's own state
  (Enter = allow, esc = deny, typing reverts to normal input).
- The base doc's "trailing-window (200 blocks) + revision memoization"
  invariant was deliberately discarded in the GPUI shell; the live
  invariant is GPUI-native: expansion state stays view-local, and
  per-token work stays O(visible), never O(whole-log).
- "Cancellation is a stop reason, as implemented today": true in the
  model (`TurnEndReason::Cancelled`); the receipt/chip rendering is
  new UI.
- The `retry ×N` chip (decision 5) is **deferred with its data**: the
  runtime has no retry concept in the contract; the chip returns when
  one exists.
- gpui-component assets are reused wherever they fit (owner direction,
  reconfirmed): theme via the single `Scheme` seam in `src/theme.rs`
  (see the Contract addendum below and the module doc), components
  (buttons, lists, badges) surveyed before hand-rolling.

Implementation proceeds as staged branches, each owner-confirmed:
A contract groundwork, B theme roles (both invisible), then C running
card + receipts, D inline expansion, E approval-mode composer,
F failure display + stop button (each visually confirmed).

## Decisions

### 1. Turn receipts — a completed turn's tool activity collapses to one line (extends the base design)

- On top of the existing per-call collapsed blocks, add a
  **turn-level aggregation row**: when a turn completes, its tool
  calls and diffs fold into a single receipt line, e.g.
  `▸ grep ✓ · fs.read ✓ · [login_form.rs +2 −1] · bash cargo test 12 ✓ · 38s`
  (mock 3b: a row of pill-shaped chips; file chips carry a language
  dot, test results use the success color).
- What stays in the conversation by default: user messages, assistant
  prose, and one receipt line per turn — nothing else.
- Only the currently running turn renders expanded, as a card
  (decision 2).
- The existing turn footer (model · duration) may merge into the
  receipt line.

### 2. Running-turn card (repositions the base design)

- The in-progress turn's tool calls render inside one card with an
  accent-colored border, one row per tool call; row content is the
  existing per-tool renderer from base slice 1, unchanged.
- Card header: state label + progress `n / m` + elapsed seconds +
  stop button (decision 6).
- When the turn completes, the card folds into its receipt line
  (animation optional).

### 3. Opened receipt = inline expansion (mock 6a)

- Clicking the receipt's `▸` expands the per-call row list in place.
- Each row expands further individually (fs.edit → line diff, bash →
  command + output, fs.read/grep/glob → terse summaries) — all
  reusing the base slice-1 renderers.
- Expansion state is view-local (preserves the base doc's invariant).
- Rejected alternative: popover display (6b).
- Recorded as a future idea, not scheduled: splitting the trace into
  its own pane (6c) — on hold until there is a policy for adding
  views to panes.

### 4. Approval UI = the composer's approval mode (mock 4b — supersedes base decision 8 / slice 4)

- Overrides `docs/agent-output-ui-design.md` decision 8 (inline
  approval inside the requesting tool block, forced expand +
  scroll-in).
- When the session enters `WaitingForApproval`, the composer
  transforms into approval mode:
  - a warning-colored header row: "Allow {operation} on {target}?"
    plus a diffstat;
  - a button row: Allow (⏎) / Deny (esc); starting to type reverts
    the composer to normal instruction input;
  - the pending diff/command renders neutrally inside the running
    card, labeled "proposal — not applied".
- Multiple queued approvals: oldest first with a "+N more" indicator,
  as today.
- No "always allow" button now — per-pattern persistent grants are
  explicitly deferred in `docs/agent-tools-design.md`. Leave one
  button-slot of layout room between Allow and Deny for it.
- Recorded future direction: prompt-intent auto-approval ("auto
  mode"). Implement approval mode as a *swappable composer state* so
  auto mode can later skip or auto-resolve it.

### 5. Failure display (mock 5a)

- A tool call that fails mid-turn stays a single row inside the
  running card (error-colored mark + failure summary + expandable
  log). While the agent keeps going, nothing appears on the
  conversation side.
- If a retry resolves it, the receipt keeps a `retry ×N` chip.
- If the turn ends failed (`Failed` / `Halted`): the receipt folds
  with an error-colored mark, and the assistant prose explaining the
  stop remains. No resident failure card and no composer
  "judgment mode" (5c was an AI-side feature — offering options — and
  does not stand as UI alone; rejected).

### 6. Interruption (mock 7a; future 7b)

- Stop button in the running card's header (`cancel-turn`; suggested
  binding esc esc, adjust to fit the keymap).
- Cancellation remains a stop *reason*, not an error (as implemented
  today): a cancelled turn's receipt folds normally with a
  `stopped · {elapsed}` chip, and partial output/prose is kept.
- Sending from the composer while a turn runs stays next-turn
  delivery; the placeholder states "sends as the next turn"
  explicitly.
- If steering (injecting instructions into the running turn) is built
  later, it takes shape 7b: the interjection renders as a `↪` row
  inside the running card with its uptake state, and the composer
  gains two actions — interject / ⇧⏎ for next turn.

## Out of scope (not decided in this review)

- Session-management entry points (8a/8b/8c were explored, none
  chosen).
- Receipt rendering for very long turns, empty states, notifications.
  (A "dark theme" concern was listed here originally; dropped by owner
  decision 2026-07-13 — Horizon's theme is one config-driven scheme,
  and a light/dark duality is not part of this design at all.)
- Rewind/checkpoints (7c — explored, not adopted; if ever built, the
  recorded direction is reverse-applying fs.edit plus mtime
  verification).

## Suggested implementation order

1. Running-turn card + turn receipts (decisions 1–2) — a display
   change layered on base slices 1/2.
2. Receipt inline expansion (decision 3) — re-arrangement of existing
   renderers.
3. Approval-mode composer (decision 4) — replaces base slice 4; the
   base doc's key-capture design (AgentPaneFocus) still applies.
4. Failure display + stop button (decisions 5–6) — small pieces.

## Contract addendum (2026-07-12)

Stage A of the turn-receipts work (`docs/tasks/backlog.md` item 16,
now resolved): `horizon-agent`'s contract/frame gained the model-only
groundwork decisions 1–2's receipt line and decision 5's failure
display need, with no rendering change. Model-only means exactly
that — the pane view (`src/agent/view.rs`) only got the one match arm
required to keep compiling; the receipt line, running card, and
failure display themselves are still unbuilt (next stage).

- **New frame item.** `AgentFrameItem::TurnEnded { reason, model,
  elapsed }` is pushed when `Event::TurnEnded` folds (previously a
  no-op there). `reason` is the existing `TurnEndReason`. `model` is
  `Option<String>`, folded from the turn's most recent
  `Event::ProviderRequestSent` — `None` for a turn that ends before any
  provider request (e.g. an immediate cancel). `elapsed` is a
  `std::time::Duration`.
- **Elapsed-time trade-off.** No event on the wire carries a
  wall-clock timestamp today (`persistence::event_log::Record::
  created_at_unix_ms` exists, but it's stamped by the `Appender` at
  persistence time, not visible to this crate's pure `Event`-level
  fold). `elapsed` is instead computed by a reducer-side sidecar
  (`frame::TurnClock`, threaded through `apply_agent_event_to_frame`
  the same way `StateEntry` sidecars `AgentFrame`'s own state-elapsed
  tracking): an `Instant` captured when the turn's opening
  `MessageCommitted(User)` folds, read back when `TurnEnded` folds.
  This is exact for a *live* fold (events arrive as they happen) but
  collapses to near-zero for a *cold replay* (`agent_frame_from_events`,
  used for persisted-log bootstrap and `duckdb`'s history queries),
  since a replay folds every historical event in one tight loop.
  Accepted for stage A: a replayed old turn's receipt shows a
  near-zero duration rather than an error or a missing field, and
  never overstates elapsed. A precise persisted duration is a
  follow-up if it turns out to matter, most likely by deriving it from
  `duckdb`'s existing `agent_events.created_at_unix_ms` (mirroring
  `agent_turns`'s own "no derived durations, join through
  `ended_event_id`" choice) rather than adding a timestamp to `Event`
  itself.
- **Explicit tool-call outcome.** `ToolCallResult` gained an
  `is_error: bool` field, lifted out of `output`'s pre-existing
  `"is_error"` JSON convention (every tool in `tools::` already
  follows it — `docs/agent-feedback-design.md` decision 1) so a future
  UI reads a typed field instead of sniffing `output` itself.
  `ToolCallResult::new(call_id, output)` derives it automatically and
  is now the one constructor every call site in this crate goes
  through, so the convention lives in one place.
  `AgentFrameItem::ToolCallFinished`'s shape is unchanged (it already
  wraps the whole `ToolCallResult`), so this needed no frame-level
  change at all.
- **No protocol bump.** Both additions are additive with
  `#[serde(default)]` (`ToolCallResult.is_error` defaults to `false`,
  matching the JSON convention's own "absence means success" reading)
  — the same shape `horizon-session-protocol`'s own
  `Envelope.session_id` used when it was added without moving
  `SESSION_PROTOCOL_VERSION` (currently 4). A version bump there is
  reserved for changes an older peer can't safely decode at all; this
  isn't one. `Event::TurnEnded`'s own wire shape is untouched — model
  and elapsed are derived at fold time, never persisted.
- **Explicitly out of scope.** No retry concept (the runtime has none
  today — deferred by owner decision 2026-07-12, not part of this
  stage), and no rendering change beyond the one `AgentFrameItem`
  match arm `src/agent/view.rs` needed to keep compiling.
