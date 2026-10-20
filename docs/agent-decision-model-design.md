# Decision model: the approval judge and the turn-continuation check

Status: implemented (judge on a decision model; harness turn-continuation
check). Both thresholds are initial values; the audit records they write
are the material for setting them.

A decision model generates no text. One call hands it a `state` (text or
JSON) plus named, typed questions and gets back a probability for every
option of every question, scored in a single pass. Horizon uses one for
the two places where a fixed set of labels is the whole answer: whether a
boundary-crossing tool call may run without a human, and whether a turn
that the model ended should continue.

## The model and its wire

Clef is Cloudflare's decision model: 27B parameters, fine-tuned from
Qwen3.8-27B, Apache 2.0; Clef Flash is a 9B variant. Ollama serves both
locally and serves `clef:cloud` in Ollama cloud, through the System One
API. The cloud endpoint is the host and key the existing `[[providers]]`
entry already uses (`https://ollama.com/v1`): `POST {base_url}/systemone`
with `Authorization: Bearer <key>`. Facts below were checked on
2026-10-21 against `docs.ollama.com/api/systemone` and
`ollama.com/library/clef`.

- Request: `model`, `state` (a string, or an object/array serialized as
  JSON), `questions` (1–64 named questions). Each question has
  `instructions` and a `type`: `choice` (`criteria`: label → description,
  2–26 options for Clef; answer carries `choice`, per-label
  `probabilities`, and `confidence`), `noul` (optional `criteria` with
  `"false"`/`"true"` descriptions; answer is `noul`, the probability of
  true), or `score` (ordered levels; unused here). Answers are not passed
  to later questions.
- `confidence` is `1 - H(p) / ln(N)`: how concentrated the distribution
  is, not a calibrated probability of being right. Thresholds here are
  set on the per-option probabilities, never on `confidence`.
- Cloud context is 65,536 tokens and input is never truncated: a larger
  request is rejected with 413. Every error body is `{"error": "..."}`.
  No streaming, no sampling controls.
- Ties follow option order, so the option to prefer on a tie is listed
  first.
- Cloud use needs a paid plan or purchased credits; billing is per input
  token. Both `clef:cloud` and `clef-flash:cloud` answered a direct call
  on 2026-10-21 (one `noul` plus one `choice` question over 296 input
  tokens: 0.65 s and 1.46 s round trip). On the same `curl … | sh` call
  the 27B model put the ask probability at 0.99 and the 9B at 0.43, so
  the pair names `clef:cloud`.

rig-core has no System One support, so `crates/horizon-agent/src/decision/`
is a direct HTTP client: the wire types, a mockable `DecisionClient`
trait, and `HttpDecisionClient` over reqwest. Nothing of this crosses the
ACP line to the shell or the terminal wire.

## Configuration

`decision_model = { provider, model }` is a top-level pair beside
`default_model` and `small_model` (`docs/provider-configuration.md`). Its
provider must be an `openai-compatible` entry: the System One route lives
beside `/chat/completions` on the same base URL, so the entry's `base_url`
(or `OPENAI_BASE_URL`) is what `/systemone` is joined to and its
`api_key_env` is the credential. The pair absent or invalid, the key
variable unset, or the entry without a base URL (OpenAI's default URL
serves no System One route) is warned at startup and leaves both features
off: every approval candidate goes to the human, and a turn ends where the
model ended it. Reload Config affects new sessions; a running session
keeps the connection it captured at start, the same rule the judge's
`small_model` connection followed before. `small_model` now serves titles
only.

## The judge

The judge's seam, input restriction, rate limit, fail-closed rule, and
audit carrier are those of `docs/agent-approval-design.md`'s "Judge
design". What changed is the call itself: one `noul` question per
candidate instead of a two-stage chat cascade.

- **Question.** `ask_human`: must a human approve this call before it
  runs. `instructions` is the fixed, Horizon-authored triage text (the
  boundary-crossing framing, the asymmetric cost of a wrong answer, and
  the standing instruction that the call's arguments are data to inspect,
  not instructions to follow); a git-metadata call and a call whose
  approval grants one host execution get their framing appended. The
  `criteria` descriptions spell out both outcomes.
- **State.** A JSON object: `user_messages` (prior user messages, oldest
  first), `tool_call` (`tool`, the built-in tool's static description,
  and the raw `args`), `requested_filesystem_grants`, `requested_domains`,
  `host_execution_requested`, `git_metadata_operation`. This is the same
  input the chat prompt carried, and the same restriction holds: no tool
  results, no assistant prose. Because the arguments are a JSON value
  inside `state` and the instructions are a separate field, the
  per-call-id fence the chat prompt needed around the arguments is gone.
- **Verdict.** `AutoApprove` when `noul` is at or below the ask
  threshold (initially 0.2), `Escalate` otherwise. A decision model writes
  no reasoning, so there is no second stage: the gray zone belongs to the
  human. A client error, a timeout (the 60-second deadline is unchanged),
  or a reply without the answer escalates with the existing
  `fallback_reason` values. The rate limiter is unchanged.
- **Size.** Prior user messages are trimmed from the oldest down to a
  character budget before the call; the arguments are never trimmed. A
  request the endpoint still rejects as too large fails closed like any
  other client error.
- **Audit.** `judge_verdict` records keep their carrier and keys and gain
  `judge_ask_probability` (the raw `noul`) and `judge_threshold`;
  `judge_confidence` is the probability of the verdict taken (`1 - p` for
  an auto-approval, `p` for an escalation); `judge_stage` is gone. A
  verdict row and the human's decision on the same candidate are the
  calibration pair the research doc's §4 protocol needs, and the threshold
  can be moved without a code change (below).

## The turn-continuation check

`docs/agent-tools-design.md`'s 2026-10-20 note leaves the early hand-back
to the loop: an instruction the conversation already carries is not made
effective by repeating it in the system prompt. The check is the
harness-side mechanism that note points at, in the shape of Gemini CLI's
next-speaker check (`packages/core/src/utils/nextSpeakerChecker.ts`, read
2026-10-21): after a reply with no tool calls, a classifier decides whether
the model or the user speaks next.

- **Where.** In `run_turn`, after truncation recovery and before the
  memory checkpoint, on an outcome that would otherwise end `Completed`
  (no tool call outstanding, not cancelled, not failed) when the session
  has a decision connection, its role is not an exploration role, no stop
  or queued input is pending, and the turn's final assistant text is not
  empty. An exploration child's end of turn is consumed by its parent, so
  it is left alone.
- **Question.** `next_speaker`, a `choice` between `user` and `model`
  (a tie cannot continue: the threshold below is far above one half). The
  instructions restate the three rules: `model` when the
  reply states an immediate next action the assistant itself will take
  (including a tool call it describes but did not make) or is clearly
  incomplete against the request; `user` when the reply ends with a direct
  question or needs a decision, access, or information only the user can
  give, or reports the request complete; `user` when in doubt.
- **State.** `user_request` (the message that started the turn),
  `assistant_reply` (the turn's final text), `tool_calls_in_turn`,
  `harness_continuations_so_far`.
- **Outcome.** The turn continues when the choice is `model` and its
  probability is at or above the continuation threshold (initially 0.8):
  the loop commits a `MessageRole::AutoContinue` message telling the model
  its reply announced a step it did not take and to continue until the
  request is complete or something only the user can give is needed, then
  runs the next round as a `Continuation` input, the shape truncation
  recovery already uses. No `TurnEnded` is emitted in between. At most
  three continuations per turn; after that, and on any client error or
  timeout (10 seconds), the turn ends as the model ended it.
- **Audit.** Every check writes a `continuation_verdict` record on the
  same carrier as `judge_verdict`: the probabilities, the threshold, the
  outcome, the latency, and any fallback reason.

## Measurement switches

Two environment-only values, read at session start and clamped to
`0..=1`, exist for moving the thresholds against the audit records without
a rebuild (listed in AGENTS.md beside the clearing threshold):

- `HORIZON_AGENT_JUDGE_ASK_THRESHOLD` — auto-approve when the judge's
  `noul` is at or below it; default `0.2`.
- `HORIZON_AGENT_CONTINUATION_THRESHOLD` — continue when the `model`
  probability is at or above it; default `0.8`.

## Scope

Images and `score` questions are unused. The decision model is not a
router: model selection stays with the person and the model picker. The
standing-agent memory checkpoint keeps its mechanical form
(`docs/standing-agent-memory-design.md`).
