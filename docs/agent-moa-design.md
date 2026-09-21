# Mixture-of-Agents as a selectable model (`[[moa]]`)

Status: implemented (v1).

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
tool call and result ahead of the answer). Such an output is trimmed to
its answer, or dropped when nothing usable is left; it is not retried.

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
`[[moa]]` tables may coexist. `Reload Config` applies as it does for
`[[providers]]`: new sessions see the change.

In model selection `moa` sits beside the provider entries, and its items
are the `[[moa]]` names.

`moa` is a reserved group name: a `[[providers]]` entry called `moa` is
warned about and cannot be selected. An entry whose aggregator's provider
has no key is refused on selection with the reason; the `moa` group is
marked unavailable only when none of its entries can run (the group's one
`ProviderSummary` carries one availability flag, and its items are plain
names with no per-item availability).

## Where it lives

- **Config.** `crates/horizon-config`: `[[moa]]` parsing, resolution, and
  unknown-key warnings. `crates/horizon-agentd/src/providers.rs::moa_configs`
  translates it for the agent at startup and on `Reload Config`.
  `crates/horizon-agent/src/config.rs`: `MoaMember` / `MoaEntry` /
  `MoaTable`; a member's availability (`api_key_present`) is resolved from
  the `[[providers]]` entry it names.
- **Selection.** `registry.rs` registers `builtin.agent.moa.<name>` per
  entry, running the aggregator's `{provider, model}`.
  `session/state.rs::apply_set_session_model` handles `provider == "moa"`.
  `horizon-agentd/src/session/connection.rs` appends the `moa` group to
  `list_providers` as one more `ProviderSummary` and answers
  `list_provider_models("moa")` with the entry names from the config (no
  request); the wire needs no MoA-specific surface and the shell needed no
  edit.
- **The pass.** `providers/rig/session/moa.rs`: the conversation proposers
  are given (kept for every session, so a mid-session switch into `moa`
  starts with context; rebuilt from persisted events on resume), the
  proposer prompt, the barrier, the injected block. `tools/moa.rs`: launch
  and watcher threads over `explore::fold_until_terminal`. The
  `Command::UserMessage` arm in `session/state.rs` runs the pass before
  the turn; tool-result, continue-turn, and task-notification rounds do
  not.
- **Spawning on a named entry and model.** `ExplorationRequest { prompt,
  provider, model }` replaces `ExplorationHost::start(prompt)`. The daemon
  spawns the proposer on `builtin.agent.rig.<provider>` and sends
  `SetSessionModel` ahead of the prompt on the same ordered channel.
- **Record linkage.** `Event::MoaPassStarted { entry, proposers:
  [{session_id, provider, model}] }`, emitted at launch with the sessions
  that actually started; event kind `moa_pass_started`, stored as an
  ordinary event-log / DuckDB row and ignored by the frame fold.
- **Smoke test.** `crates/horizon-agentd/tests/e2e.rs`,
  `moa_pass_runs_against_a_real_provider`: `#[ignore]`d and gated on
  `HORIZON_MOA_SMOKE=1` plus a real key, so the gate never runs it. It
  drives one pass against the real provider and checks that each
  proposer's requests carry its pinned model, that proposers used tools,
  that the answer is right, and that the block never enters history.

## Constraints the code depends on

- **Where the proposals go in the request.** They are projected into the
  provider-facing view as one user-role message at an index fixed for the
  turn — the length `rig_history` had before the turn's opening message —
  through `clearing::history_for_provider_request`. Appending to the
  system prompt instead would change the head of the request on every
  pass and forfeit the provider's prompt cache for the history behind it.
  The index keeps everything ahead of the block byte-identical across the
  turn's rounds and never lands between a tool call and its result.
- **A key-less member is never launched.** A session on a key-less entry
  answers from the deterministic fallback responder, and the event fold
  cannot tell that text from a model's answer.
- **The clearing window follows the model.** A proposer's model is pinned
  after session construction, so the window discovered at construction
  belongs to the entry's default model. `apply_set_session_model`
  re-discovers it (for every session, picker switches included);
  `ClearingState::adopt_window` moves only the window and leaves the
  frozen cleared set and the last measured input size in place.
- **Context-window discovery authenticates per entry.** `model_limits`
  reads the entry's `api_key_env` and caches per `(base_url, api_key_env,
  model)`. The judge and title clients still read a hardcoded variable.
- **Wire.** `contract::Event` gained a variant: additive, no protocol
  bump. A shell process started before the rebuild cannot decode
  `MoaPassStarted` (skipped per item); restarting the app avoids the
  noise.
- **An entry with no `default_model` has no default model** and falls back
  to rig's built-in `gpt-4o-mini` for the construction-time window lookup
  (one wasted `/models` request per key, cached).
- **Echo trimming keys on `User:` at line start.** A legitimate answer
  containing that string on its own line is cut there.

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
