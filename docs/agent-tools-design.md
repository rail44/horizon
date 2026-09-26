# Agent Tool Baseline Design

Decision record for making a single agent session usable for daily development
(the prerequisite for every multi-agent scenario in the product direction).
Based on a 2026-07 survey of shipping agents (Claude Code, Codex CLI, Gemini
CLI, aider, OpenHands, Cline, goose) and primary design guidance (Anthropic
engineering posts, the SWE-agent ACI paper, OpenAI's agent guides). Where the
industry diverges, the choice and rationale are noted explicitly.

## Goals

- File tools, command execution, turn interruption, a thin system prompt, and
  minimal provider config — enough for one agent to do real work in Horizon.

Non-goals for this baseline (deferred; see the last section): context
compaction, MCP, persistent shell sessions,
plugin-provided tools, running agent commands inside terminal sessions.

## Tool Set

| Tool  | Permission       | Notes                                              |
|-------|------------------|----------------------------------------------------|
| read  | auto-allow read  | Line-windowed (offset/limit) with a default cap    |
| glob  | auto-allow read  | Dedicated tool, not shell                          |
| grep  | auto-allow read  | Dedicated tool, not shell                          |
| write | require approval | Creates parents; overwrite requires prior read     |
| edit  | require approval | Exact string replacement (below)                   |
| patch | require approval | Validated multi-hunk/multi-file change set (below) |
| bash  | require approval | Fresh process per command (below)                  |
| web_search | boundary: auto | Fixed Exa endpoint; shadow-judged             |
| web_fetch | boundary: exact-host grant | Bounded SSRF-safe fetch           |

All tools require **absolute paths**; relative paths are rejected with an
actionable error (models measurably mishandle relative paths — SWE-bench-era
finding restated in Anthropic's agent guidance).

## Read and Search Semantics

### Input contract (2026-09-26)

The 18 non-board built-ins use the Rust definitions in
`crates/horizon-agent/src/tools/input/`. Serde checks argument types and
unknown fields; Schemars generates the model-visible schemas from those same
definitions. Numeric bounds, text lengths, and defaults belong to the input
types, rather than separate handler and catalog constants. Invalid values
(including numeric strings, string booleans, out-of-range limits, unknown
keys, and positional arrays in place of objects) produce an identified tool
error before approval, execution start, grants, or side effects. Errors name
the offending field or list element.

Execution prepares a typed call before policy selection. Synchronous handlers
and background jobs consume those arguments. Approval resolution reconstructs
the typed call from the exact recorded request, so restored approvals and
retries receive the same validation without persisting a second input format.
The original JSON remains the audit record. Filesystem staleness, path access,
domain grants, redirects, and other mutable conditions are still checked at
use. Cross-field rules (for example, differing old/new edit text and a valid
memory log range) remain semantic validation; schema generation does not
replace them. Board and host-provided tool contracts are separate.

Limits are now rejected instead of silently clamped. For example,
`fs.read.limit=5000` must be corrected to at most 2000; `limit="10"` is a
type error rather than the default window. Removed fields such as
`fs.grep.context` are unknown-field errors.

### Result contract (2026-09-26)

The same 18 non-board tools construct bodies from `contract::tool_output` and
return `tools::output::Response`. Success/failure is chosen explicitly; approval
and containment evidence is added before the result is serialized. `is_error`
inside failed output is a derived provider-visible marker, never the authority
for a built-in outcome. External board/host adapters retain their own JSON
convention. The provider still receives `{outcome, output}` from the recorded
`ToolCallResult`, both live and on resume.

`fs.edit` records each entry as `Applied { occurrences }`, `Failed { message }`,
or `NotAttempted`. Completed views count only recorded applied edits, including
those before a later failure. Expanded rows label failed/unattempted entries and
show only applied replacement diffs. Line totals remain reconstructed replacement
statistics weighted by occurrences, not a net file/session diff (multiple matches
on one line can contribute more than once). Pending approval previews remain
proposals. No rollback is introduced.

Bash records an explicit termination kind: ordinary exit, timeout, termination,
execution failure, or reuse. An ordinary nonzero command exit remains a completed
tool execution with that exit code; containment denial can still fail an exit-zero
pipeline. Failed captures cannot be reused, and reuse binds the original request
by both call and occurrence. Child reports are typed before registration and used
by both push notifications and `task_output`, retaining useful partial reports.

Display decoders share the producer types. Bodies without the required structure
remain available as raw output instead of being represented as completed changes.
The JSONL and wire envelopes are unchanged (event-log v3, agent wire v25); the
additional bash termination field requires no log migration. Existing bash payloads
without that field use the raw display fallback and are not reused as fresh results.

### Results

`fs.read` is for a known file or a relevant line window, not content
discovery. Its catalog routes specific-content questions to `fs.grep`,
unknown paths to `fs.glob`, and independent known files to parallel reads.
The default window is 500 lines; an explicit request may reach 2,000 lines.
The rendered content also stops at 50,000 characters on a line boundary and
returns `next_offset`, so a continuation is explicit rather than an
accidental whole-file response. Each result carries a version derived from
the file mtime and size.

`fs.grep` accepts either one file or a directory and returns **locations
only** — one `path` + `line_number` per match plus the total count, never
the matching line or surrounding context (`d74a75e`, 2026-07-25; the
measurement behind the reversal is in
`research/agent-read-navigation-prior-art-2026-07-25.md` §1 and the
follow-up session analysis: context lines cost ~5x the locations and only
16% of them were ever revisited by later reads). The complete serialized
result is capped at 50,000 characters. These are harness invariants rather
than prompt-only conventions; content questions route to `fs.read` with a
window around the reported line.

A provider request carries the session's canonical history verbatim. Horizon
maintains no separate, lossily-projected view of it: nothing elides duplicate
reads, shrinks old tool-result bodies, or drops turns on the way out. That
mechanism existed between 2026-07-20 and 2026-07-25 and was removed with the
owner decision recorded in
`research/agent-context-memory-separation-2026-07-20.md` — a lossy history
transformation is a tradeoff that should not be taken while the amount of
context ordinary work produces in the first place is itself unresolved. The
bounds above are the current answer to output size, and they act where the
content is produced rather than after the fact.

The measurement and prior-art basis for the read/grep bounds is recorded in
`research/agent-tool-output-and-read-routing-2026-07-24.md`.

`fs.read`/`fs.grep`/`fs.glob` reach two places without approval: the
session's workspace root, and the session's own Git metadata — the gitdir a
linked worktree's `.git` pointer file names, plus the repository's common
dir, resolved by the same `tools::metadata_writable_roots` the Git-operation
approval uses. Without the second, a session in a linked worktree could not
read its own `.git/HEAD` while a session in an ordinary checkout reads it as
an in-root file. Reads only: `fs.write`/`fs.edit` stay confined to the
workspace root. Anything else goes through the approval gate.

## Edit Semantics

The industry has converged on exact-string replacement with uniqueness
enforcement (Claude Code, Gemini CLI, OpenHands, goose, Cline):

- **`fs.edit` takes a list of edits and nothing else**:
  `{"edits": [{path, old_string, new_string, replace_all?}, …]}`, at least
  one entry, no single-edit top-level shape (owner decision 2026-07-28).
  Batching is therefore the ordinary path rather than a capability the
  model has to discover — the measurement behind that decision is
  `research/agent-editing-phase-analysis-2026-07-28.md`: 82/82 edits in
  the analyzed session ran as single-edit rounds, and the model built a
  35-round Python bulk-edit workaround rather than reach for either
  batching tool. The shape matches kimi-cli's native `edit: Edit |
  list[Edit]` dialect minus the scalar branch.
- **Sequential, stop at the first failure, no rollback.** Edits apply in
  list order; edits to the same file compose (a later edit reads what an
  earlier one wrote, and a path this call already wrote is exempt from the
  staleness gate for the rest of the call). The first failing edit ends
  the call: already-applied edits stay on disk — rolling other files back
  would fabricate state — and the result reports every edit's outcome in
  order (`applied` with its `occurrences`, `failed` with its reason,
  `not_attempted`) plus `failed_index`, so the model can fix that one edit
  and resend from there. A malformed `edits` list is rejected before any
  file is touched, so a shape error never leaves a partial application.
- `old_string` must match **exactly**. Zero matches and multiple matches are
  `is_error` results with actionable text. By default, `old_string` must also
  match **uniquely** ("found 3 matches — include more surrounding context");
  set `replace_all: true` to replace every occurrence instead. A successful
  result always includes `occurrences`, the number of replacements performed
  (1 in default mode).
- **Staleness gate, enforced mechanically:** a file must have been read in
  this session, and its mtime must be unchanged since that read, or the edit
  is rejected ("file changed on disk — read it again"). Read-before-edit is a
  harness invariant, not prompt discipline. Note that `fs.edit` itself
  records the mtime it leaves behind, so a subsequent edit is allowed without
  an intervening `fs.read`; this is intentional (it is what lets edits chain
  without a read round trip per edit), but it means a resubmitted identical
  edit passes the gate — and when `new_string` contains `old_string`
  (insertion-shaped edits), the resubmission matches again and silently
  applies twice. Root-caused 2026-07-26 (backlog 48); a session-local
  repeat-detection guard was implemented, then removed by owner decision the
  same day — no surveyed implementation carries one, three occurrences in
  nineteen days did not justify novel surface area, and the quality gate
  catches the consequence downstream. The knowledge stays; the mechanism
  does not.
- No fuzzy-match fallback in v1. Gemini ships a four-tier fuzzy cascade;
  Claude Code deliberately ships none. Start strict, collect failure data,
  add leniency only if the data demands it.
- A process-wide per-path lock serializes `fs.write` and `fs.edit`
  mutations that target the same file, including calls from different
  sessions. An `fs.edit` call holds every path in its list for the whole
  call, acquired in lexical order to avoid deadlocks.

### `fs.patch`, removed 2026-07-28

`fs.patch` (a V4A `*** Begin Patch` marker format, shipped 2026-07-22 as
the multi-file batching path) was deleted rather than repaired: 5 failures
in 6 lifetime uses, because its `@@` headers are literal source-line
anchors while the schema never said so and models write the diff-style
`@@ -1,6 +1,8 @@` they were trained on. Batching moved to the tool the
model already trusts. Rationale and numbers:
`research/agent-editing-phase-analysis-2026-07-28.md`.

## Parallel Tool Calls

Horizon explicitly enables OpenAI-compatible `parallel_tool_calls`. A model
may issue several independent calls in one assistant response; the provider
session retains the whole batch and runs exactly one follow-up completion
after every result has arrived. Tool semantics still decide execution
ordering: independent asynchronous web calls may overlap, filesystem writes
to the same path serialize through the path lock, and bash preserves its
per-session FIFO. Parallelism is a request/execution property, not a generic
user-visible `batch` tool.

## Bash Semantics

- Fresh process per command; the harness tracks the working directory across
  calls (`cd` persists via tracking, not via a live shell).
- Wall-clock timeout, default 300s, per-call override up to a hard 1800s
  maximum. The catalog tells the model to omit the override normally and to
  use a shorter value only for an intentional quick probe; builds, tests,
  hooks, and Git commands commonly outlive the old 60/120-second budgets.
- Output capped in-context (~30k chars, head+tail preserved); the full output
  spills to a temp file under `$TMPDIR` (`/tmp` on Linux), outside the
  workspace root, and the path is returned in the result. `fs.read`/`fs.grep`
  are workspace-confined and reject that path as "escapes the workspace
  root", so a selective re-read of the spill must go through `bash`
  (`cat`/`grep`) — and under the sandbox even `bash` cannot reach the host's
  `/tmp` (issue 010). (Truncate-in-context + spill-to-file is the shipping
  standard across Claude Code, goose, Cline, Codex.)
- A bash registration belongs to one `(session_id, call_id)` from enqueue
  through execution. Cancellation skips a queued job or kills its active
  process; a process attached after cancellation is killed immediately.
  Reusing an ID creates a distinct registration, so retiring an old job cannot
  unregister its replacement. Session teardown cancels all its registrations.
- Timeout and turn-cancellation kill the in-flight command's process group
  (`libc::kill(-pgid, SIGKILL)`). On Linux, a best-effort `/proc` walk also
  reaches descendants that escaped the group through `setsid`/`setpgid`.
  Registration remains active through the bounded output drain; process
  retirement and cancellation serialize access to the kill handle.

## Async Completion Identity

Async completion identity (2026-09-24): bash and web capture the request's
`OccurrenceId` before queueing and retain it on normal, denial, redirect, and
panic outcomes. The daemon checks that identity before folding or forwarding
any async completion, including approval judgments. An old attempt cannot
answer a newer request with the same provider call ID. Untagged legacy results
keep their existing fallback; synchronous denial resolution still returns the
original attempt's `prior_result`. This changes no persisted or wire format.
The provider's pending-call map owns outstanding work; a second cancelled-ID
set is unnecessary and would suppress valid results when an ID is reused.

## Bash Containment

Hardening added after a 2026-07 incident: a tool-approval banner that didn't
visibly react to a held `y` key let a user re-send `Approve` for the same
still-running `bash` call 134 times in 29 seconds, spawning 134 concurrent
`cargo test --workspace` runs and OOMing the machine. The approval
idempotence fix (a call transitions pending -> resolved exactly once — see
`agent::tools::approval`'s guard and `AgentFrame::has_tool_call_started`)
closes the hole that let duplicates through in the first place; the two
measures below are defense in depth against a session's bash calls
otherwise piling up:

- **Per-session serialization.** A session's approved `bash` calls run one
  at a time: while one is executing, a later approved call for the *same*
  session queues (simple FIFO) rather than spawning concurrently. A
  persistent per-session worker thread was considered and rejected — bash
  is already a "fresh thread per call" design (simplicity, no
  long-lived-thread lifecycle to manage across session creation/teardown),
  so the FIFO is layered on top of that as a pure ordering constraint
  instead (`tools::bash::registry`'s session queue table).
- **Low priority.** Every bash child is niced (`libc::setpriority`,
  `PRIO_PROCESS`, level 10 — felt, but not maximal, since it's work the
  agent is actively waiting on) from *inside* the forked child via
  `pre_exec`, before it execs — not via a post-spawn `setpriority` call
  from the parent, which would race a fast-forking command that spawns
  grandchildren before the parent gets scheduled to make the call.
  `pre_exec` guarantees the niceness is in place before bash (and every
  descendant it later forks, since nice is inherited across fork/exec)
  starts running, regardless of process-group shape.
- **Shared Cargo-cache guard.** In an isolated workspace whose tracked
  Cargo config places `build.build-dir` under `{cargo-cache-home}`, the
  sandboxed path refuses a directly recognizable `cargo clean` unless it
  carries `-p`/`--package`. A filesystem grant that lets Cargo build in the
  shared directory necessarily also lets Cargo remove files there; one
  unscoped clean otherwise makes every worktree rebuild the full dependency
  graph. Package-scoped stale-crate recovery remains available, as does an
  explicitly worktree-local `CARGO_BUILD_BUILD_DIR` for a truly isolated
  full rebuild. This is a proactive resource/UX guard, not a shell security
  parser or part of the containment boundary.

These measures do not cap memory directly (niceness affects CPU scheduling, not
memory), so they don't replace the idempotence fix — they reduce the blast
radius of any future bug that lets a session accumulate more than one
in-flight bash call.

## Web Tools

`web_search` and `web_fetch` are asynchronous Horizon-owned tools. Search
uses a fixed HTTPS Exa adapter, an environment-only `EXA_API_KEY`, bounded
inputs/responses, and a vendor-neutral result shape. It is auto-approved as
the narrow fixed-vendor boundary crossing while the shadow judge records
every call.

Fetch asks before contacting an unapproved exact host. Approval adds only
that normalized host to the session's shared domain policy; it does not
cover subdomains. Redirects are followed manually, and an unseen redirect
host stops before contact for another `DomainGrant`. The request path uses
a pinned safe resolver, rejects local/private/metadata and IPv6 transition
targets, permits only standard HTTP(S) ports, disables ambient proxies, and
caps redirects, total time, body bytes, DOM work, metadata, and returned
content. HTML is reduced to Markdown with `dom_smoothie`; supported text is
passed through and binary or encoded bodies are rejected.

## Error Model and Loop Guards

- Every tool failure returns an `is_error` tool result; the loop never
  crashes on tool errors. Error text says what went wrong and what to try.
- The system prompt carries a one-line retry nudge (models otherwise tend to
  give up after a single tool failure — documented by OpenAI).
- **Turn-loop guards are a built-in safety net, not a work limiter**
  (revised 2026-07-18, `docs/issues/002-agent-iteration-cap-halts-real-
  work.md`'s resolution — the original version of this section described a
  25-turn cap tuned so tightly it fired on ordinary agentic work). Two
  independent guards, both fixed built-in constants in `crates/horizon-
  agent` (`config::DEFAULT_ITERATION_CAP`/`DEFAULT_DOOM_LOOP_WINDOW`), no
  longer configurable via the file at all — the entire `[agent]` section
  (including `iteration_cap`/`doom_loop_window`) was removed from Horizon's
  config schema in the 2026-07-18 config-narrowing wave (see AGENTS.md's
  "Configuration" section):
  - **Iteration cap (100).** Halts after 100 consecutive tool-driven turns
    since the last user message — `providers::rig::session::TurnLoopGuard::
    record_tool_turn`, incremented once per landed tool *batch*, not once
    per call within it.
  - **Doom-loop detection (window 5).** Halts once the last 5 consecutive
    tool results fingerprint identically as (tool, args, output) —
    `TurnLoopGuard::record_fingerprint`.
  - **A halt reads as a pause, not an error.** Neither guard emits
    `Event::Error` anymore; both emit `Event::TurnEnded` with a specific
    reason (`TurnEndReason::HaltedByIterationCap`/`HaltedByDoomLoop` —
    `contract::TurnEndReason`'s doc comment covers the legacy bare
    `Halted` variant kept only for pre-resolution persisted logs). The
    transcript renders the turn's receipt calmly (`src/agent/turns/
    receipt.rs`'s `receipt_status`: `is_error: false`, text naming the
    guard and its threshold, e.g. "paused after 100 consecutive
    tool-driven turns"), and the session returns to `WaitingForUser` —
    reads as waiting-for-user, not failed.
  - **Continuing is one action.** `CommandId::ContinueAgentTurn`
    (parameterless, mirrors `CancelAgentTurn`'s shape) resumes the halted
    turn without composing a new message: a button sits directly on the
    paused receipt row, and the command is reachable from the palette, a
    control-plane invoke (`horizon continue-turn <session-id>`), and
    `WorkspaceShell::execute`. Wire shape: `Command::ContinueTurn`
    (parameterless), handled by `providers::rig::session::run_session_loop`.
    A guard halt stashes the real, already-executed tool result that
    tripped it in an in-memory `pending_halt_result` slot rather than
    folding it into `rig_history` immediately (mirroring how an ordinary
    tool-driven turn treats a batch's last-landed result as the *next*
    turn's prompt, not a pre-pushed history entry). `Command::ContinueTurn`
    consumes that slot, resets the guard, and resumes exactly as if the
    guard had never tripped; a plain `Command::UserMessage` sent instead
    flushes the same slot into history first, so typing past a halt still
    works. **Replay safety**: `pending_halt_result` is purely in-memory
    session-loop state, never persisted and never reconstructed from
    `rig_history` — every freshly spawned session loop starts with it
    `None` regardless of what history it loaded, so a session that ended
    halted and is later resumed/replayed sits at `WaitingForUser` without
    auto-continuing; a stray `Command::ContinueTurn` reaching a
    fresh/idle session is a safe no-op.
  - The guard itself is unchanged in kind: 100 consecutive tool turns (or 5
    identical results) with zero user interaction still stops the loop —
    only the threshold, presentation, and resumability changed.

## Turn Loop and Cancellation

The current per-session loop blocks the whole OS thread inside
`block_on(turn)`, so `Command::Cancel` is structurally unreadable mid-turn.
This changes:

- The session loop becomes concurrent: commands are received while a turn is
  in flight (async loop with `select!`, or turn spawned as a task; the
  command channel becomes async-capable).
- A `tokio_util::sync::CancellationToken` scopes each turn; the streaming
  loop and tool execution `select!` against it; bash children are killed and
  host-side web requests are cancelled on turn/session teardown.
- **Cancellation is a stop reason, not an error** (borrowed from the Agent
  Client Protocol): text already streamed is kept and the turn is committed
  as cancelled; pending approval requests belonging to the cancelled turn are
  marked cancelled; a `ToolCallResult` arriving after cancel is accepted and
  dropped.
- Cargo: add `tokio-util`; enable tokio `macros`, `process`, `time` features.

## System Prompt

Thin, per current guidance (over-prescription measurably harms newer models):
identity, an environment block (cwd, OS, git repo or not), a few lines of
tool policy, the retry nudge, and an explicit caution list for destructive
actions. No step-by-step workflows.

**Addendum (2026-07-07).** The prompting survey
(`docs/research/agent-prompting.md` Part 1.4) found short communication
and verification norms near-universal even among deliberately thin
prompts, and Horizon had none; the prompt now carries them (be concise;
report outcomes faithfully; verify before declaring done) plus one line
naming session persistence, which the recall tool made true. Owner
constraint recorded at the same time: norms must stay model-agnostic --
Horizon expects to switch providers, so provider-specific prompt lore
(e.g. Kimi-tuned phrasing, or removing the tool-policy lines on Kimi's
official advice) is out of scope regardless of its evidence.

**Addendum (2026-07-27).** "No step-by-step workflows" above is narrowed
to *no unmeasured* ones (owner decision). The delegation-first routing
block — `prompt::DELEGATION_ROUTING_SECTION`, appended only to sessions
that advertise the `task` tool — is a workflow prescription, and it ships
because it was measured effective against both production models
(`docs/research/agent-delegation-and-batching-probes-2026-07-27.md`, cells
C5 and C7b; `docs/agent-explore-design.md`'s addendum of the same date is
the scope record). The model-agnostic constraint above is untouched: the
shipped wording is the one that worked for both models, with no per-model
branching. A prescription without that kind of evidence stays out.

## Config

Provider/model selection and base URL flow through Horizon's single TOML
config file plus environment variables (env wins) — see `AGENTS.md`'s
"Configuration" section and `config.example.toml` for the full precedence.
Provider and Exa API keys stay environment-only. No configuration UI. The bash/fs tool
tuning and turn-loop guard values on this page are *not* config-file knobs:
as of the 2026-07-18 config-narrowing wave (the "Error Model and Loop
Guards" revision above, extended to the rest of the former `[agent]`
section), every one of them is a fixed built-in constant in
`crates/horizon-agent/src/config.rs`.

## Where the Industry Diverges — Our Choices

1. **Dedicated search tools vs shell-only.** Codex CLI and goose ship no
   read/grep tools and route through `rg`/`cat`. We ship dedicated
   `glob`/`grep`: under per-command bash approval, shell-routed searches
   would hit the approval gate constantly. Revisit if OS sandboxing lands.
2. **Per-command spawn vs persistent PTY.** Split across the industry. We
   spawn per command for simplicity; a persistent-shell story may later merge
   with the "agent exec as a terminal session" idea below.
3. **Strict vs fuzzy edit matching.** Strict (Claude Code's side of the
   split), for predictability and simpler failure analysis.

## Deferred, With Reasons

- **Compaction / context editing** — a long-horizon concern; not needed to
  make one agent useful.
- **MCP** — the industry's extension slot has converged on MCP, but
  Horizon's plugin system is our intended seat for tool providers. The
  relationship (bridge? contract compatibility?) is a future design topic —
  record, don't build.
- **OS sandboxing + pattern-scoped persistent permissions** — naive
  per-action approval collapses in practice (Anthropic measured ~93%
  approval rates before sandboxing); the durable fix is an OS sandbox, with
  per-pattern persistent grants ("always allow `npm test`") as the interim
  step. Both are out of scope for v1 and recorded here so the approval UX is
  designed with them in mind.
- **Agent exec as a terminal session** — running agent commands inside a
  visible Horizon terminal session instead of a hidden subprocess. A
  Horizon-native evolution to explore after the standard kit works.

## Key Sources

- Anthropic: Writing Effective Tools for Agents; Effective Context
  Engineering for AI Agents; How We Contain Claude; Claude Code docs
  (tools reference, sandboxing).
- SWE-agent: Yang et al., arXiv:2405.15793 (agent-computer interfaces).
- Codex CLI and OpenCode source (apply_patch, per-file edit locks, parallel
  call batching); Gemini CLI source
  (edit.ts match cascade); goose source (developer extension).
- Agent Client Protocol (agentclientprotocol.com) — cancellation and
  permission-request semantics.

### Completed containment attempts own their identity

`ToolCompletion` carries a completed attempt's call and occurrence IDs only in
its `ToolCallResult`. The daemon uses that same identity for acceptance and
for locating the request to reissue; denial variants do not repeat the call ID.
A redirect grant has no completed result and therefore carries its own origin.
This is an internal worker/daemon boundary; persisted and wire types are unchanged.

### Configure tool capabilities before sharing runtime state

`ToolSessionBuilder` owns initial non-board configuration and is not cloneable.
Its consuming `build` creates the shared `ToolSessionState`; configuration setters
are unavailable on that state, so cloning can no longer silently discard a later
configuration change. Live approvals still update the shared grant stores. Root
canonicalization, missing-root refusal, grant revalidation, and proxy/judge fallback
retain their existing behavior. The daemon finishes construction before registering
or publishing the runtime. Board-host installation retains its separate existing
entry point and is performed immediately after `build`, before sharing.

### Ephemeral feedback at shared boundaries

`ProviderEvent::is_ephemeral` identifies progress and session metadata before
tool dispatch or persistence. `Appender` owns the exclusion for both streaming
and acknowledged writes, so callers do not need to pre-filter a batch. `LiveState`
folds tool progress and model metadata, and ignores child-task progress owned by
the view. The wire conversion keeps each feedback kind in its dedicated variant;
the unused conversation-event placeholder is never persisted or forwarded. The
existing event envelope, JSONL format, and wire variants are unchanged.

## Execution planning and exact approval (2026-09-25)

`policy::plan_tool_call` returns one `ToolPlan`: automatic execution mode,
`ApprovalRequest`, or rejection output. The coordinator uses that same decision
to execute or pass an `ApprovalCandidate` to the judge/human gate. There is no
second policy classification to keep synchronized with the displayed prompt.
Trust tiers and the scope of filesystem, network, Git and host grants are unchanged.

`ToolUpdate` owns the acknowledged lifecycle for automatic and approved tools.
The request and start must be saved before a synchronous effect, worker enqueue,
or session grant expansion. Tool handlers return output plus any domain records;
those records and the result are saved before publication and the next provider
round. The daemon publishes these applied events once. Commands queued by a
synchronous tool still precede its provider result. Storage failures stop execution
through the [persistence contract](agent-persistence-contract.md).

Human approve/deny commands carry `ToolCallIdentity` (`call_id`, `occurrence_id`)
from the displayed row or CLI argument through the control plane and agent wire.
Only the matching, pending `ApprovalRequested` in the open turn is actionable.
A missing prompt, superseded occurrence, saved decision, start, or finish rejects
the command without forwarding or expanding a grant. `ApprovalResolved` now
participates in the frame fold so duplicate decisions remain rejected after replay.
Keyboard targeting and dismissal also distinguish occurrences.

Judge completions retain the original request and approval; both identities must
match, and an intervening human prompt or decision takes precedence. Judge audit
payloads include the occurrence id. Filesystem and Git grants are still revalidated
at approval and by the sandbox before process spawn.

Agent wire v25 requires the exact identity. CLI syntax is `horizon approve
<session-id> <call-id> <occurrence-id>` (and `deny`, optionally with `--reason`).
Agent event-log v3, terminal wire and log wire are unchanged. Activating v25 requires
a rebuilt shell and agent daemon plus a full app restart; runtime reload alone is
insufficient. Integration neither migrates live data nor restarts the running app.

## Background work lifetime (2026-09-26)

Bash, Web, approval judgments and child watchers share registration and actual
retirement accounting. Cancellation identifies the exact occurrence, and child
lifetimes distinguish a single MoA pass from the parent session. Session teardown
waits for retirement before deleting a worktree; see
[background work ownership](agent-background-work.md) for the stop/finish contract
and the requirements for adding a worker.
