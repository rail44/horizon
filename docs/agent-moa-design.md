# Mixture-of-Agents as a selectable model (`[[moa]]`)

Status: designed 2026-09-19..20 (owner consultation; every numbered
decision below is the owner's, quoted where a quote exists). **Not
implemented.** One judgment point is still open for owner review — see
"Open for review".

This document is the self-contained record. An implementation brief should
point here rather than restate it.

## Why

The owner's aim, in the owner's words (2026-09-19/20):

- 「難しい局面での解の質を上げたい」
- 「『難局である』という判断がAIには難しいので、モデル選択で高性能なもの
  として扱える手段がほしい」 — the human decides when a situation is hard,
  and expresses that by choosing a model. Any shape in which a model
  decides when to bring in other models (an advisor/consult tool the agent
  calls at its own discretion) is outside the requirement.
- 「LLMをローカル動作させる必要はない」 — what runs locally is the
  combining mechanism; the member models are cloud APIs.
- The motive is cost: 「オープンモデルの進歩が激しく、claudeやopenaiに
  比べてプロバイダが格安で提供するように状況が変化しているので、その中でも
  コストパフォーマンスに優れるモデルいくつかを組み合わせることで性能面も
  得ることができれば、コーディングなどの作業を超えて設計相談などの判断力を
  要するタスクでも常用したい」. Switching to an expensive single model is
  therefore not an answer (「既にコストの面でKimi-k3が突出して高いので」);
  an expensive model is at most a yardstick.
- What is to be built is MoA: 「そもそも私が実現したかったのはMoAであって」.
- Build first, improve through use: 「とりあえず作ってから改善を経てみれば
  いい」.

### Why inside Horizon

The delivery shape wanted is Sakana Fugu's — a multi-model system that
presents as one model. An external OpenAI-compatible proxy registered as a
`[[providers]]` entry would need no Horizon change, but no existing
implementation survives Horizon's request shape. Every provider round
carries `tools`, the full tool-call history, and `stream: true`
(`providers/rig/completion.rs`, `run_provider_stream`). Checked against
source on 2026-09-19:

| Candidate | What happens with Horizon's requests |
|---|---|
| Maestro (`walidboulanouar/maestro`) | With `tools` present it becomes a single-model passthrough; verify/escalate and `maestro-ultra` are disabled (`src/core/orchestrator.ts`, "re-running mid tool-call would break the agent"). |
| OptiLLM (`moa`, `bon`, …) | Flattens the conversation to `(system_prompt, initial_query)` strings; tools never reach the inner calls; one base model sampled repeatedly, not different models. |
| OpenFugu | `serve.py` uses only the last user message; no tools, history, or streaming; needs local Qwen3-0.6B inference. |
| Thug-Fugu | Requires a string `content` on every message and carries role+content only, so a tool-call history is not representable (source reading, not run). |

Fugu itself, and the TRINITY / Conductor coordinators it builds on, are
closed (no weights, no code).

### Why this granularity

MoA as published and as implemented (arXiv:2406.04692;
`togethercomputer/MoA`) is tool-less single-turn: each proposer writes one
complete answer to the user's message, the aggregator rewrites. The
tool-using studies that show gains keep that unit — each agent
investigates on its own and produces a complete answer, and the answers
are aggregated:

- arXiv:2604.11753 (Princeton, v3 2026-08; agentic search and deep
  research, six benchmarks; GLM-4.7 / Qwen3.5 / MiniMax-M2.5; eight
  independent rollouts of the *same* model; not coding). Averages:
  Pass@1 30.01 / 40.15 / 44.02 → concatenating final answers and
  re-synthesizing (plain MoA-style aggregation) 42.58 / 52.78 / 54.90 →
  an aggregator that can inspect the rollouts' records with tools
  47.90 / 55.83 / 57.31. Synthesis beat selecting one rollout, most on
  open-ended tasks: "quality is distributed across trajectories, so no
  single trajectory dominates". Exposing thinking traces mattered little
  (−0.03 to −2.77); what mattered was access to tool observations.
- arXiv:2510.01279 (TUMIX, Google, ICLR 2026): agents with different tool
  strategies answer, share answers, refine for a few rounds; +3.55%
  average over the best baseline (including Self-MoA) on Gemini-2.5.
- arXiv:2406.04692 §3.3: "Mixture-of-Agents significantly outperforms LLM
  rankers" — rewriting with the proposals as reference beats picking one.

Not evidenced, and therefore to be learned by use: MoA applied per tool
step (no study found); mixing *different* models in a tool-using setting
(both studies above repeat one model; arXiv:2502.00674, single-turn, 2024
open models, found repeating the best model ≥ mixing); coding tasks. One
local observation favors mixing (2026-09-20 read-compare on the owner's
own board #26 consultation, n=1): all five DeepSeek-V4.1-Flash samples
missed the status-based alternative the owner took next, while
Qwen3.8-27B and GLM-5.3-Flash raised it.

Counter-evidence considered: arXiv:2512.08296 reports every multi-agent
topology degrading sequential planning by 39–70% and negative returns once
the single-agent baseline exceeds 45% — under *matched total iterations*,
i.e. each agent's budget is cut ("multi-agent systems fragment the
per-agent token budget"). This design does not split a budget; the owner's
premise is that extra calls to cheap models are affordable.
arXiv:2602.18998 reports a verification gap when whole-task samples are
*selected* by the model itself; this design synthesizes instead.

## Decisions (owner, 2026-09-19..20)

1. **A selectable model; the human chooses it.** MoA appears in model
   selection and is chosen per session (or switched to mid-session) by the
   owner. Nothing in the mechanism judges difficulty.

2. **It is MoA, and the aggregator writes its own answer.** The aggregator
   uses the proposals as reference; it does not adopt one wholesale
   (「『選ぶ』というのが、あるモデルの回答を全面的に受け入れるという意味だとは
   思っていなかった」).

3. **Unit: one owner message = one MoA pass, two layers.** Proposers
   answer, then the aggregator answers (「2層で始めましょう」). The paper's
   default of three layers (a middle round where proposers rewrite after
   seeing each other) is deferred; it roughly doubles cost and latency.

4. **Proposers are `task`-shaped sessions, launched by the harness.** Each
   proposer is a read-only peer session of the existing `task` form
   (`docs/agent-explore-design.md`, `docs/agent-async-task-design.md`):
   `fs.read`/`fs.grep`/`fs.glob` only, no approvals reachable, iteration
   cap 25 with summarize-on-cap, a first-class session in the event log
   and the DuckDB projection, never attached to a pane. The harness starts
   them automatically when an owner message arrives in a MoA session — the
   model never decides to. Each proposer investigates independently and
   writes a complete answer; one that wants to read more keeps reading
   while the others wait (「もっと読みたいと言っているモデルのみ続行すれば
   いいのではない？」).

5. **Proposers receive the conversation as text, and nothing else.** The
   prompt carries the owner's messages and the aggregator's answers so
   far, written out as plain text, plus the new owner message. Earlier
   rounds' investigation is *not* passed; a proposer re-reads what it
   needs (「まずは前者にしましょう」). Plain text is deliberate:
   structured history seeding was built for `task` and removed by owner
   decision on 2026-07-27 after a fork-seeded child died on a chat-template
   400 (`docs/agent-explore-design.md`, 2026-07-27 addendum); with several
   model families in one pass that failure is likelier, and text avoids it.
   Giving proposers `recall` over earlier rounds is deferred until
   re-reading proves wasteful.

6. **The aggregator can inspect the proposers' records, through the
   existing recall tools.** Besides the proposals, the aggregator can look
   into each proposer session's persisted record (tool calls and tool
   results included) with `recall.search` / `recall.read` and read full
   reports with `task_output` (owner: 「toolでduckdbから確認できる機構は既に
   あるのでこれを使うのでよさそうですかね」). These map onto the Princeton
   aggregator's tools (get_solution / search_trajectory / get_segment).
   One extension is needed: `recall.search` takes scope `"session"` (own)
   or `"all"` today; it needs a specific-session filter. The store-level
   `search_history(scope: Option<SessionId>, …)` already takes one.

7. **Only the aggregator writes or executes.** Proposers stay read-only,
   which is the standing decision for `task` children
   (`docs/agent-async-task-design.md` decision 7). Edits, `bash`, and any
   approval happen in the aggregator's ordinary turn.

8. **The pane shows the aggregator's answer only.** Proposals and
   proposer activity are not rendered. They persist in the event log and
   DuckDB like any session. The live task-progress rows (2026-09-10) do not
   apply here; the owner's reason for those rows: 「タスクの作業が見えて
   ほしいのは、セッションに依頼した作業のうち具体的に何を委譲されて進めて
   いるのかを知りたいのが理由で、今回はこれに当てはまらない」. While
   proposers run, the pane shows the ordinary in-progress state.

9. **Configuration lives in the config file, with model IDs written
   directly, and providers may be mixed from the start.** A new
   `[[moa]]` array; each member names a `[[providers]]` entry and a model
   ID. No aliases — the owner intends to remove the alias feature
   (「alias自体が不要なのに足された機能なので削ろうとしています。なので、
   モデルIDを直接記述して指定するようにしたい」), so nothing here may
   depend on it. Key names are provisional:

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

   Listing the same member twice is the paper's single-proposer setting.
   Several `[[moa]]` entries may coexist. `Reload Config` applies like
   `[[providers]]` does: new sessions see the change.

   Carried as the designer's reading, not yet confirmed by the owner: in
   model selection, `moa` sits beside the provider entries and its items
   are the `[[moa]]` names. Which models to use, how many, and who
   aggregates are the owner's config values, not design decisions.

## Open for review

- **Cancelling the aggregator's turn.** `task` children survive
  `cancel-turn` by owner decision (`docs/agent-async-task-design.md`
  decision 4: interrupting the requester must not vaporize in-flight
  investigation). MoA proposers are internal to the turn — their output
  has no consumer once the turn is cancelled — so the proposal here is
  that cancelling stops them. This runs against the standing decision's
  direction and needs the owner's confirmation before implementation.

## Implementation shape (held by the implementing session)

Not owner decisions; recorded so the constraints are not rediscovered.

- **Seam.** `registry::Provider` is session-granular, so MoA is not "one
  more `Provider` impl". The pass belongs in the rig session loop around a
  turn that answers an owner message: launch proposers, wait for all of
  them (a barrier — today's `task` delivery coalesces completions but does
  not wait for a set), then run the aggregator's turn with the proposals
  available for *every* provider round of that turn (the aggregator may
  call `recall`/`fs` tools before answering). Tool-result rounds and
  follow-on rounds never start another pass.
- **Handing the proposals to the aggregator.** Together's reference
  implementation appends them to the system prompt, and
  `prompt::system_prompt`'s `extra_sections` is the existing mechanism for
  that. It changes the head of the request each pass, which forfeits the
  provider's prompt cache for the whole history behind it. Cached reads
  are ~20× cheaper on the owner's current provider ($0.03 vs $0.6 per 1M
  for DeepSeek-V4.1-Flash), and the cache does work there: in the
  2026-09-20 replays a repeated prefix hit ~99.8% (58.5% of all prompt
  tokens over 26 calls, the first call per model and question being a
  necessary miss). The alternative is to project the proposals into the
  provider-facing view at a position that stays fixed for the turn — right
  after the owner message that opened it — through
  `clearing::history_for_provider_request`, the single seam between
  canonical history and what a request carries, where the standing-role
  memory document is already projected in as a user-role message without
  entering canonical history. That seam's contract is that consecutive
  request builds stay byte-identical while nothing changed; an injection
  has to keep it. Decide by measurement; either way the proposals must
  not enter `rig_history` (decision 8, and decision 5's input is owner
  messages plus aggregator answers only).
- **Aggregator instruction.** Start from the paper's
  Aggregate-and-Synthesize prompt (critically evaluate, do not replicate).
- **Spawning a proposer on a named entry and model.** The exploration host
  pins the requester's `provider_id` today
  (`crates/horizon-agentd/src/session/exploration.rs`); MoA needs a
  per-member entry (`builtin.agent.rig.<name>`) and model ID. The role
  model seat is `&'static str`, so it is not the carrier.
- **Failure handling.** A failed or capped proposer contributes whatever
  report it has (the existing empty-report rule applies); the pass
  proceeds with the rest. If none produce a usable report the aggregator
  answers alone and says so in the log, not in the pane. An unavailable
  member (missing key) is skipped the same way.
- **Record linkage.** `task` ties a child to its requester through the
  launching tool call's events. Harness-launched proposers have no such
  call, so the turn ↔ proposer-session-ids relation needs its own durable
  record — improving by use depends on being able to pull "what did each
  member say for this message" out of DuckDB.
- **Echoing the transcript format.** Handing a conversation over as text
  can make a model continue the transcript instead of answering — seen
  once in ~70 calls on 2026-09-20 (a DeepSeek sample invented a tool call
  and its result before answering). Detect and drop or retry.
- **Context-window discovery per member.** `model_limits` reads
  `GET {base_url}/models` with `OPENAI_API_KEY` hardcoded
  (`providers/rig/model_limits.rs`), as do the judge and title clients.
  With mixed providers, a member on another key gets no window and its
  Tier 1 clearing never fires. Make the lookup follow the entry's
  `api_key_env`.
- **Wire and config.** `[[moa]]` is a new `horizon-config` table (the
  unknown-key warning machinery covers typos). Surfacing MoA entries to
  the model picker touches the `list_providers` summary type — regenerate
  `agent-wire.json`; a non-additive change needs the protocol bump and
  the full-restart discipline in `AGENTS.md`.
- **Tests.** The deterministic fallback provider is the harness, as for
  `task`.

## Out of scope for v1

Three or more layers; `recall` for proposers; write-capable proposers;
MoA per tool step; any learned or automatic choice of when to use MoA;
rendering proposals in the pane.

## How we will know

By use, per the owner's direction. Every proposer session is an ordinary
session in the event log and DuckDB, so "what each member contributed to
this answer" and "what it cost" are queries, not new instrumentation
(`agent-inspect` skill). The 2026-09-20 read-compare (`q1`–`q3` over the
owner's own consultations) is the only local data so far and was judged
by the owner as too thin to decide anything (「題材が悪くてあんまり差を
見出しづらいのと、実際にやりとりをしてみないと感触は分からない」).
