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

## Post-review adjustments (owner feedback 2026-07-13, stage D)

Deviations from the mock/decision 1 wording, made on stage D's branch
after two rounds of visual review (owner explicitly accepted each as a
deviation rather than asking for a mock update):

- **Click affordance (round 1, superseded by round 2 below).** "It's
  hard to tell from its looks that the receipt is clickable." Round 1
  added a subtle hover background and an accent-tinted `▸`/`▾` glyph;
  round 2 feedback ("still hard to notice with hover-only") upgraded
  the resting state itself: the receipt row now always shows a faint
  border (`theme::text_subtle()` at low alpha — the same role/alpha the
  expanded row list's own container border uses) plus rounded corners
  and modest padding, reading as a quiet pill/button row even before
  hover; hovering still strengthens the background on top of that. The
  glyph stays accent-tinted.
- **Receipt aggregation.** "Rows of glob/grep/read chips carry no
  information — aggregate them into something like 'x times tool
  called', and express the number of files read/edited as prose." The
  collapsed receipt line is now prose-first: low-signal query calls
  (`fs.read`/`fs.grep`/`fs.glob`/`recall.*`/`workspace.snapshot`/
  `skill.read`/any other non-edit, non-bash tool) fold into `{N} tool
  call(s)`, with `fs.read` broken out separately as `read {N} file(s)`
  (distinct paths, not call counts) and edit-class calls
  (`fs.edit`/`fs.write`) as `edited {N} file(s)` (same distinct-path
  treatment). Bash calls keep individual chips (the command itself is
  meaningful); **any failed call, of any class, is never aggregated**
  — it keeps its own error-marked chip, so a failure stays visible on
  the collapsed line even though its class would otherwise fold into a
  count. Individual file chips with diffstat leave the collapsed line
  entirely (that detail now lives only in the expansion rows, which
  are unaggregated and otherwise unchanged from stage D). Pure
  aggregation logic (`classify_call`, `aggregate_receipt`,
  `receipt_prose`) lives in `src/agent/turns.rs` with colocated tests.
- **Early fold (round 2): the provisional receipt — superseded by round
  5 below.** "The fold into a receipt happens after the final response
  finishes rendering — can it happen when the final response STARTS
  rendering?" The original mechanism was a pure predicate,
  `turns::running_turn_folds(items)`: true once a running turn's tool
  calls are all finished (at least one exists) *and* assistant text
  appeared after the last tool-related item, rendering a **provisional
  receipt** in place of the running card while true, but flipping back
  to the card if the model made *another* tool call after that trailing
  text (documented on the function itself as intended, not a glitch).
  Kept here as history only — the predicate, the `Provisional` tail
  variant, and their tests are gone; round 5's burst splitting replaces
  the whole mechanism with something that doesn't need a flip-back at
  all.
- **Early fold v2 (round 5): monotone burst splitting.** Owner
  observation, watching the flip-back live: "if we split the card and
  receipt once a response has appeared, it gets a bit simpler and the
  bouncing behavior disappears." Confirmed direction, replacing round
  2's whole-turn predicate entirely. A turn's tool activity is now
  segmented into `turns::Burst`s (`segment_bursts`): a burst is a
  maximal run of tool activity that **closes permanently** once every
  tool call in it has finished and assistant text follows the last one
  (or the turn's own `TurnEnded` arrives, closing the trailing burst
  even with no closing text — tools that ran right up to the end). A
  closed burst never reopens into a card, however much more the turn
  goes on to do; a tool call arriving after the closing text starts a
  brand new burst instead. Rendering walks a turn's items
  chronologically: user message, then per burst — its receipt line,
  then the text that followed it — then the next burst's receipt/card,
  and so on; the turn's *last* burst renders as the running card only
  while it's still open (unfinished tools, or no closing text yet).
  "One receipt per turn" is now **one receipt per burst**, and only the
  turn's actual final burst (the last one, once `TurnEnded` folds)
  carries the end status/total elapsed/model
  (`turns::ReceiptTail::Final`) — every other burst's receipt
  (`ReceiptTail::Intermediate`) shows only the aggregated prose/failed-
  call chips, since the contract has no per-burst timing to show.
  Aggregation itself stays exactly as built (round 1/3/4): each
  receipt aggregates only its own burst's calls, reusing
  `aggregate_receipt`/`receipt_prose` unchanged. Most turns make one
  burst, so the common case (tools run, finish, the model answers, the
  turn ends) looks identical to before, minus the flip-back it no
  longer has to do. Receipt expansion keys off a burst's own start
  index (`base_index + burst.start`), extending the existing
  `TurnSpan::start` convention the same way round 2's turn-level keying
  did. Note for context, not acted on here: the stop button's home
  during final-text streaming (after the last burst has closed but
  before `TurnEnded`) is a stage-F concern, solved composer-adjacent —
  not by keeping a card alive artificially.
- **Approval integrated into the tool-call row (round 3).** From a live
  screenshot of a real session: "can't tell which tool call corresponds
  to which approval" (a running card with ~15 stacked yellow
  `approval requested: …` boxes below it, no visible link back to a
  specific row) and "an already-approved box doesn't need to be shown
  anymore" — "the tool call and its approval buttons could share one
  row, and the fact it was approved can be written as a short phrase in
  that button area." Approval is now integrated directly into the tool
  call's own row instead of rendering as a standalone block at all:
  `ToolCallView` gained `approval: ApprovalState` (`None` / `Waiting` /
  `Approved` / `Denied`), derived in `turns::build_tool_call_views` from
  whether the call ever had an `ApprovalRequested` item and, once
  resolved, whether its result matches `tools::approval::
  denied_output`'s exact convention (`{"is_error": true, "message":
  "denied by user"}` — checked by message text, not just `is_error`,
  since an *approved* call that later fails on its own is also
  `is_error: true`). A `Waiting` running-card row gets small inline
  Approve/Deny buttons (wired to that row's own `call_id`, exactly like
  the old box's buttons) plus a subtle warning tint on the row; a
  resolved row shows a short muted phrase ("approved"/"denied",
  danger-colored for denied) in that same area instead. There is no
  longer any `ApprovalRequested`-as-standalone-box rendering path inside
  the running card. A completed turn's expanded receipt row surfaces the
  same one-word phrase but never buttons (history isn't actionable). The
  oldest-first keyboard/palette approve-tool-call/deny-tool-call
  commands and the control-plane path are unaffected — they still
  dispatch by pending-queue order, independent of which row's buttons a
  pointer happens to click. Stage E's approval-mode composer will layer
  the keyboard path on top of this; the row buttons remain the pointer
  path.
- **Bash folds into prose too (round 3 follow-up).** A second
  screenshot showed ~15 near-identical bash chips wrapping over five
  lines — every command shared the same `cd …` prefix, so the 32-char
  truncated heads were indistinguishable, the same "conveys no
  information" complaint that motivated the query/edit aggregation.
  Successful bash calls now fold into `ran {N} command(s)` exactly like
  query/edit calls (superseding round 2/3's "bash always stays
  individual" rule); a failed bash call still breaks out as its own
  chip like any other failed call. After this change, the only chips
  ever left on a collapsed receipt are failures.
- **Turn grouping fixed to actually partition the item list (root cause
  of a live "incomprehensible screen state" report).** From a real,
  reproduced event sequence (`docs/tasks/backlog.md`-adjacent
  investigation, 2026-07-13): a user typed another message while an
  earlier bash call's approval was still unresolved (next-turn delivery
  is deliberate even mid-flight, decision 6) -- and again, twice more,
  each retry requesting its own new unresolved approval. `turns::
  group_into_turns` used to treat every user message as opening a *new*
  span unconditionally, closing the previous one as a permanently
  dangling `ended: None` stub it could never retroactively fix. Once the
  session's state eventually left the in-flight set (here, a cancel),
  every such stub fell back to the transcript's flat per-item rendering
  path — raw, unprocessed `tool`/`tool result` JSON, `Debug`-formatted
  `tool (preparing)`, and standalone approval boxes with no visible link
  back to their row, stacking indefinitely. Fixed at the root: a user
  message no longer opens a new span while one is already open — it's
  just one more item inside the still-open turn (rendered as its own
  message block via the existing per-item loop already in
  `AgentView::render_turn`), which stays open, however many
  interjections land in it, until an actual `TurnEnded` closes it. The
  invariant is now a doc comment on `group_into_turns` itself, with
  colocated tests reproducing the real sequence. `ToolCallPreparing`'s
  rendering was also humanized (verb + byte count instead of a raw
  `Debug` dump) as a defensive measure, since it's still — in principle
  — reachable through the one remaining legitimate outside-any-span case
  (items preceding a resumed history's first user message).
- **Ghost approvals excluded from the actionable dispatch queue (round
  4: "the current running turn's approval can't be approved or denied —
  it's just stuck").** Investigated from the persisted event log of a
  real, then-live session: a mid-turn interjection (round 3's own
  fix target) can leave an earlier tool call's `ApprovalRequested`
  genuinely unresolved when *its own* turn ends — a "ghost" with no
  live daemon-side gate left to answer a decision for it (the session
  loop that owned that approval moved on to a later turn's own request
  entirely). `AgentFrame::pending_approval_call_ids`/
  `pending_approval_call_ids_in` (oldest-request-first, no turn-boundary
  awareness) would still return a ghost at the front of the queue
  forever, so the palette's approve-tool-call/deny-tool-call commands
  (`pending.first()`) kept dispatching decisions at a call that could
  never resolve — "one approval worked, then everything looked stuck"
  is exactly what silently targeting a ghost looks like from the
  outside. Fixed with a new sibling reading,
  `actionable_pending_approval_call_ids_in` (`crates/horizon-agent/src/
  frame.rs`): identical fold, plus one rule — a `TurnEnded` clears every
  request still outstanding at that point. The *original* function is
  deliberately left untouched and still used for
  `turns::is_approval_still_pending` (the completed-turn transcript's
  own defensive "still shows a dangling approval box" case), which
  needs the unscoped reading precisely because it's asking about a
  request within its own already-ended turn's slice. Every dispatch/
  gating call site — the two palette commands, the command-availability
  gate, and the standalone approval box's button-visibility check — now
  reads the actionable version instead.
  - **What actually emitted the `TurnEnded(Cancelled)` behind the ghost,
    given the owner never clicked a cancel/stop control**:
    `horizon-sessiond`'s own `resume_persisted_sessions` (crash/restart
    recovery, `crates/horizon-sessiond/src/session.rs`) synthesizes
    exactly this — drains every outstanding tool call as cancelled, then
    a `TurnEnded(Cancelled)` — for any session found `is_turn_in_flight()`
    (which includes `WaitingForApproval`) when sessiond starts back up.
    This runs automatically on every sessiond respawn, including
    `Reload Session Runtime` after a rebuild — exactly the iterate/
    rebuild/reload loop this very review cycle was running. No explicit
    cancel needed at all; it's correct, intentional cleanup on its own
    (verified: it does drain the *entire* outstanding set, not just
    part of it) — the bug was purely that the frame's own pending-queue
    reading never learned to treat what it left behind as inert. No
    daemon-side change was needed once that was understood.
  - Tests (`crates/horizon-agent/src/frame.rs`): a ghost ordered before
    a live request is excluded and the live one dispatches first; every
    pending call in an ended turn empties the queue; the scoped and
    unscoped readings agree within one still-open turn (no regression
    to the common case); a request resolved before its own turn ends is
    never mistaken for a ghost.
- **Inline Approve/Deny buttons (round 4).** Reported alongside the
  above: the row-level buttons (round 3) never dispatched a click at
  all, even for a request confirmed live and correctly classified
  `Waiting` — distinct from the ghost-queue issue, since a row's own
  button targets its exact `call_id` directly and never goes through
  the oldest-first queue. Static review (id uniqueness, closure
  capture, nesting) found the button construction sound and structurally
  close to the previously-working standalone box's; physical click-level
  reproduction was attempted (an isolated headless instance driving a
  real bash-approval `Waiting` row) but had to be abandoned before a
  verdict — the test environment's inherited `WAYLAND_DISPLAY` meant a
  headless GUI instance risked opening on the owner's real desktop
  instead of the intended offscreen `Xvfb` display, so it was killed
  immediately rather than risked further (no window was confirmed to
  have reached the real screen; the owner's own session and data were
  unaffected throughout). Applied the most concrete, evidence-aligned
  fix identified without full verification: the row itself (unlike
  `render_expandable_tool_call_row`'s header, which already carries
  `.id(row_id)`) had no explicit element id of its own — only its
  buttons did. Gave it one (`running-row-{call_id}`), matching the
  codebase's own established convention for interactive rows. Flagged
  here as unverified at the click level; if the owner's next rebuild
  still shows the buttons inert, that rules this fix out and the next
  session should pursue physical reproduction via a proper isolated
  X11 `Xvfb` launch with `WAYLAND_DISPLAY` explicitly unset (not just
  `DISPLAY` set), or a `gpui::TestAppContext`-based click-simulation
  test (no precedent for one exists yet in this codebase).
- **Stage E: the approval-mode composer -- superseded by row-centric
  approval v2 below.** Shipped the keyboard path (decision 4) on top of
  round 3's row buttons, which stayed the pointer path exactly as that
  round left them -- neither replaced the other.
  `src/agent/turns.rs` gained `ComposerMode` (`Normal` /
  `Approval { call_id }`, an explicit enum rather than a bool + separate
  call_id so the amendment's own recorded future direction -- auto-mode
  skipping or auto-resolving this state -- has a clean third arm to add
  later) and the pure `next_composer_mode(actionable_queue, dismissed)`
  that decides it, plus `ApprovalHeader`/`approval_header` for the
  banner's operation/target/diffstat text -- all colocated-tested. Kept
  here as history only -- the banner itself, and `ApprovalHeader`/
  `approval_header` with it, are gone; row-centric v2 below retargets
  `ComposerMode`'s same keyboard semantics onto the row instead. The
  no-flap rule, key-capture findings, and mock-deviation notes below are
  still accurate background for how Enter/Esc reach the composer at all
  -- only the "renders as a banner" half is superseded.
  `src/agent/view.rs` wires it: `AgentView` tracks `composer_mode` plus a
  `dismissed_approval: Option<ToolCallId>` marker, both kept in sync by
  `sync_composer_mode` from the session's
  `actionable_pending_approval_call_ids_in` queue (already ghost-excluded
  by round 4) -- called from the session-change observer (covers all
  three non-composer resolution paths: row button, palette, CLI) and
  from the composer's own `InputEvent::Change` handler.
  - **No-flap rule.** Typing dismisses *the exact call_id currently
    shown*, not "approval mode" in general: `dismissed_approval` is set
    to that call_id on the first `Change` event with non-empty composer
    text, and `next_composer_mode` keeps returning `Normal` for as long
    as the queue's head stays that same call_id -- however many more
    keystrokes or re-renders follow, including deleting back to an empty
    composer. It only shows `Approval` again once the head actually
    changes: the dismissed call resolves (any path) and a different one
    becomes the head, or the queue was empty and gains its first entry.
    This was chosen over alternatives like "re-show whenever the
    composer is empty" (would flap on every backspace to empty while
    composing a reply) or "dismiss approval mode globally until the next
    session-level event" (would miss a second, distinct approval that
    arrives while the user is still typing about the first).
  - **Key-capture findings.** `InputState`'s `InputEvent` is
    `{ Change, PressEnter { secondary, shift }, Focus, Blur }` -- no
    `Escape`/`Enter` variant carries a keybinding-specific payload, and
    there is no `AgentPaneFocus`-style context to layer approval-mode
    key capture onto (confirmed absent per this doc's own Current-state
    note). Enter routes cleanly: the existing `PressEnter { shift: false
    }` subscription now checks `composer_mode` first and calls
    `approve`/returns before ever reaching the send-message branch, so
    an empty composer's Enter can never send an empty message while
    approval mode is showing. Esc does not have an `InputEvent` variant
    at all -- checked the vendored gpui-component source at the pinned
    rev (`crates/ui/src/input/state.rs`'s `InputState::escape`): it
    consumes `Escape` only for its own concerns (inline-completion
    dismissal, IME-mark clearing, or `clean_on_escape`, which Horizon's
    composer never opts into) and otherwise calls `cx.propagate()`, so
    the action keeps bubbling up the element tree. `render_composer`
    wraps the `Input` in its own container div with
    `.on_action(cx.listener(Self::on_escape))` to catch it there --
    exactly the pattern gpui-component's own `SearchPanel` uses for the
    same action (`crates/ui/src/input/search.rs`), and exactly the
    fallback this doc's Current-state note anticipated ("put the deny
    binding on the pane/composer container"). No new `KeyBinding`
    registration was needed -- the "escape" keystroke already resolves
    to the `Escape` action within `InputState`'s own "Input" context,
    registered once at gpui-component init.
  - **Mock → role mapping.** The banner is a monochrome amber panel
    (mock 4b keeps its header dot/title/diffstat all in the same amber
    family -- `#f59e0b` border, `#fffbeb`/`#fde68a` fills, `#92400e`/
    `#d97706`/`#b45309` text -- unlike the running card's diff panel
    elsewhere, which does use green/red for +/-): `theme::warning()` for
    the border/dot/title/diffstat text, a new `warning_tint(alpha)`
    helper (mirroring `accent_tint`) for the header fill and the
    button-row divider, `theme::text_subtle()` for the reason line and
    the "typing switches to instructions" hint. Allow reuses `.primary()`
    and Deny `.danger()`, the same `Button` variants the row buttons
    already use for the identical semantic pairing.
  - **Deviations from mock 4b.** (1) The reason line: the mock's header
    has no room for one, but decision 4 doesn't rule it out and the
    keyboard path otherwise drops the `ApprovalRequested` reason on the
    floor entirely, so it's surfaced as a secondary muted line beneath
    the title. (2) The reserved "always allow" slot renders as a bare
    fixed-width spacer, not the mock's muted-but-button-shaped
    placeholder -- decision 4 says "no always-allow button now" and a
    button-shaped placeholder risks reading as a disabled real control
    rather than reserved space. (3) The mock's per-button hint
    (`⏎`/`esc`) is a separately dimmed sub-span; `Button::label` takes
    one string, so the hint is folded into the label text itself
    ("Allow (⏎)" / "Deny (esc)"). (4) "+N more" (decision 4's
    oldest-first queue indicator) has no mock 4b equivalent (its scene
    only has one pending call) -- added to the header's trailing edge,
    same amber text as the diffstat.
  - Not yet reconciled: the stop button's future esc-esc binding
    (decision 6, stage F) will land on an ancestor of this same composer
    container; `on_escape` already propagates a bare Escape when
    `composer_mode` is `Normal`, so a later esc-esc chord handler higher
    in the tree is not blocked by this stage's own handler -- worth
    re-checking once stage F actually adds it.
- **Stage F shipped: failure display + stop button (decisions 5–6).**
  Ran in parallel with stage E's approval-mode composer branch, scoped
  away from the composer's `Input` widget and approval-mode logic (its
  seam: the running card, tool rows, receipts, and the status-line row).
  - **Failure rows (decision 5).** A running-card row is click-expandable
    exactly when `turns::running_row_expandable` (`call.finished &&
    call.is_error`) holds — every other row (still running, or finished
    successfully) stays non-interactive, matching stage D's "receipts
    already cover history" scoping. An expandable row reuses the same
    `turns::tool_call_body`/`render_tool_call_body` machinery
    `render_expandable_tool_call_row` already built for the receipt, so
    the same per-tool bodies (bash's command+output, fs.edit's diff, ...)
    now also work as a running-card failure log — the doc comment that
    scoped that machinery to be reusable this way (stage D) was written
    for exactly this. The `retry ×N` chip stays deferred (no retry
    concept in the runtime, per the current-state note above); a turn
    that ends `Failed`/`Halted` was already covered by
    `receipt_status_covers_every_end_reason`'s existing test, unchanged
    here.
  - **Stop button (decision 6).** `render_stop_button` (a stateless free
    function, `src/agent/view.rs`) renders a small `.outline().danger()`
    gpui-component `Button` — bordered and danger-tinted rather than the
    row-level Deny button's filled danger, matching mock 7a's quiet
    chrome over an alarming one — that dispatches `CommandId::
    CancelAgentTurn` via the existing `RunCommand` gpui action (now
    `pub(crate)`, previously module-private) rather than calling
    `AgentSession::cancel` directly: the same `WorkspaceShell::execute`
    path the palette and `[keybindings]` chords already use, per AGENTS.md's
    "operations go through the command model" convention. It appears in
    two places: the running card's header (the existing spacer already
    reserved this room) and a new stop affordance on the status-line row
    whenever `state_indicates_turn_in_flight` holds. The second spot
    resolves round 5's own noted gap: a burst closes into a receipt as
    soon as its trailing text appears, so during final-text streaming —
    after the last burst closed, before `TurnEnded` — there is no card on
    screen at all; the status line is the one surface still guaranteed
    present whenever a turn is in flight, so it carries its own copy of
    the same button.
  - **Cancelled rendering.** Already covered — `receipt_status_covers_
    every_end_reason`'s `Cancelled` case asserts the `stopped · {elapsed}`
    text stage C built; no gap found.
  - **Keybinding.** `cancel-agent-turn` was already resolvable via
    `keymap::command_for` (no code change needed); `config.example.toml`
    gained a commented-out `"ctrl+." = "cancel-agent-turn"` example. Not
    `esc esc` — stage E claims plain `esc` for approval-mode deny, so the
    decision text's original suggestion is intentionally not followed;
    the button is this stage's primary affordance, per the task scope.
  - **Composer placeholder (decision 6, small).** `turns::
    composer_placeholder(turn_in_flight)` is a pure function returning
    "Message the agent…" or, while a turn is in flight, "Message the
    agent (sends as the next turn)…" (mirroring mock 7a's own wording).
    Wired into `Render::render` via `InputState::set_placeholder` — two
    lines, deliberately minimal to stay out of stage E's way on the same
    widget; a same-file merge conflict here is expected and fine to
    resolve by keeping both sides' changes.
- **Row-centric approval v2 (owner decision 2026-07-13, after reviewing
  stages E+F on main) -- supersedes stage E's composer banner.** With
  approve/deny already living on each `Waiting` row since round 3, the
  composer's own approval-mode banner was reported as no longer earning
  its place -- removed entirely, along with the UI it rendered (the
  warning-tinted header/diffstat, the Allow/Deny button row, the "+N
  more" indicator, the reserved always-allow slot). `ComposerMode` and
  `next_composer_mode` (decision 4, stage E) are unchanged -- the enum's
  own doc comment now says so explicitly: it remains the keyboard-capture
  state (Enter approves / Esc denies the targeted call while the composer
  is empty/not typing; typing reverts to `Normal` via the same no-flap
  rule), just with its rendering surface moved from a composer
  transformation onto the row. `src/agent/view.rs`'s `render_composer` is
  back to just the plain `Input` in its own `on_escape`-catching
  container; `AgentView`'s now-dead `pending_approval_more` field (the
  "+N more" cache) and the banner-only `pending_approval_context`/
  `render_approval_banner`/`warning_tint` helpers are deleted, as is
  `turns::ApprovalHeader`/`approval_header` (the banner's own
  operation/target/diffstat text, unused by anything else).
  - **Oldest-only keyboard annotation.** New pure predicate
    `turns::is_keyboard_approval_target(mode, call_id)`: true only when
    `mode` is `ComposerMode::Approval { call_id }` for that exact
    call_id. `render_tool_call_row` calls it to decide whether a
    `Waiting` row's Approve/Deny buttons get a trailing muted "⏎ approve
    · esc deny" annotation -- derived from the mode itself rather than
    queue position, so it can never point at the wrong row and vanishes
    the instant typing dismisses the mode (the same no-flap rule now
    governs the annotation, not just the old banner). Every other
    `Waiting` row shows plain buttons, unchanged from round 3.
  - **Waiting rows auto-display their proposal.** A `Waiting` row now
    renders its `turns::tool_call_body` (the same fs.edit diff/fs.write
    preview/bash command+output/terse-summary/raw-JSON machinery the
    receipt expansion and stage F's failure log already share)
    underneath itself automatically -- no click needed, since there's
    exactly one thing to look at before deciding -- labeled with a small
    muted "proposal — not applied" tag (`render_waiting_proposal`,
    decision 4's own wording). `waiting` and stage F's `expandable`
    (finished + failed) never coincide on one call, so this and the
    failure-log toggle stay mutually exclusive branches of the same
    wrapper. The body already carried the tool's full data -- notably
    bash's complete `command`, distinct from `ToolCallKind::Bash`'s
    `command_head` the row's own collapsed line and the receipt chip
    truncate to 32 characters -- so no `turns.rs` logic changed to get
    the full command into the proposal; only `render_tool_call_body`'s
    `Command` variant's header changed, from single-line
    `whitespace_nowrap`+`text_ellipsis` to wrapped (`whitespace_normal`),
    so a long or embedded-newline command is fully legible rather than
    ellipsized a second time -- this also improves the pre-existing
    failure-log and receipt-expansion views of a bash call, not just the
    new proposal path.
  - Colocated tests (`src/agent/turns.rs`): `is_keyboard_approval_target`
    true only for the mode's own call_id and false while `Normal`;
    `tool_call_body` on a call with an `ApprovalRequested` but no result
    yet (i.e. still `Waiting`) carries the full, un-truncated bash
    command. `next_composer_mode`'s own no-flap tests are untouched --
    the keyboard semantics didn't change, only their renderer.
- **A second turn-grouping regression: two overlapping approvals,
  approving the former, breaks the layout as attached (owner report
  2026-07-13, with screenshot).** Reconstructed from the real persisted
  log (`~/.local/share/horizon/agent-events.jsonl`, session
  `3fe93cdb-3119-409d-8da7-b4c53c0883bf`, pane "Agent #30",
  `hf:moonshotai/Kimi-K2.7-Code`): the model issued a batch of tool calls
  within one turn -- a workspace snapshot and several `fs.read`s that
  never need approval, interleaved with three `bash` calls that do. The
  last two (`bash:7`/`bash:8`) were requested back-to-back before either
  resolved -- the "two approvals showing" moment. The owner approved the
  former (`bash:7`); the daemon's own `SessionState` then read
  `WaitingForUser` for a real 36-second span (`state_indicates_turn_in_
  flight` is false for `WaitingForUser`) before `bash:8` -- still
  pending the whole time -- finally started. Unlike round 3's regression,
  grouping itself was never at fault here: `group_into_turns` already
  produced one continuous open span across the whole exchange (no
  `TurnEnded` arrives until everything settles), reproduced verbatim in
  `a_batch_of_concurrent_tool_calls_with_two_overlapping_approvals_
  stays_one_open_span`. The actual bug was `AgentView::render`'s
  per-span dispatch: a dangling span (`ended: None`) additionally
  required `state_indicates_turn_in_flight` to hold before rendering
  through `render_turn`, falling back to the same raw flat per-item path
  round 3 targeted whenever it didn't -- exactly what the screenshot
  showed (raw `tool`/`tool result` JSON blocks, a disconnected
  already-resolved approval box next to a still-actionable one, an empty
  status line since `status_line()` also reads `WaitingForUser` as
  empty). Fixed by dropping that gate entirely -- a dangling span always
  renders through `render_turn` now, regardless of the live session
  state, documented as `group_into_turns`'s invariant note. Two more
  changes closed the remaining gaps as defense in depth: (1)
  `group_into_turns` now opens a segment at the first item of *any* type,
  not just a user `Message` (invariant 2), so a structural gap -- e.g. a
  provider continuation after a daemon-synthesized `TurnEnded` on a
  `horizon-sessiond` respawn, round 4's own finding -- can no longer
  leave items permanently outside every span either; and (2)
  `render_item`'s `ToolCallRequested`/`ToolCallFinished` arms no longer
  fall back to raw JSON at all -- `AgentView::render_orphan_tool_row`
  correlates the item back to its call across whatever item slice is in
  scope and renders it with the same glyph + verb/target/summary
  vocabulary (and, for a still-actionable approval, the same integrated
  Approve/Deny row) as a running-card row, de-duplicating so a call whose
  several items all land here doesn't mint several rows. Between (1) and
  the dispatch fix, this fallback should be structurally unreachable for
  any legitimate sequence now; it stays only as a last-resort renderer
  for a genuinely unknown future item shape.
