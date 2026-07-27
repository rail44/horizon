# Parallel Exploration Sessions — `task` (originally `agent.explore`)

Decided 2026-07-25 in an owner consultation, on the measurements in
`docs/research/agent-read-navigation-prior-art-2026-07-25.md` and the two
failed dogfooding runs described below. This document is the scope record
for the implementing worker.

The model-visible tool id is **`task`** as of the 2026-07-27 addendum at the
bottom of this file; the dated sections below still say `agent.explore`,
which is what it was called when they were written. The internal role id is
still `"explore"` — see that addendum for why the two deliberately differ.

## Problem

A session's history is monotonic: every tool result is retransmitted with
every subsequent provider request, so total input grows as roughly
(rounds × history). Exploration output dominates that history. Two agent
sessions given the same implementation brief (fix the mid-turn
`WaitingForUser` emissions) both died at the model's context ceiling —
final request ~196k input tokens against a 262,144-token window with
65,536 reserved for output — after reading the consumer side of a
3,361-line module. The `fs.grep` locations-only change (`d74a75e`) cut
tool output 28%, but reads compensated (+73%): the ceiling moved from 60
to 82 requests and remained fatal. The edit itself was never the cost:
`fs.edit` accounts for 0–1% of resend weight in every measured session.

The exploration fragments do not need to live in the requesting session's
history at all. What the requester needs is the conclusion ("the emit
sites are X, the consumers are Y"), a few kB. Current-generation harnesses
(OpenCode's task tool is the closest prior art) route open-ended
exploration into a disposable session and return only its final text;
the fragments stay behind and are discarded.

## Decision

1. **New production tool `agent.explore`.** Input: a prompt stating the
   exploration question and the expected deliverable. The tool spawns a
   read-only **parallel session**, waits for its turn to finish, and
   returns the session's final assistant text (plus the spawned session id
   for cost attribution).

2. **A peer, not a child (owner decision).** The exploration session
   references the **same `workspace_root` as the requester** — including
   an isolated requester's worktree — with no worktree isolation of its
   own and **no derivation-tree edge**. The derivation tree remains pure
   code genealogy (`docs/session-relationship-design.md` decision: only
   isolation creates an edge). The exploration session therefore sees the
   requester's uncommitted working-tree state, which is the correct view
   for mid-task exploration; concurrent read-only access is safe.
   Do not use parent/child vocabulary in code or docs for this
   relationship.

3. **First-class session, invisible to the UI, auto-terminated.** It is a
   normal session in the event log and DuckDB projection (its cost is
   measurable exactly like any other session; the requester's
   `ToolCallRequested`/`Finished` events carry the spawned id, which is
   the analytics join key). It is never attached to a pane. On completion
   the tool terminates it.

4. **Read-only toolset, no recursion.** Allowed tools: `fs.read`,
   `fs.grep`, `fs.glob` only. `agent.explore` itself is excluded, so
   recursion is structurally impossible. Every allowed tool is
   `AutoAllowRead`, so approvals are unreachable; if a
   `WaitingForApproval` nevertheless surfaces, the tool call fails
   immediately with an error result — it must never hang waiting for a
   human. The existing built-in role mechanism (`roles::CONFIG_ROLE` is
   precedent) is the likely seam for the restriction; the implementer may
   choose another if it fits better.

5. **The wait is an event subscription.** The requester-side
   implementation subscribes to the exploration session's event stream
   and folds it until `TurnEnded`/`Failed`/`Terminated`, assembling the
   final assistant text from `MessageCommitted`. No bespoke return
   channel. This is deliberate shaping (owner direction): the future
   common abstraction is "subscribe to another agent session's blocking
   events (approvals) and stop events", and this tool must be expressible
   on it without rework. Inter-agent *messaging* stays out of scope under
   its standing agreement (project-level consultation first).

6. **Requester stays responsive; cancellation propagates.** The tool runs
   like `bash` does today (asynchronous completion folded into the turn),
   so `cancel-turn` on the requester terminates the exploration session
   promptly.

7. **Bounded.** Exploration sessions run with a tighter turn cap (25
   rather than the standard cap): they answer one question. On the cap,
   on provider failure, or on the exploration session's own context
   overflow, the tool returns an error result with whatever final text
   exists; the requester recovers by asking a narrower question.

8. **Restart cleanup.** An exploration session is meaningless without its
   waiter. On daemon resume, never-completed exploration sessions are
   terminated rather than re-adopted. If the explore role id alone can
   identify them, no wire change is needed; if a wire field is added it
   must be additive (regenerate the schema artifact —
   `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-sessiond
   wire_schema` — the checker gates non-additive changes).

## Catalog description (draft, adjust wording freely)

> Delegate an open-ended, multi-file exploration to a parallel read-only
> session sharing this workspace. Use it when locating or understanding
> code would take several rounds of grep/glob/read whose outputs you do
> not need verbatim afterwards; state the question and the exact
> deliverable (paths, line numbers, facts). For one to three known files,
> read them directly instead. Returns the exploration's final report.

## Tests (hermetic; the deterministic fallback provider is the harness)

- The spawned session advertises exactly the read-only toolset.
- The requester's history after a call contains the call and the final
  text, nothing else from the exploration.
- Cancelling the requester's turn terminates the exploration session.
- A `WaitingForApproval` in the exploration session fails the call.
- Daemon resume terminates a never-completed exploration session.
- Schema artifact stays green (bless if a wire type changed).

## Measurement (after merge; not the worker's job)

Re-run the `WaitingForUser` brief a third time and compare against the
two failures (82 requests / 196k peak / death): requester context peak,
completion, and total requester+exploration consumption. Total tokens are
expected to *rise* (the exploration session pays its own preamble); the
success criteria are requester headroom and completion.

## Out of scope for v1

Parallel fan-out of several explorations, background/asynchronous
explore, recursion, write-capable delegates, a cheaper model override for
exploration sessions, and inter-agent messaging.

---

## Addendum (2026-07-26): follow-up turns, fork seeding, and the measurement plan

The first measured runs (four runs of the `WaitingForUser` brief; see the
roadmap's context-consumption entry) established: routing fixed adoption
at the task's entry point only; the one delegation's report was accurate
for the question asked but the question omitted the requester's own
measured evidence, so the requester — correctly — distrusted the
conclusion and re-explored by hand. Prompt guidance ("include your
observations") is a silent-failure control: non-compliance is invisible
until a report collides with data downstream. Two structural mechanisms
are added instead, to be **measured against each other**, not decided by
argument. Their shared premise: what a delegate inherits occupies its
context window; what it does not inherit must be told.

### B. Follow-up turns

`agent.explore` gains an optional `session_id` input. When present, the
prompt is sent as a further user message to that still-alive exploration
session and the call waits for that session's next turn to end; when
absent, behavior is unchanged (fresh spawn). Rationale, from run 4: the
requester demonstrably *noticed* the report/evidence contradiction in its
reasoning; what it lacked was a cheap repair action, so it fell back to
re-reading everything itself. A follow-up lands the contradiction — which
itself carries exactly the missing evidence — on a session that already
holds the relevant files in its history.

- **Lifetime**: an exploration session now survives its first completed
  turn and is terminated when the requester's own turn ends (completed,
  failed, cancelled, or the requester session going away). One scope, no
  idle orphans. Daemon-restart cleanup is unchanged: every exploration
  session is terminated on resume.
- Each follow-up call installs a fresh event tap; the fold logic is the
  turn-scoped fold already in place.
- Failure of a follow-up (unknown/terminated session id) resolves as an
  ordinary error tool result naming the fresh-spawn alternative.

### C. Fork seeding (owner direction, simplified from history-rollback)

An exploration session may be seeded with **a copy of the requester's
history so far**, so the delegate sees everything the requester sees —
the structural answer to the evidence gap. No in-session rollback, no
replay marks: the fork child is an ordinary exploration session whose
initial `rig_history` is reconstructed from the requester's persisted
events (the same event-to-history mapping session resume already uses).
Discard-on-completion is plain termination; the requester is untouched.

- **Tail sanitization is mandatory**: at delegation time the requester's
  persisted event stream ends mid-turn, with at least one tool call (the
  `agent.explore` call itself) lacking a result. Seeding must close or
  drop unpaired calls, or the provider rejects the history.
- **Ceiling inheritance is the known cost**: the child's exploration room
  is (model window − requester's current size). Fresh spawns always have
  ~190k; a fork forked at 150k has ~46k. This is the tradeoff being
  measured, not a defect.
- **Cache expectation — grounded, and deliberately pessimistic.** Request
  #0 `cached_input_tokens` across eight same-brief sessions in the
  existing log: three hits of exactly 2,432 tokens, five zeros. So
  synthetic.new does share prefix cache across sessions, but unreliably
  (routing-dependent), and only up to the first byte of divergence.
  A same-preamble fork variant (advertise the parent's full toolset,
  enforce read-only at the execution layer, steer via a trailing message)
  would make the shared prefix span the whole seeded history — but given
  the observed unreliability it cannot be justified on cache grounds
  alone and is **deferred**; v1 implements the restricted-advertisement
  child and pays one uncached ingest of the seed.
- Mode is selected by the harness, not the model: an environment-only
  switch (`HORIZON_EXPLORE_SEED=fresh|fork`, default `fresh`), following
  the `HORIZON_AGENT_EVENT_LOG` convention. Model-visible parameters
  would confound the adoption measurement.

### Independent observation worth its own follow-up

The 2,432-token sharing ceiling exists because the system prompt renders
the per-session working directory *before* the large stable sections
(repository instructions, 16.8KB, identical for every session). Ordering
the preamble stable-first / variable-last would let all sessions share
most of the preamble opportunistically. Separate change, separately
measurable; not part of this slice.

### Measurement plan

Same brief (backlog 48's duplicate-`fs.edit` investigation, with the
measured evidence embedded in the brief per the run-4 lesson), one run
per arm to start, judged qualitatively like the four `WaitingForUser`
runs:

| arm | seeding | follow-up available |
|---|---|---|
| fresh | `fresh` | yes |
| fork  | `fork`  | yes |

Metrics per arm: adoption (initial and follow-up), requester context
peak, completion, requester+delegate total and uncached tokens, and
whether the delegate's report reconciles the embedded evidence without a
manual redo.

---

## Addendum (2026-07-27): follow-up and fork seeding removed; capped runs now summarize

Owner decision, verbatim: 「委譲についてforkと会話継続を削りましょうか。動作を
直しつつ(ターン畳み込みの)設計を相談するうえで邪魔になるので」 -- both
addendum B (follow-up turns) and addendum C (fork seeding) got in the way
of fixing turn-folding behavior and discussing its design, so both are
removed. `agent.explore` returns to v1's shape: one-shot, fresh-seeded,
spawn-and-wait. Concretely:

- **Fork seeding is gone.** `HORIZON_EXPLORE_SEED`, `tools::explore::
  SeedMode`, `ExplorationHost::start`'s `seed_history` parameter,
  `contract::StartSession::seed_history`, and `tools::explore::
  sanitize_seed_history` are all removed. Every exploration starts from
  nothing but its prompt.
- **Follow-up is gone.** `ExplorationHost::follow_up`, `agent.explore`'s
  `session_id` input, and the "an exploration outlives the turn it
  answered" lifetime rule (addendum B) are removed along with it.
  `tools::explore`'s `Input` is the prompt alone. An exploration session is
  now torn down the moment its own turn ends (or immediately on
  cancellation) -- it never outlives the call that spawned it, so the
  `live()` bookkeeping that used to defer teardown until the *requester's*
  turn ended is gone too.
- **Motivating evidence for removing fork seeding specifically**
  (`docs/research/agent-ceiling-death-autopsy-2026-07-26.md`): under
  `hf:MiniMaxAI/MiniMax-M3`, the one fork-seeded exploration in that
  campaign died instantly with a provider 400 ("Error applying chat
  template for MiniMaxAI…") -- the fork-cloned history shape violates that
  model's chat template. A structural mechanism that breaks outright on at
  least one model is not a foundation to build the turn-folding work on.

### Iteration-cap exhaustion is now a partial success, not a bare error

The same autopsy found both fresh-mode explorations in the campaign hit
the 25-iteration cap and returned only a generic error -- 91 tool calls of
real exploration work (42 and 49 tool calls respectively) discarded, with
no report at all. `docs/research/agent-context-reduction-prior-art-2026-
07-26.md` §4 surveyed prior art for this exact moment: neither OpenCode
nor Hermes hard-errors on a step/iteration cap. OpenCode injects a
`MAX_STEPS_PROMPT` ("tools disabled, summarize what's been achieved,
remaining tasks, recommendations") and forces a text-only final turn;
Hermes appends a synthetic user message ("You've reached the maximum
number of tool-calling iterations allowed... provide a final response
summarizing what you've found and accomplished so far, without calling
any more tools") and runs one toolless completion. Both preserve partial
work as the returned result; neither treats the cap as a failure.

Horizon now does the same, gated by role rather than left to the model's
own judgment or a prompt-only convention:

- `roles::RoleDefinition` gained a `summarize_on_cap: bool` field, `true`
  for `EXPLORE_ROLE` (an exploration's whole job is one delegated report,
  so a capped run should still return whatever it found) and `false`
  everywhere else (unchanged behavior for `CONFIG_ROLE` and role-less
  sessions).
- When the rig turn loop's iteration-cap guard trips
  (`providers::rig::session::halt_turn_loop`) for a role with
  `summarize_on_cap`, it runs one forced completion
  (`run_cap_summary_turn`) with every tool definition withheld
  (`RigAgentConfig::allowed_tool_ids` overridden to an empty list) and a
  synthetic user message asking the model to stop and summarize --
  relevant files with paths and line numbers, its best partial answer, and
  what remains unknown. The real, already-executed tool result that
  tripped the guard is folded into history first, exactly where an
  ordinary tool-driven turn would put it.
- If that wrap-up succeeds, the summary is committed as an ordinary
  assistant message and the turn ends as `TurnEnded(HaltedByIterationCap)`
  with the summary as the session's final message -- no different, from
  the requester's side, than an exploration that happened to finish on its
  own. If the wrap-up itself fails (provider error) or gets cancelled,
  `rig_history` is rolled back to exactly where it stood before the
  attempt and the session falls back to the original halt behavior
  (`pending_halt_result` stashed for `Command::ContinueTurn`) -- current
  (pre-2026-07-27) behavior, unchanged.
- `tools::explore`'s `Outcome::into_output` treats a report alongside
  `HaltedByIterationCap` as success (`is_error: false`) with an explicit
  `capped: true` marker, so the caller can tell "capped but still useful"
  apart from an ordinary completed report. Without a report (the wrap-up
  never ran or produced nothing), the cap still resolves as an ordinary
  error -- with no "ask a narrower question" wording anymore, since
  follow-up no longer exists to act on it.
- Doom-loop halts are unaffected: forcing another completion on a model
  that just proved it repeats itself is not the same bet as forcing one on
  a model that ran out of turns making genuine progress, so
  `summarize_on_cap` only applies to `GuardHalt::IterationCapExceeded`.

---

## Addendum (2026-07-27): renamed to `task`, delegation-first routing, two-field input

Owner decision on the direct-API control probes recorded in
`docs/research/agent-delegation-and-batching-probes-2026-07-27.md`. That
probe asked why real sessions almost never delegated, measuring the *first
action* under the full 16-tool catalog at both an exploration entry and an
implementation entry, n=3–5 per cell, against both production models
(`hf:moonshotai/Kimi-K2.7-Code` and `hf:MiniMaxAI/MiniMax-M3`). Adoption
degrades with catalog size (competition with `bash`), not with the size of
the `AGENTS.md` preamble. This addendum ships exactly what that probe found
to work.

### The rename

`agent.explore` → **`task`** as the model-visible id
(`tools::explore::TOOL_ID`; the module keeps its name). Cell C3: the plain
`task` name plus a generic task-tool description moved Kimi from a
degraded 2/3–5/5 baseline to 4/5 and was neutral for MiniMax. Renaming
alone was *not* sufficient for either model — it is a real but partial
lever, kept because it costs nothing.

The internal role id stays `"explore"` (`roles::EXPLORE_ROLE_ID`). It is a
persistence and cleanup identity: it is written into the event log, it is
what makes `horizon-sessiond` refuse to resume an orphaned exploration at
startup, and it is what filters exploration sessions out of the
client-visible session list. Renaming it would touch resume paths and
already-persisted records for no model-visible benefit.

### The two routing clauses

The single hedged line the base system prompt used to carry ("Delegate
open-ended, multi-file exploration to agent.explore when it is available;
keep its report, not a raw read/grep sweep of your own") is gone. In its
place, `prompt::DELEGATION_ROUTING_SECTION` carries the two measured
winners verbatim:

- Exploration entry (cell C5, Kimi 3/3 + MiniMax 5/5): "For an open-ended
  exploration goal, calling task must be your FIRST action — do not run
  bash/ls or read files to orient yourself first; the task agent does its
  own orientation inside its own session, which costs you nothing."
- Implementation entry (cell C7b, Kimi 3/3 + MiniMax 4/5): "For any
  implementation task, your FIRST action must be to delegate the up-front
  investigation and planning to the task tool — even when the change
  targets look already known or the task statement names concrete
  components. State the question and the exact deliverable (relevant
  files, line numbers, a step plan). Do not grep/read/bash to orient
  yourself first. Then implement the changes yourself in this session —
  task agents cannot write files; never delegate the implementation
  itself." (The final sentence is the 2026-07-27 amendment below; the
  measured C7b wording ended "Start implementing only after its report
  returns.")

What the probe ruled out, so it is not tried again without new evidence:
imperative tone alone (E2i lifted both models but not to saturation),
negative routing on `bash`'s own description (C4: zero effect on MiniMax,
harmful for Kimi), renaming alone (C3), and any conditional escape hatch
in the implementation clause — "in an unfamiliar area" (C7) dropped
MiniMax to 2/5, and removing it recovered 4/5. The **explicit ban on
orienting first** is the one intervention that closed adoption for both
models at both entries.

Deliberately not addressed: the sequential-read momentum documented in the
same probe's §3. Instructions cannot fix it (cell MI: an imperative
parallel-call instruction recovered 0/5 against a history of single
calls); only the trajectory's own precedent can, which is part of why
delegation-first matters. No prompt wording is spent on it.

### Conditional inclusion is the mechanism, not hedged wording

The probe measured the clauses **unhedged**, so the shipped wording keeps
no "when it is available" clause. Conditionality lives in whether the
section is included at all: `providers::rig::session::session_extra_sections`
appends it only when the session's advertised toolset actually contains
`task`, decided from `RigAgentConfig::allowed_tool_ids` — the same
allowlist `completion::rig_tool_definitions` filters the advertised catalog
with, already role-adjusted by `role_adjusted_config` before the session
spawns. `None` (role-less) means the unrestricted catalog and therefore
inclusion; `Some(list)` includes it only if `task` is in the list.

The case this exists for is an exploration session itself: its role allows
`fs.read`/`fs.grep`/`fs.glob` and deliberately not `task` (decision 4,
which is what makes recursion structurally impossible), so telling it "your
FIRST action must be task" would be an instruction it could only fail. The
`config` role is excluded by the same rule. Both paths are covered by
`session_extra_sections_includes_the_delegation_block_only_when_task_is_advertised`.

### Two-field input

The input schema is now `{description, prompt}`, both required — the shape
every winning probe cell ran with. `prompt` is unchanged (the question and
the exact deliverable; the spawned session sees it and nothing else).
`description` is a short 3–5 word label, validated in `tools::explore::
Input::parse` but not forwarded to the exploration: it is what the
requester's own transcript row shows while the task runs
(`transcript::tool_call::classify`).

That transcript row is the only place a delegated task announces itself.
There is deliberately no session-title wiring: sessions have no title on
the `StartSession` contract or the session wire at all (titles are
workspace-side strings generated per attached pane, "Agent #N"), and an
exploration session is never attached to a pane and is withheld from the
client-visible session list by decision 3. A title field would have been
invisible by construction.

### Post-ship fixes from the first dogfooded session (2026-07-27)

The first real session run under the addendum above died on a provider
400 (`Error applying chat template for MiniMaxAI/MiniMax-M3 ... 'str
object' has no attribute 'items'`) and, before that, mis-used the new
`task` tool. Two fixes, both shipped against that one session
(`12fd8d14`):

**Double-encoded tool-call arguments (the session killer).** M3 emitted a
tool call whose `arguments` arrived as a JSON *string containing JSON*
instead of an object. Horizon stored it verbatim in rig history; on the
next request the serving layer's M3 chat template iterates
`arguments.items()`, hit the string, and returned 400 — for every
subsequent request, since the poison was already in the persisted
history. M3's template has no string branch; Kimi's tolerates both, so
the failure is M3-specific while the repair is model-agnostic.

`providers::rig::completion` now normalizes at two points. A streamed
tool call whose `arguments` is a string that decodes to a JSON object is
decoded *before* dispatch and before it becomes history
(`repair_double_encoded_tool_arguments`), so the observed call executes
as intended instead of failing input validation. Anything still not an
object when a history message is assembled is replaced with `{}`
(`replay_safe_tool_arguments`), covering rig's own `stream.choice`
aggregation, the cancelled-turn `partial_assistant_message` path, and the
history rebuilt from persisted events on resume
(`mapping::rig_tool_call_from_request`). The `{}` substitution trades
history fidelity for session survival deliberately: only the
provider-facing replay is rewritten, and the model already learned its
input was malformed from the tool's own error result. The provider
payload persisted with `ToolCallRequested` still records the raw
emission.

**`task` is for investigation and planning only.** The same session
delegated the *implementation* to `task` children twice ("Read the files
as needed for the changes you make"). Task children are read-only by
construction (the `fs.read`/`fs.grep`/`fs.glob` whitelist of decision 4),
so each degenerated into another exploration that spent its turn budget
producing nothing actionable. The generic `task` name reads as
write-capable in these models' training distribution, so the constraint
is now stated rather than implied: the catalog description gains "Task
agents are read-only — they investigate, locate, and plan, but cannot
write files or run commands that modify state; implementation happens in
this session after the report returns", and
`prompt::DELEGATION_ROUTING_SECTION`'s implementation clause ends "Then
implement the changes yourself in this session — task agents cannot write
files; never delegate the implementation itself" in place of "Start
implementing only after its report returns". Both edits are additive and
terminal: every measured word from cells C3/C5/C7b is untouched, so this
does not re-open the probe's result.

---

## Addendum (2026-07-28): `task` is asynchronous; spawn-and-wait is gone

Implemented per `docs/agent-async-task-design.md` (owner-approved
2026-07-28), which is the scope record for this change and carries the
reasoning; this addendum records only what it means for the decisions
above.

- **Decision 1's "waits for its turn to finish" is void.** `task` returns
  immediately with `{session_id, description, status: "started"}`. Its
  report reaches the requester as a *notification message* injected before
  a later provider round, or as the synthetic input of a turn the
  completion starts by itself. There is no blocking path left in the tool,
  and no `Execution::Started`: the launch call resolves synchronously with
  its receipt.
- **Decision 5's event subscription survives and is now named.** The
  waiter still folds the child's event stream until it reaches a terminal
  state; what changed is where the result goes (a per-requester queue plus
  a wake, instead of the requester's `async_results` channel). The
  daemon-side plumbing moved into
  `crates/horizon-sessiond/src/session/subscription.rs`, documented as the
  general "subscribe to another session's stop/blocking events" seam that
  approval forwarding for future write-capable children extends with one
  more event kind.
- **Decision 6's "cancellation propagates" is deliberately reversed.**
  Children are session-scoped, not turn-scoped: `cancel-turn` leaves them
  running (mainstream behavior — interrupting the requester must not
  vaporize in-flight investigation), and only the requesting session ending
  terminates them. `tools::explore::cancel_if_running` is gone from
  `tools::processing` accordingly.
- **New: a concurrency cap of 3** per requester, and a **`task_output`**
  fetch tool (ownership-checked; advertised only alongside `task`, through
  the same allowlist seam `DELEGATION_ROUTING_SECTION` rides).
- **Decision 8's restart cleanup is unchanged**: still keyed on the explore
  role id alone, still terminating never-completed children on resume.
- **The measured routing clause takes its second amendment**, again
  confined to the final report-return sentence: "Keep working while it
  runs; its report will arrive as a notification, and you implement the
  changes yourself in this session — task agents cannot write files; never
  delegate the implementation itself." The two orienting bans (cells C5 and
  C7b) stay byte-identical, so the probe's result is still not re-opened.
- **Wire**: `contract::MessageRole::TaskNotification` marks a delivered
  notification in the event log, and its placement before the
  `#[serde(other)] Unknown` catch-all is what required
  `SESSION_PROTOCOL_VERSION` 13 → 14.
