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
- Receipt rendering for very long turns, empty states, dark-theme
  tuning, notifications.
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
