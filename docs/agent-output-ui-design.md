# Agent Output UI — Transcript Redesign

Status: owner-approved 2026-07-06; implementation in slices (this doc
records the whole design; slice status is tracked in the roadmap).
Amended 2026-07-12 by `docs/agent-output-ui-amendment.md` (turn
receipts + running-turn card layered on top; decision 8 / slice 4's
inline approval superseded by a composer approval mode).

## Problem

Two pains from the owner's real usage (see docs/research/agent-ui.md,
second installment): (a) every agent response fragment becomes its own
block, so vertical information density is low; (b) edit tools render
their raw output, so *what the agent is trying to do* is illegible.

## Policy

Imitate good precedent: adopt the conventions that multiple products
converged on independently (the research doc's 定石), and spend owner
decisions only where products genuinely diverge. Being a Floem-native
GUI, prefer expressiveness beyond TUI constraints — but richness means
where decoration goes, not how much (transcript keeps minimal chrome;
decoration is reserved for modals and badges).

## Decisions

1. **One tool block per call.** The Requested/Preparing/Started/
   Finished lifecycle items merge into a single transcript block keyed
   by call id, updated in place as events arrive.
2. **Tool block = one summary line by default.** Header:
   status glyph + verb + target (+ result summary once finished),
   e.g. `Edit src/agent/view/mod.rs · 2 hunks`. Click expands the
   body; collapsed is the default for every tool state including
   errors (errors are color-marked in the header).
3. **Per-tool renderers, no raw-output path for file tools.** fs.edit
   renders a line diff (reconstructed by joining the finished result
   to its originating request's `old_string`/`new_string`), fs.write
   renders a highlighted content preview, bash renders command +
   captured output as preformatted text, fs.read/glob/grep/
   workspace.snapshot render terse result summaries. Raw JSON is the
   fallback for unknown tools only.
4. **Diff rendering** uses dedicated theme roles (added/removed
   surfaces); line background carries the change, the sign column is
   colored separately. New files render as highlighted content, not
   as an all-added diff.
5. **Thinking auto-expands only while streaming** and collapses to a
   one-line header when done (manual toggles win). [slice 2]
6. **Density rules**: whitespace and horizontal rules belong to turn
   boundaries only (turn footer: model · duration), not between tool
   calls; user messages stay boxed (asymmetric to assistant prose);
   backlog 7's user-bubble/approval colors return as theme roles.
   [slice 2]
7. **Follow-scroll becomes an explicit state machine**: sticky bottom,
   deliberate detach on scroll-up, a return pill, and a jump to the
   latest user message. [slice 3]
8. **Approval moves inline** into the tool block that requested it:
   forced expand + forced scroll-in, preview height-capped so buttons
   stay visible, key capture relocated from the banner
   (AgentPaneFocus). [slice 4]
9. **A Changes overview** (edited files + diffstat for the session)
   joins the pane as a collapsible aggregation. A Todo tool does not
   exist yet; a plan/todo panel is deferred until the agent grows one
   (roadmap item, agent-foundation). [slice 5]

## Invariants

- The trailing-window (200 blocks) + revision memoization of
  `src/agent/view/transcript.rs` must be preserved: expansion state is
  view-local and never part of the window computation; merging
  lifecycle items must not make the revision proxy miss updates.
- All colors go through theme roles (no hardcoded colors).
- Operations stay on the command model; approval commands unchanged.

## Known limitation: window-trim scroll drift (slice 3)

While `follow` is `Detached` and a very long session (> 200 blocks past
the last one visible) trims the oldest block from the window, the
surviving blocks shift up by the trimmed block's height, but the
scroll view's pixel offset does not compensate -- the same offset now
shows different content. `floem::views::Scroll` exposes an item-based
jump (`scroll_to_view(ViewId)`, used by slice 3's "jump to latest user
message" pill and its own `latest_user_block_id` resolution), so an
anchor-and-reassert fix is possible in principle: track the topmost
visible block while detached, and re-`scroll_to_view` it whenever a
trim (`TranscriptWindow.omitted` increasing) is observed. Deferred for
now -- it needs (a) a topmost-visible-block resolver walking every
mounted block's layout rect, and (b) a trim-only trigger distinct from
the ordinary revision bump that already drives the sticky-bottom snap,
which is more machinery than this slice's scope. Only affects sessions
long enough to exceed the 200-block window while the user is reading
older content; not a regression from slices 1/2, which already
established the same trailing-window trade-off.

## Slices

1. Tool-block merge + one-line summaries + per-tool renderers (edit
   diff, write preview, bash output) + diff theme roles. [this slice]
2. Density, turn boundaries, thinking auto-expand, backlog 7 roles.
3. Follow-scroll state machine.
4. Inline approval.
5. Changes overview.
