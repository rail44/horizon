# Mixture-of-Agents as a selectable model (`[[moa]]`)

Status: designed, not implemented.

A `[[moa]]` entry is a model the user can pick in model selection. A
session running on it answers each user message with one Mixture-of-Agents
pass: several proposer models each investigate and answer, then an
aggregator model writes the session's answer with the proposals as
reference. Nothing in the mechanism decides *when* to use it; selecting
the entry is what turns it on.

## Flow

One pass per user message, two layers.

1. A user message arrives in a MoA session. The harness starts one
   proposer session per configured member. The model never launches them.
2. Each proposer investigates on its own and writes a complete answer.
   Proposers do not wait on each other; one that needs to read more keeps
   reading while the rest sit finished.
3. When every proposer has finished (or failed), the aggregator's turn
   runs. It writes its own answer; the proposals are input to it, not
   candidates to pick from.
4. Tool-result rounds and follow-on rounds inside the aggregator's turn
   never start another pass.

## Proposers

A proposer is a `task`-shaped session (`docs/agent-explore-design.md`,
`docs/agent-async-task-design.md`): a read-only peer on the requester's
`workspace_root`, `fs.read` / `fs.grep` / `fs.glob` only, no approval
reachable, iteration cap 25 with summarize-on-cap, a first-class session in
the event log and the DuckDB projection, never attached to a pane. Unlike a
`task` child it runs on its member's own `{provider, model}` rather than
the requester's.

Input is the conversation written out as plain text — the user's messages
and the aggregator's answers so far — followed by the new user message.
Nothing else: no tool calls, no tool results, no earlier proposals. A
proposer that needs something read in an earlier round reads it again.

Plain text is a constraint, not a style: a structured history produced
under one model family can violate another family's chat template and be
rejected with a provider 400 (the failure recorded in
`docs/agent-explore-design.md`, 2026-07-27 addendum). A pass mixes
families by construction.

Handing a conversation over as text can make a model continue the
transcript instead of answering (observed once in ~70 calls: an invented
tool call and result ahead of the answer). Such an output is detected and
dropped or retried.

## Aggregator

The aggregator is the MoA session's own model call, so everything around
it is unchanged: the input queue, approvals, the judge, the turn-loop
guards, Tier 1 clearing. It alone writes or executes; edits, `bash`, and
approvals happen in its ordinary turn.

The proposals stay available to it for every provider round of the turn
(it may call tools before answering). Beyond the proposals it can inspect
each proposer session's persisted record — tool calls and tool results
included — with `recall.search` / `recall.read` (both take a `session_id`)
and read a full report with `task_output`.

Proposals never enter `rig_history`. Canonical history holds user messages
and aggregator output only, which is also exactly what the next pass's
proposers are given.

The instruction starts from the Mixture-of-Agents paper's
Aggregate-and-Synthesize prompt: evaluate the proposals critically, do not
replicate them.

## What the pane shows

The aggregator's output only. While proposers run the pane shows the
ordinary in-progress state; proposer activity and proposals are not
rendered, and the live task-progress rows are not used. Every proposer
session persists in the event log and DuckDB like any other session.

## Cancelling

Cancelling the aggregator's turn stops that pass's proposers. This differs
from `task` children, which survive `cancel-turn`: a proposer's output has
no consumer once its turn is gone. `task` children the aggregator launches
itself keep the `task` rule.

## Failure

A failed, capped, or unavailable (missing key) proposer does not fail the
pass. It contributes whatever report it has, under `task`'s empty-report
rule, and the pass proceeds with the rest. With no usable proposal the
aggregator answers alone; the log says so, the pane does not.

## Configuration

```toml
[[moa]]
name = "mix"
aggregator = { provider = "synthetic", model = "hf:deepseek-ai/DeepSeek-V4.1-Flash" }
proposers = [
  { provider = "synthetic", model = "hf:deepseek-ai/DeepSeek-V4.1-Flash" },
  { provider = "synthetic", model = "hf:zai-org/GLM-5.3-Flash" },
  { provider = "another",   model = "..." },
]
```

`provider` names a `[[providers]]` entry (connection and key variable come
from there); `model` is a model ID written directly. Members may sit on
different entries. Listing a member twice samples that model twice. Several
`[[moa]]` tables may coexist. The table does not use the `[[providers]]`
alias map. `Reload Config` applies as it does for `[[providers]]`: new
sessions see the change.

In model selection `moa` sits beside the provider entries, and its items
are the `[[moa]]` names.

## Implementation constraints

- **Seam.** `registry::Provider` is session-granular, so a pass is not one
  more `Provider` impl. It belongs in the rig session loop around a turn
  that answers a user message. Waiting for a *set* of sessions is new:
  `task` delivery coalesces completions but does not wait for all.
- **Where the proposals go in the request.** Appending them to the system
  prompt (`prompt::system_prompt`'s `extra_sections`) changes the head of
  the request on every pass and forfeits the provider's prompt cache for
  the whole history behind it. Projecting them into the provider-facing
  view at a position fixed for the turn — right after the user message
  that opened it — keeps the cached prefix.
  `clearing::history_for_provider_request` is the single seam between
  canonical history and what a request carries, and it already projects
  the standing-role memory document in without touching canonical history;
  its contract is that consecutive request builds stay byte-identical
  while nothing changed.
- **Spawning on a named entry and model.** The exploration host pins the
  requester's `provider_id`
  (`crates/horizon-agentd/src/session/exploration.rs`). A member needs its
  own entry (`builtin.agent.rig.<name>`) and model ID; the role model seat
  is `&'static str` and cannot carry a configured ID.
- **Record linkage.** A `task` child is tied to its requester through the
  launching tool call's events. Harness-launched proposers have no such
  call, so the turn ↔ proposer-session-ids relation needs its own durable
  record, queryable from DuckDB.
- **Context-window discovery per member.** `model_limits` must
  authenticate with the entry's `api_key_env`; with a hardcoded variable a
  member on another key gets no window and its Tier 1 clearing never
  fires.
- **Wire.** Surfacing MoA entries to the model picker touches the
  `list_providers` summary type. A non-additive change needs the protocol
  bump, and a bump needs a full app restart (`AGENTS.md`).

## Not in v1

More than two layers; `recall` for proposers; write-capable proposers; a
pass per tool step; any automatic choice of when to use MoA; rendering
proposals in the pane.

## Reference material

- Mixture-of-Agents, arXiv:2406.04692 (2024-06), and
  `togethercomputer/MoA`. Tool-less, single-turn. Layers of proposers,
  each seeing the previous layer's answers; a final aggregator. §3.3: the
  aggregator rewriting with the proposals as reference outperforms an LLM
  ranker that picks one. The reference implementation appends proposals to
  the system prompt and keeps only the aggregator's answer in history.
- arXiv:2604.11753 (Princeton, v3 2026-08). Agentic search and deep
  research, six benchmarks; GLM-4.7, Qwen3.5, MiniMax-M2.5; eight
  independent tool-using rollouts of one model, then aggregation. Average
  scores: single rollout 30.01 / 40.15 / 44.02; concatenating the final
  answers and re-synthesizing 42.58 / 52.78 / 54.90; an aggregator that
  searches and reads the rollouts' records with tools 47.90 / 55.83 /
  57.31. Synthesis beat selecting one rollout, most on open-ended tasks.
  Hiding thinking traces from the aggregator cost 0.03–2.77; the gain came
  from access to tool observations. Same model throughout; not coding.
- TUMIX, arXiv:2510.01279 (Google, ICLR 2026). Agents with different tool
  strategies answer, share answers, and refine for a few rounds; +3.55%
  average over the best baseline, Self-MoA included, on Gemini-2.5.
- Self-MoA, arXiv:2502.00674 (2025-02). Single-turn, 2024 open models:
  sampling the best single model repeatedly scored 65.7 on AlpacaEval 2.0
  against 59.1 for a mixed pool and 53.1 for the best model alone.
- Not covered by any of the above: a pass per tool step; mixing different
  models in a tool-using setting; coding tasks.
- Prompt-cache behavior on the current provider, from replaying three
  consultations on 2026-09-20: a repeated prefix was served ~99.8% from
  cache (58.5% of all prompt tokens over 26 calls, the first call per
  model and question being a miss); cached reads were billed at $0.03
  against $0.6 per 1M tokens for DeepSeek-V4.1-Flash.
