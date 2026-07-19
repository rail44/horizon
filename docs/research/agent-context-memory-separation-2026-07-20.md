# Instruction vs. Tool-Output Context Separation — Agent Memory Architecture Survey (2026-07-20)

Research pass triggered by a dispatched-agent incident: a worker session's
first turn read 19 files (~99,000 tokens, 1.65x the configured history
budget) and, in doing so, evicted its own task instructions from the
context window it sent to the provider, leaving only approval/sandbox
source code — which it then misread as an implicit request to probe
approval bypasses, and never executed the actual task. This report is
input for an owner consultation, not a decision: it surveys how mature
coding agents, memory-architecture literature, and empirical
long-context research treat the distinction between (1) durable intent
(task instructions, small, should survive the whole session) and (2)
disposable working context (tool outputs, large, mostly re-fetchable,
transient) — and inventories what `rig-memory` 0.39 already offers toward
separating them. No implementation is proposed or made.

Companion docs: `docs/research/letta.md` (MemGPT/Letta memory-hierarchy
survey, 2026-07-06, referenced throughout rather than re-derived) and
`docs/research/agent-prompting.md` (system-prompt structure survey,
2026-07-07, which this report's Horizon code-walk supersedes in one
place — see the note in the "current architecture" section).

## Context: what Horizon does today, exactly

**The eviction policy.** `crates/horizon-agent/src/providers/rig/completion.rs`
builds the per-turn provider request with `history_token_window_policy`
(returns `rig_memory::TokenWindowMemory::new(config.history_token_budget,
HeuristicTokenCounter::openai())`) applied by `windowed_history_for_request`,
which clones the session's history and calls `MemoryPolicy::apply` on it
(lines 313-345). `TokenWindowMemory`'s algorithm (`rig-memory` 0.39,
`~/.cargo/registry/.../rig-memory-0.39.0/src/lib.rs:457-490`) walks
messages **newest-to-oldest**, accumulates a per-message token estimate
from `HeuristicTokenCounter` (OpenAI preset: `bytes_per_token=4.0`,
`per_message_overhead=4`, `per_attachment_tokens=256` — a byte-length
heuristic, not a real tokenizer, "can under- or over-count by up to ~30%"
per its own doc comment), and **stops at the first message from the end
that would exceed the budget** — a hard chronological cutoff. Everything
older is dropped outright: no summarization, no demotion hook, no
distinction by message role or content type. `config.rs:157` fixes the
budget at `DEFAULT_HISTORY_TOKEN_BUDGET = 60_000`, chosen "conservative
rather than tight," with the config comment explicitly noting the current
provider is "an OpenAI-compatible endpoint fronting Kimi" (K2, per
`agent-prompting.md` Part 1.3).

**What is exempt from the cutoff, and why.** The system prompt
(`prompt::system_prompt`, `prompt.rs:76-114`: identity, environment block,
tool policy, destructive-action list) plus `extra_sections` — the
composed "Repository instructions (AGENTS.md)" block built by
`instructions::extra_sections` (capped at
`DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS = 24_000` chars,
`instructions.rs:39-76`) and any role/skill prompt sections
(`session::session_extra_sections`) — are passed via
`.preamble(system_prompt(environment, extra_sections))` on rig-core's
`completion_request` builder (`completion.rs:157`). `.preamble()` is a
structurally separate channel from `.messages(history)`: `TokenWindowMemory`
never sees it, so AGENTS.md/CLAUDE.md content, role text, and skill
sections are architecturally permanent for the session's lifetime. This
confirms the motivating claim precisely, and corrects one adjacent
under-specification in `agent-prompting.md` Part 2.1, which described
`system_prompt` as taking only `environment` at the time it was written
— the `extra_sections` parameter (and the whole instructions/roles/skills
composition into it) was added later and is exactly the mechanism that
keeps AGENTS.md pinned.

**What is *not* exempt: the task instruction itself.** The user's (or a
dispatching caller's) task prompt is not part of the preamble at all — it
enters through the ordinary turn loop like anything else.
`providers/rig/session.rs`'s `Command::UserMessage { text }` arm
(lines 215-247) wraps it as a plain `Message::user(text.clone())` and
passes it as the `prompt` argument to `run_cancellable_turn` /
`complete_rig_turn`. Inside `complete_rig_turn` (`completion.rs:93-96`),
once the turn resolves: `rig_history.push(prompt); rig_history.push(assistant_message);`
— the task instruction becomes an ordinary entry in `rig_history: Vec<Message>`,
indistinguishable at the type level from the tool-result messages that
follow it (`Command::ToolCallResult` builds `rig_tool_result_message(&result)`
and feeds it through the exact same `prompt` parameter and the exact same
push, `session.rs:302-345`). `rig_core::completion::Message::User` can
hold either `UserContent::Text` (real user text) or `UserContent::ToolResult`
(a tool's output fed back) — both are the same enum variant, and neither
`HeuristicTokenCounter::count` nor `TokenWindowMemory::apply_with_demoted`
inspects which one it is; the walk is purely positional (index from the
end) and purely quantitative (byte-derived token cost). **There is
currently no code path that treats "the instruction that started this
session" as different in kind from "the 4KB a tool happened to return."**
A turn that requests enough large tool reads (`fs.read` is line-windowed
per call — default 2000 lines, `tools/catalog.rs:29-56` — but nothing
bounds the *cumulative* size of many reads within one session) can push
the original task prompt past the 60,000-token horizon while itself
having contributed nothing that survives structurally, which is exactly
what the reported incident did: 19 full-file reads at turn 1 (~99,000
estimated tokens against the 60,000 budget) evicted the task instruction
and any design-doc content that had been *read via a tool* (as opposed to
composed into `extra_sections`), leaving only the most recent tool
outputs — approval/sandbox source — as the entire visible context for
the next completion request.

**What is deliberately never truncated.** `rig_history` itself, and the
DuckDB-projected event log it is rebuilt from at session start
(`load_rig_history`), are never touched by windowing — only the cloned
*view* handed to the provider each turn is (`config.rs:263-266`'s comment
is explicit about this). This is a real asset: nothing is destroyed, so
any fix that changes what gets *sent* to the provider does not need to
touch persistence, and a demotion/compaction scheme can always fall back
to re-deriving from the full log.

**The central design question**, restated: two kinds of context —
(1) durable intent (small, should live for the session's duration) and
(2) bulky, mostly re-fetchable working material (tool outputs) — currently
share one undifferentiated, purely-recency-ordered eviction path once
either has entered `rig_history`. The system-prompt/preamble split already
gives Horizon *one* durable tier, but the task instruction that starts a
session (and any tool-sourced material a session needs to keep) does not
get into it.

## Comparative table: how mature agents treat tool output vs. instructions

All entries below are dated to the sources fetched 2026-07-20; several of
these systems (opencode, crush especially) are mid-refactor and the
numbers should be expected to drift. See the confidence-notes section for
per-row caveats.

| Agent | Tool-result eviction | Where instructions live | Summarization? | Survives compaction/eviction? | Source |
|---|---|---|---|---|---|
| **Claude Code** | No dedicated tool-result-only mechanism observed in official docs; whole conversation (including tool outputs) is summarized by `/compact` (auto at ~95% capacity or manual) | System prompt (never in message history); CLAUDE.md delivered as **a user message immediately after the system prompt**, not part of it | Yes — LLM-generated structured summary ("preserves architectural decisions, unresolved bugs, implementation details... discarding redundant tool outputs") | System prompt: unchanged (not history). Project-root CLAUDE.md + unscoped rules + auto-memory: **re-injected from disk** after `/compact`. Path-scoped rules / nested CLAUDE.md: lost until next matching file read. Skill bodies: re-injected, capped 5,000 tok/skill, 25,000 tok total, oldest dropped first | [code.claude.com/docs/en/memory](https://code.claude.com/docs/en/memory), [code.claude.com/docs/en/context-window](https://code.claude.com/docs/en/context-window) (official) |
| **Anthropic Messages API** (`clear_tool_uses_20250919`, provider-level primitive, not a specific agent) | Yes, dedicated: clears **oldest tool results first**, server-side, at a configurable input-token trigger; keeps a configurable minimum of recent tool uses (`keep`); can `exclude_tools`; replaces cleared content with placeholder text; `clear_tool_inputs` optionally also drops the call parameters, not just results | N/A (a raw completion primitive, not a full agent) | No — this is surgical clearing, explicitly contrasted with "holistic" summarization in the same docs | Cleared content does not "survive"; distinguishes itself from compaction precisely because it doesn't try to preserve meaning, only frees tokens | [platform.claude.com/docs/en/build-with-claude/context-editing](https://platform.claude.com/docs/en/build-with-claude/context-editing) (official, beta) |
| **OpenAI Codex CLI** | No tool-output-specific clearing found; auto-compaction near the model's effective context ceiling (~95% in one source, a ≤90%-of-window configurable cap in another — figures disagree, see confidence notes) first tries a **structured-state substitute** for a full LLM summary call (task state / file-edit history / key decisions already tracked), falling back to an LLM summary | AGENTS.md hierarchy is walked and concatenated at startup (official, `developers.openai.com/codex/guides/agents-md`), lower directories override higher ones | Yes, when the structured-state shortcut isn't available | Summary + up to ~20,000 tokens of the most recent **user** messages are kept; everything else discarded | Community reverse-engineering (gist + 2 independent blogs, cross-agree); official but generic: [developers.openai.com/api/docs/guides/compaction](https://developers.openai.com/api/docs/guides/compaction) (Responses API primitive, not CLI-specific) |
| **OpenCode (sst/opencode, "OpenCode")** | **V1** reportedly pruned old tool outputs specifically (protect most-recent 40k tokens, prune only if ≥20k tokens of older tool output can be freed, skill-type outputs exempt) — **V2's own docs explicitly say this in-place pruning is no longer implemented** ("V1-style in-place tool-output pruning is not implemented in V2"); V2 instead caps tool output at 2,000 chars in checkpoints and relies on whole-conversation compaction | Not investigated in this pass beyond the 2026-07-07 tools survey (`docs/research/crush-opencode-tools-2026-07-07.md`), which didn't cover instruction placement | Yes — LLM summary on overflow (`estimated tokens > context_limit - buffer`) | `keep.tokens` (default 8,000) retained verbatim from recent context alongside the summary; older messages are not deleted, just fall outside the active window | Official V2 docs: [v2.opencode.ai/compaction](https://v2.opencode.ai/compaction); V1 behavior via DeepWiki + GitHub issue (secondary) |
| **charmbracelet/crush** | A described (GitHub-issue-level, not yet confirmed as an official doc page) "compaction" pass uses a small/fast model to **delete low-signal lines verbatim** rather than rewrite/summarize them — explicitly preserves exact file paths, error messages, line numbers that a paraphrase would lose | Not investigated | Both exist per GitHub issues: a **deletion-only** compaction (no rewriting) and a separate **auto-summarization** ("Build → Compact") that replaces early history with a narrative summary — the latter has an open bug where the generated summary is itself phrased as an actionable task, causing an infinite Build→Compact loop | Summary message's role is rewritten from assistant to user so it anchors the next window | GitHub issues + DeepWiki (secondary, medium-low confidence; no official Crush doc page found) |
| **Cline** | Yes, tool-output-specific: **deduplicates repeated reads of the same file**, replacing redundant re-reads with a short notice instead of the full content again | `.clinerules` (official, `docs.cline.bot`); task instruction is one line item among "Task / environment details / tool defs / system prompt / history" that together fill the window | Truncation, not LLM summarization: removes whole **message pairs** (user+assistant) once size exceeds `maxAllowedSize = max(contextWindow - 40_000, contextWindow * 0.8)`; ~50% removed in the ordinary case, ~75% when switching to a much smaller-context model | Officially documented performance guidance: models "start showing some performance degradation... around 70-80%" full — Cline's design target is staying under that, not preserving specific content | Official blog: [cline.bot/blog/clines-context-window-explained](https://cline.bot/blog/clines-context-window-explained-maximize-performance-minimize-cost); truncation formula + file-dedup mechanism via DeepWiki (semi-primary, generated from source, medium confidence on exact numbers) |
| **aider** | No tool-output-specific mechanism (aider's tools are thinner — mostly file edits, not free-form reads); repo map is a separate, always-regenerated context artifact (default 1k-token budget), not part of chat history at all | `CONVENTIONS.md` is **opt-in only** (`/read` or explicit config) — the one surveyed agent that does *not* auto-concatenate a repo instruction file | Yes — chat history has a configurable **soft token limit** (`--max-chat-history-tokens`), past which a separate "weak model" summarizes it | Not documented beyond "summarization begins" past the soft limit; `/history` lets a user manually check/uncheck which history entries are kept | [aider.chat/docs/config/options.html](https://aider.chat/docs/config/options.html), [aider.chat/docs/usage/conventions.html](https://aider.chat/docs/usage/conventions.html) (official) |
| **Cursor** | Not documented officially; third-party sources describe conversation summarization "to preserve key decisions" and large referenced files being summarized separately, but no mechanism-level detail | Proprietary `.cursorrules`/rules files; no official architecture doc found | Reportedly yes ("routinely summarizes your previous conversation"), mechanism undocumented | Unknown — no official source found | Third-party blogs only (low confidence); official docs page ([cursor.com/learn/context](https://cursor.com/learn/context)) covers user-facing guidance, not internals |
| **Windsurf (Cascade)** | Not documented; no official mechanism-level source found for tool-output handling specifically | **Memories** (auto-generated by Cascade, stored locally per-workspace, `~/.codeium/windsurf/memories/`) + **Rules** (user-authored, global/workspace/system) are both explicitly persistent, reloaded every interaction, separate from message history | Unknown/undocumented for raw conversation truncation | Memories/Rules are, by design, outside the conversation entirely (loaded fresh each turn from disk) — so they trivially "survive" any conversation-level truncation, but this says nothing about how Cascade handles a long single conversation's own history | [docs.windsurf.com/windsurf/cascade/memories](https://docs.windsurf.com/windsurf/cascade/memories) (official, but scoped to Memories/Rules, not compaction internals) |

**Cross-cutting read.** Every agent surveyed keeps *some* instruction
channel structurally outside the eviction path (system prompt, CLAUDE.md
re-injection, AGENTS.md hierarchy re-read, `.clinerules`, Memories/Rules).
None of the mechanism-level sources describe an agent that lets its own
*task* instruction get silently evicted the way Horizon's current code
allows — the closest analog is Claude Code's own explicit warning ("If an
instruction disappeared after compaction, it was either given only in
conversation... Add conversation-only instructions to CLAUDE.md to make
them persist"), which is itself an admission that conversation-only
content, including a plain task instruction, is exactly as vulnerable in
Claude Code as it is in Horizon — the difference is that Claude Code's
docs surface this as a known, documented failure mode with a prescribed
workaround, where Horizon currently has neither.

## Pattern catalog

Each pattern: mechanism, when it applies, its tradeoffs, and a fit
assessment for Horizon specifically.

**(a) Tool-result clearing, chronological / surgical** (Anthropic API
`clear_tool_uses_20250919`; conceptually close to OpenCode V1's reported
pruning and Cline's file-read dedup). *Mechanism*: identify tool-result
messages specifically (not just "old messages") and remove/replace only
those, oldest first, once a threshold is crossed; optionally keep the
tool-*call* (so the model still knows it looked something up) while
dropping only the bulky result. *Applies when*: the dominant context bloat
source is tool output (exactly Horizon's `fs.read`/`fs.grep` case) and the
outputs are cheaply re-fetchable if needed again. *Tradeoffs*: invalidates
prompt-cache prefixes at the clear point (Anthropic's own docs flag this
and provide `clear_at_least` to avoid clearing too little to be worth the
cache-write cost — not directly applicable to Horizon's OpenAI-compatible
provider, which this codebase does not currently exploit prompt caching
for, but the general principle — don't evict in small, cache-hostile
increments — still applies to any future caching work); loses the actual
content, so a later turn that genuinely needs the old file content must
re-read it (acceptable if files rarely change mid-session; not acceptable
for content that isn't re-derivable, e.g. a tool result summarizing an
external API call with no stable replay path). *Fit for Horizon*: high —
this is the single most direct precedent for "many `fs.read` calls flood
the window," and rig-memory's `TokenWindowMemory::apply_with_demoted`
already gives Horizon the "which messages got cut" list needed to build
this without adopting the full `ConversationMemory` wrapper stack (see
next section).

**(b) Whole-conversation summarization / compaction** (Claude Code
`/compact`, Codex CLI's fallback path, aider's chat-history
summarization, `rig-memory`'s `CompactingMemory`/`TemplateCompactor`).
*Mechanism*: once near a limit, run an LLM (or, per Codex's reported
fast path, structured state already tracked outside the LLM) over the
evicted prefix and splice a narrative summary back in place of the
verbatim messages. *Applies when*: the conversation genuinely needs long-
range narrative coherence across turns that can't be captured by keeping
recent messages alone (architectural decisions made 40 turns ago, a bug's
history). *Tradeoffs*: this is exactly the failure mode
`docs/research/letta.md` documents most strongly — MemGPT's own DMR
experiment found recursive summarization (35.3%) far behind
search/retrieval (93.4%), and Letta's own self-reported failure mode is
"memories become generic and lossy after repeated refinements." A
summary is also a single point of failure: if it drops something subtly
important, that loss is invisible until much later. `CompactingMemory`'s
own doc comment additionally warns the spliced summary sits **outside**
the wrapped policy's token budget, so a compaction path needs its own
cap or the prompt can silently exceed `history_token_budget`.
*Fit for Horizon*: medium — useful for genuinely long sessions, but per
Letta's Terminal-Bench agent (`letta.md` §17) the industry's own
practical answer is to combine this *with* a recall/search mechanism,
not use it alone; Horizon has no such recall tool over its DuckDB
projection yet (`letta.md`'s top finding #1).

**(c) Structured note-taking / agentic memory outside the window**
(Anthropic's memory tool + "structured note-taking" pattern, Claude
Code's own `MEMORY.md`, Letta's core-memory blocks). *Mechanism*: the
agent (or the harness) periodically writes small, curated notes to a
file/store that lives outside the context window entirely and gets
reloaded (often in full, at a hard size cap — Claude Code's `MEMORY.md`
is capped at "first 200 lines or 25KB") at the start of every
conversation, regardless of what happened to the rest of history.
*Applies when*: there's a small set of facts (build commands, prior
mistakes, durable preferences) worth carrying across the *whole*
session or across sessions, distinct from "what just happened in this
turn." *Tradeoffs*: someone (agent or human) has to curate it, and a bad
write can pollute every future session; Claude Code's own design
mitigates this by forcing a hard size ceiling and nudging the model to
keep the index concise rather than letting it grow unboundedly.
*Fit for Horizon*: high as a *complementary* mechanism to (a)/(d) — this
is a different axis (across-session persistence) from the immediate
problem (within-session eviction), but the *pattern* of "small, disk-
backed, always-reloaded-in-full, hard-capped" is exactly the shape a
"pin the task instruction" mechanism (option 1 below) would need.

**(d) Sub-agent / spatial context isolation** (Anthropic's own
recommended architecture; Claude Code's `Task`/subagent tool; crush's
`agentic_fetch`; Letta's Context Repositories worktree pattern,
`letta.md` §8). *Mechanism*: instead of evicting *after* the fact,
prevent bulk exploration from ever entering the parent's context at all
— delegate it to a child session/turn with its own disposable context,
and only let a condensed, deliberately-written summary cross back.
Anthropic's own framing: "Each subagent might explore extensively, using
tens of thousands of tokens or more, but returns only a condensed,
distilled summary." *Applies when*: a task is expected to require heavy,
disposable exploration (many file reads, broad greps) before useful work
begins — exactly the incident's shape (19 files read at turn 1).
*Tradeoffs*: architecturally heavier than (a)-(c) — it's a delegation-
model change, not a memory-policy change; the parent never sees the raw
material at all, so if the summary omits something the parent later
needs, the cost is a full re-delegation, not a cheap re-read. *Fit for
Horizon*: this is the pattern that would have prevented the reported
incident **structurally** rather than mitigating its symptom (a worker-
style dispatched agent doing broad exploration is exactly a sub-agent
use case), but it is out of scope for a `rig-memory`-level fix — it's a
session/delegation-architecture question, closer to the "委譲" thread
already open in `letta.md` §(c) than to this report's immediate
`TokenWindowMemory` question.

**(e) Two-tier "mission + scratch" blocks** (Letta's own Terminal-Bench
agent, `letta.md` §17: a read-only task-description block plus a
separate, agent-editable todo block). *Mechanism*: reserve one small,
structurally protected slot for "what am I supposed to be doing" (fixed
for the task's duration) and a second small, mutable slot for "what have
I figured out so far / what's left" that the agent itself keeps current
— both are cheap (small, fixed-size) and both are explicitly exempted
from whatever compaction/eviction touches the rest of history. *Applies
when*: sessions are long enough that "what was I asked to do" and
"what's my current plan" need to outlive many rounds of bulky tool
output in between. *Tradeoffs*: needs a place to put the mutable slot
(a todo/plan artifact) that Horizon doesn't yet have as a first-class
concept — this is closest to (c) but scoped tighter (task-local, not
cross-session). *Fit for Horizon*: high conceptually, and cheap: this is
essentially option 1 below (pin the instruction into `extra_sections`)
plus a second, currently-nonexistent "current plan" slot the agent
itself would need a tool to update.

**(f) LangChain/LlamaIndex-style pluggable memory blocks** (LlamaIndex's
`Memory` class: `StaticMemoryBlock` — always inserted verbatim, no
eviction, e.g. persona/instructions; `FactExtractionMemoryBlock` — an
LLM extracts durable facts from flushed-out history, capped at
`max_facts`, auto-summarized further if that cap is exceeded;
`VectorMemoryBlock` — flushed message batches go into a vector store for
later semantic recall; all three are fed by the same flush event once
short-term history exceeds a `chat_history_token_ratio`. LangChain's
older `ConversationBufferWindowMemory` (last-K messages) vs.
`ConversationSummaryMemory` (LLM summary) vs.
`ConversationSummaryBufferMemory` (hybrid: verbatim recent + summarized
older) is the same design space one generation earlier). *Mechanism*:
model memory as several independently-configured "blocks," each of which
decides for itself what to do with content flushed out of the primary
window — some blocks never evict (static), some summarize, some archive
for later retrieval. *Applies when*: different classes of evicted content
genuinely want different treatment (a fact worth keeping forever vs. a
tool result worth archiving for possible re-retrieval vs. nothing worth
keeping at all). *Tradeoffs*: more moving parts than a single policy;
requires classifying content into blocks, which is itself a design
decision. *Fit for Horizon*: this is the closest external precedent to
"treat instructions and tool output differently" as a *general
framework* rather than a point fix — `rig-memory`'s own `DemotionHook`/
`Compactor` traits are structurally analogous to LlamaIndex's block
callback (see next section), so adopting this framework's *shape*
doesn't require adopting LlamaIndex itself.

**(g) Position-based instruction placement** (informed by, not a direct
recommendation of, the "lost in the middle" line of work). *Mechanism*:
independent of any eviction policy, place the most important instructions
at the very start and/or very end of whatever context survives, since
mid-context positions are where models are measurably weakest at
retrieval — this is about *where in the surviving window* something
sits, not *whether* it survives. *Applies when*: an instruction can't be
moved to a structurally-exempt channel (preamble, memory file) for some
reason and must remain in the message stream. *Tradeoffs*: doesn't
prevent outright eviction (a hard token-budget cutoff still deletes
things, regardless of position, once they fall off the counted-from-the-
end window) — it only helps for content that survives the cutoff but
would otherwise sit in a weak position. *Fit for Horizon*: low priority
relative to (a)/(c): Horizon's core problem is outright deletion, which
this pattern doesn't address; it's a secondary refinement once messages
are known to survive.

## rig-memory 0.39 toolkit: concrete options for Horizon

Read directly from `~/.cargo/registry/.../rig-memory-0.39.0/src/lib.rs`
and `rig-core-0.39.0/src/memory.rs`. Four building blocks matter here:

- `MemoryPolicy` trait: `apply(Vec<Message>) -> Result<Vec<Message>>`
  (required) plus `apply_with_demoted(Vec<Message>) -> Result<(kept,
  demoted)>` (default: `(apply(..)?, vec![])`; `TokenWindowMemory` and
  `SlidingWindowMemory` both override it to actually populate `demoted`).
  This is what Horizon already calls today via plain `apply`.
- `DemotionHook` trait (`rig_core::memory`) + `DemotingPolicyMemory<M, P,
  H>` (rig-memory): a one-way drain — whatever a policy demotes gets
  handed to a hook (`on_demote(conversation_id, messages) -> Result<()>`)
  for archival/logging, with in-process delivery-watermark tracking so
  the same demoted slice isn't redelivered on every subsequent load.
- `Compactor` trait + `CompactingMemory<M, P, C>`: the two-way version —
  a compactor turns the demoted prefix (plus the previous summary, for
  recursive rollups) into an `Artifact: Into<Message>` that gets spliced
  back at the front of the kept window. `TemplateCompactor` is the
  zero-dependency reference implementation: a plain `"role: text"` line
  per message, tool results collapsed to the literal string `"[tool
  result]"` — informationally weak (loses path/content entirely) but
  useful as a scaffold or test double.
- Both `DemotingPolicyMemory` and `CompactingMemory` are
  **`ConversationMemory` adapters** (they implement `load`/`append`/
  `clear`), not bare `MemoryPolicy` wrappers — a structural mismatch with
  how Horizon currently uses this crate (see option 2 below).

**Option 1 — pin the task instruction into the always-present tier.**
Thread the session's originating task text through `session_extra_sections`
(`providers/rig/session.rs`) as an additional `extra_sections` entry,
mirroring how AGENTS.md content and role/skill text already get into
`system_prompt`'s preamble — i.e., give it the same structural immunity
`.preamble()` already grants AGENTS.md, rather than only sending it once
through `rig_history` as an ordinary `Message::User`. Code seam:
`prompt::system_prompt(environment, extra_sections)` already accepts an
arbitrary `Vec<String>`; the assembly point is `session_extra_sections`,
called once per session start. *Open design question this doesn't answer
by itself*: Horizon sessions are long-lived and can receive many
follow-up `Command::UserMessage`s well after the first — the "instruction"
a long session is executing may not be a single fixed string from turn 1,
so this option needs a product decision about what "the" pinned
instruction is in a session with multiple top-level asks (the initial
one only? the most recent one? all of them, capped like
`repository_instructions_cap_chars`?). *Tradeoff*: preamble content is
already exempt from `history_token_budget` but is otherwise uncapped in
this codebase apart from the repository-instructions section's own
24,000-char limit — pinning free-form task text needs an analogous cap
so a very long dispatched task prompt doesn't itself blow out headroom
for tool responses.

**Option 2 — use `TokenWindowMemory::apply_with_demoted` directly,
without adopting the `ConversationMemory` wrapper stack.**
`windowed_history_for_request` currently calls the `MemoryPolicy::apply`
one-return-value method; `apply_with_demoted` is already implemented on
`TokenWindowMemory` and costs nothing extra to call instead — it returns
`(kept, demoted)`. This unlocks (a)-style tool-result-specific handling
*without* restructuring `rig_history` into a `ConversationMemory` backend
(which `DemotingPolicyMemory`/`CompactingMemory` both require, and which
Horizon does not currently have — `rig_history` is an in-process `Vec`
rebuilt once at session start from the DuckDB projection via
`load_rig_history`, never routed through a `ConversationMemory::load`
call per turn). With the `demoted` list in hand, `windowed_history_for_request`
(or a new function beside it) could inspect which demoted messages are
tool results (`Message::User` whose content is `UserContent::ToolResult`)
and, e.g., log them, or replace them in the *kept* window's leading edge
with a short reference (tool id, a truncated preview) rather than letting
them vanish with no trace — closer to Anthropic's placeholder-text
behavior than to a silent hard drop. *Tradeoff*: still coarse-grained —
this only sees what `TokenWindowMemory`'s own newest-first walk already
decided to cut, not a targeted "cut tool results before cutting anything
else" priority; achieving that would need a custom `MemoryPolicy`
(straightforward — it's a two-method trait) that partitions messages by
kind before applying the token budget, rather than reusing
`TokenWindowMemory` as-is.

**Option 3 — `CompactingMemory` + a bespoke `Compactor` (not
`TemplateCompactor`).** If narrative continuity across a long session is
wanted in addition to (or instead of) surgical clearing, a custom
`Compactor` impl (the trait is two methods: an associated `Artifact` type
and an async `compact(conversation_id, evicted, carry_over) -> Artifact`)
could treat evicted tool-result messages specially — e.g. render each as
"read `{path}` lines `{offset}-{offset+limit}`" rather than
`TemplateCompactor`'s generic `"[tool result]"`, preserving the
*reference* even though the content is gone (a resurfaced version of
pattern (a)'s placeholder-text idea, done via the summarization channel
rather than the clearing channel). This still requires adopting
`CompactingMemory`'s `ConversationMemory`-adapter shape (same structural
cost noted in option 2), and inherits `CompactingMemory`'s own documented
caveat that the spliced summary sits outside the wrapped policy's token
budget — needs its own size cap, and inherits the general "generic and
lossy" risk `letta.md` §14 documents for any repeated-summarization
design unless the compactor is deliberately kept information-preserving
(paths/line-ranges, not paraphrase) the way pattern (a)/crush's reported
verbatim-deletion approach are.

**Option 4 — delegate exploration-heavy work to an isolated sub-session**
(pattern (d), restated as a Horizon-specific option). Rather than
changing what `TokenWindowMemory` does with an overflowing `rig_history`,
prevent the overflow at its source for sessions expected to do broad
reconnaissance (a dispatched worker given an open-ended "read the
relevant code and do X" task, exactly the incident's shape): route the
exploratory reads through a disposable child turn/session whose reads
never enter the parent's `rig_history`, with only a written summary
crossing back — Claude Code's subagent architecture and crush's
`agentic_fetch` are the closest working precedents. This is the most
invasive of the four (a delegation/session-architecture change, not a
`rig-memory` policy swap) and is flagged here only as the option that
addresses the incident's root cause structurally; it is not a `rig-memory`
toolkit option and would need to be scoped against Horizon's existing
delegation design questions separately.

None of these four are mutually exclusive, and none is proposed as a
decision here — they sit at different layers (1 and 4 are architectural;
2 and 3 are drop-in policy changes) and could combine (e.g. 1 for the
instruction, 2 for tool-result handling, 4 reserved for sessions
explicitly flagged as exploration-heavy).

## What lost-in-the-middle and context-rot research imply for instruction placement

**"Lost in the Middle" (Liu et al., arXiv:2307.03172).** Multi-document
QA and key-value retrieval tasks both show a U-shaped performance curve:
"performance is often highest when relevant information occurs at the
beginning or end of the input context, and significantly degrades when
models must access relevant information in the middle" — true even for
models built for long contexts. This is a *positional* effect: content
that survives eviction can still be functionally invisible if it sits in
the wrong place. (Read via the arXiv abstract page only in this pass —
the exact quantitative tables from the full paper were not pulled; treat
the qualitative U-shape as high confidence, any specific percentage as
unverified here.)

**Context rot (Chroma's 2025-26 research report, `trychroma.com/research/context-rot`,
18 frontier models tested, plus the independent arXiv:2606.29718
follow-up on long-horizon agentic search).** The key reframing beyond
"lost in the middle": degradation is not an overflow phenomenon
(exceeding the nominal context limit) but starts well before it —
"accuracy drops non-uniformly as input length grows, sometimes by 30 to
50 percent well before the documented limit," and "what matters more is
how that information is presented," not raw length alone: semantic
similarity between the relevant content and surrounding "distractor"
content predicts collapse better than token count does, and models
diverge in *how* they fail (Chroma reports Claude models tending to
abstain when uncertain, GPT models tending to hallucinate instead). The
2026 follow-up paper found the failure mode in long-horizon *agentic
search* specifically manifests as models "directly giv[ing] up or
prematurely provid[ing] uncertain answers" as accumulated context grows,
and evaluated seven context-management strategies as mitigations —
i.e., the same class of techniques cataloged above (clearing,
compaction, sub-agents) is the literature's own answer to this finding,
not just an engineering convenience.

**Combined implication for Horizon.** These two bodies of work point the
same direction but at different failure mechanisms: "lost in the middle"
says *position within a surviving window* matters; context rot says
*even nominally-in-scope* context degrades the model's reliability well
short of any hard limit, especially when it's cluttered with
semantically-similar distractors (exactly what a pile of full-file reads
is, relative to a task instruction). Both are strictly less severe than
Horizon's current failure mode — `TokenWindowMemory`'s hard chronological
cutoff doesn't just degrade the instruction's usefulness, it can delete
it from the request entirely once enough newer content has accumulated.
Fixing the outright-deletion problem (this report's options 1/2/4) is a
precondition for any position-based refinement (pattern (g)) to matter at
all — there is no position that helps content the provider never
receives.

## Axis A: the budget is a fixed constant, not derived from the model's window

The survey above is about *how to partition and evict within a budget*
(axis B). A separate axis surfaced in the same consultation: *how large
the budget should be*. `DEFAULT_HISTORY_TOKEN_BUDGET = 60_000`
(`config.rs:157`) is a Horizon-side hardcoded constant applied to every
model regardless of that model's actual context window, and the
2026-07-18 config-narrowing wave removed it from the config-file surface
entirely (it is no longer even tunable without a code change). Its doc
comment says it was "chosen with Kimi in mind" — but as a flat number,
not derived from Kimi's window.

The operative number: the incident's model, `hf:moonshotai/Kimi-K2.7-Code`,
is served by synthetic.new (the current provider) with a **256,000-token
context window** (dev.synthetic.new/docs/api/models, fetched 2026-07-20;
it also backs the `syn:large:vision` alias). Horizon budgets 60,000 of
that 256,000 for history — roughly **23% utilization**, hard-dropping the
rest. The incident's ~99,000 tokens of turn-1 reads would have fit ~2.5x
over inside 256,000; **a model-derived budget alone would have prevented
the eviction**, with no partitioning change at all. Axis A is therefore
independently valuable and low-risk, but it does not *replace* axis B:
widening the window only defers the overflow, and context-rot research
(above) shows reliability degrades well before any hard limit anyway, so
a bigger budget without tool-result-aware eviction can trade "task
deleted" for "task buried among 200k tokens of distractors."

*Sub-question axis A raises*: for the budget to track the model, Horizon
must learn each model's served window (256k for this one). That value
is not currently anywhere in the config or provider handshake — options
are a static per-model lookup table, a `[provider]` config field, or a
provider capability query — and it varies per session, since e.g. the
judge model (`syn:small:text` = GLM-4.7-Flash) has a different window
than the agent model. Left for the owner consultation. Also unchanged by
axis A: the heuristic counter is byte-based (±30%), and real headroom is
needed on top of history for the preamble and the turn's own generated
output — so a model-derived budget is `served_window - preamble -
max_output - safety_margin`, not the whole window.

## Deep-dive + decision (2026-07-20)

A source-level follow-up on opencode (`anomalyco/opencode` @ `dev`,
`67caf89`) and crush (`charmbracelet/crush` @ `main`, `d162615`), plus a
live check of the provider path, settled both axes.

### Corrections to the survey above

- **opencode has TWO compaction implementations.** A wired production path
  (`packages/opencode/src/session/{compaction,overflow}.ts`, `SessionV1`)
  and an unwired "V2" native runner (`packages/core/src/session/`, reachable
  only from `runner/llm.ts`, which nothing in the shipped CLI imports). So
  the earlier "V1 pruning removed in V2" reading (from a GitHub issue /
  DeepWiki) is inaccurate: the production path STILL has tool-output pruning
  (opt-in, default off); the next-gen path simply hasn't implemented it yet
  ("Deterministic old tool-result pruning remains a separate follow-up",
  `specs/v2/session.md`).
- The "crush deletes low-signal lines verbatim via a small model" claim
  (also GitHub-issue-sourced) was NOT found at current `main` — crush does
  whole-conversation summarization only. Medium-confidence grep-based
  negative.

### How mature agents derive the compaction threshold from the model

Both fetch the model's context window from an external model-metadata
catalog, via an identical three-tier fallback (network → disk cache →
build-embedded snapshot):

- opencode: models.dev `/api.json` → `Model.limit.{context,input,output}`.
- crush: `catwalk.charm.land` → `catwalk.Model.ContextWindow`/`DefaultMaxTokens`.

Threshold formulas (crush's is the simplest to borrow):

- crush (`internal/agent/agent.go:1027-1048`): `threshold = cw>200_000 ?
  20_000 : cw*0.2`; fire when `remaining (= cw − cumulative usage tokens)
  <= threshold`; **`cw==0` (unknown model) never fires** (protection).
- opencode prod (`overflow.ts:10-33`): `reserved = min(20_000,
  maxOutputTokens)`; overflow when actual usage total `>= usable
  (= input − reserved, or context − maxOutput)`.

Both drive the decision off the provider's **actual usage tokens**, not a
byte heuristic (only opencode's unwired V2 uses a heuristic pre-estimate).

### How they handle old tool results (and the replay-cache question)

- **opencode prod**: opt-in prune (default off) replaces OLD tool-result
  *bodies* with a placeholder (`[Old tool result content cleared]`) while
  KEEPING the tool call (pairing preserved), protecting the recent 2 turns
  + 40k tokens; `skill` outputs exempt. This is almost exactly axis B below.
- **crush**: whole-conversation summarization only (hard cutoff, summary
  role rewritten assistant→user).
- **Neither implements a tool-result REPLAY cache** (store the result,
  re-inject later without re-executing), nor file-read dedup, nor a
  content-addressed result store. crush's `filetracker` is a
  read-before-edit guard, not a context cache. Both DO use provider prompt
  caching (Anthropic `cache_control`) — the LLM-API caching explicitly out
  of scope here. So the owner's replay-cache idea has no prior art in either
  and is dropped for now (its value concentrates in expensive/non-idempotent
  tools; revisit with the web tools, backlog 18, if pursued).
- Pairing: crush repairs orphans every send (`filterOrphanedToolResults` +
  `syntheticToolResultsForOrphanedCalls`); opencode keeps call+result in one
  "tool part" so turn-level trimming can't split them.

### Provider path confirmation (axis A source)

synthetic.new's `GET /openai/v1/models` returns, per model, a
`context_length` (`262144` for `hf:moonshotai/Kimi-K2.7-Code`) AND a
`max_output_length` (`65536`), programmatically — so Horizon can derive the
budget **in-band** from the provider it already talks to, no external
catalog needed for the current provider. Standard OpenAI `/models` does NOT
return these, so the derivation must use them when present and fall back
conservatively when absent.

### Decision (2026-07-20)

Axis A and axis B are implemented together; the replay cache is dropped for
now.

**Axis A — model-derived history budget.** At provider/session start, query
`{base_url}/models` once (cached per process per `(base_url, model)`), and
set `history_token_budget = context_length − max_output_length −
preamble_reserve − safety_margin`. Graceful fallback to the current
conservative constant when the query fails, the model is unlisted, or
`context_length` is absent (provider-agnostic — use the field when present,
degrade otherwise). Borrow crush's `cw==0 → don't derive, keep the fixed
default` protection. Driving the decision off the provider's actual usage
tokens (crush/opencode's approach) is a noted follow-up, not required now;
the byte heuristic stays for the per-message "what to trim" estimate.

**Axis B — tool-result-aware eviction (opencode-prune-shaped).** A custom
`MemoryPolicy` that, on overflow, replaces OLD tool-result message *content*
with a short reference placeholder (keeping the tool call and its `call_id`,
so pairing holds), oldest-first, protecting the most recent tool results;
only if still over budget after all tool results are elided does it fall
back to dropping the oldest whole interactions (rig-memory's existing
orphan handling preserves pairing). The task instruction
(`UserContent::Text`) is never touched by the tool-result pass, so it
survives as a byproduct (the "option 2 protects the instruction" property).
Full results remain in `rig_history` + the DuckDB event log, so a later
re-read re-executes and returns fresh content. Integration point: replace
stock `TokenWindowMemory` in `history_token_window_policy`;
`windowed_history_for_request` unchanged. Dispatched to a worker 2026-07-20.

## Confidence notes — where this report is less sure

- **Cursor**: no official technical documentation of internal context
  management was found; every claim in this report's table row comes
  from third-party 2026 blog posts, none of which cite Cursor engineering
  sources. Low confidence throughout that row.
- **Windsurf**: the Memories/Rules architecture is officially documented
  and high confidence; how Cascade truncates or summarizes a single long
  conversation's own message history is not documented anywhere found in
  this pass — that half of the row is an acknowledged gap, not a
  low-confidence claim.
- **charmbracelet/crush**: sourced from open GitHub issues and a
  DeepWiki page (LLM-generated from source, not an official doc), not a
  confirmed Crush documentation page. This is also a direct discrepancy
  against `docs/research/crush-opencode-tools-2026-07-07.md` (2026-07-06
  HEAD), which surveyed crush's tools and found no compaction/context
  mechanism at all — either that survey's scope simply didn't cover this
  area, or crush added compaction after 2026-07-06; not resolved here.
  Medium-low confidence.
- **OpenAI Codex CLI**: no dedicated official OpenAI blog post detailing
  the CLI's own internal compaction algorithm was found (as distinct from
  the generic, officially-documented `/responses/compact` Responses API
  primitive). The specific numbers in this report's table (auto-compact
  threshold, ~20k tokens of retained user messages, the structured-state
  fast path) come from a community gist and independent blog posts that
  agree with each other but are still third-party reverse-engineering,
  not an OpenAI source. The exact trigger percentage is inconsistently
  reported across sources found (~95% in one, a hard ≤90% configurable
  cap in another) — treat both numbers as approximate.
- **OpenCode V1 vs V2**: the V1 pruning numbers (40k protected / 20k
  minimum prunable) come from a GitHub issue and DeepWiki, not an
  official doc; the V2 docs' statement that this pruning is "not
  implemented in V2" is officially documented and high confidence, but
  whether V1's behavior as described ever shipped exactly as reported is
  secondary-sourced.
- **Anthropic `clear_tool_uses_20250919` default trigger** (100,000 input
  tokens): came through a WebFetch summarization pass rather than a
  direct quote of the platform docs' literal default-value line; the
  mechanism description around it (oldest-first order, `keep`,
  `exclude_tools`, `clear_at_least`, cache-prefix interaction) is higher
  confidence, directly quoted from the fetched page.
- **Cline's exact truncation formula and file-dedup mechanism**: Cline's
  own official blog post did not state the formula; it came from a
  DeepWiki page that cites specific source file/line ranges
  (`ContextManager.ts`) but was not independently verified against
  Cline's actual GitHub source in this pass.
- **Letta/MemGPT material**: carried over from `docs/research/letta.md`
  (2026-07-06) rather than re-verified here; see that document's own
  methodology note (one arXiv PDF figure-derived number flagged there as
  lower confidence).
- **This report's own Horizon code citations** (file:line references
  under "Context") were read directly from the working tree at HEAD
  `70aec14` and are high confidence; they are not third-party or
  summarized.

## Key findings（日本語、10行以内）

1. Horizon の TokenWindowMemory は新しい順に詰めるだけの純粋な recency
   カットオフで、タスク指示とツール出力を型レベルで区別しない
   （`Message::User` は同一 variant）。preamble に乗る system prompt と
   AGENTS.md だけが構造的に不滅で、タスク指示自体は不滅ではない。
2. 19 ファイル誤読事故は、この区別の欠如がそのまま実害化した実例。
3. 成熟エージェント各社は「指示だけは別チャンネル」を必ず持つ
   （system prompt / CLAUDE.md 再注入 / AGENTS.md 階層 / Memories）が、
   会話内だけの指示は Claude Code 自身も脆弱と公式に認めている。
4. ツール出力特化の淘汰（Anthropic clear_tool_uses、Cline のファイル
   重複排除、OpenCode V1 の prune）が本件に最も直接効く先行事例。
5. 要約系（compaction）は Letta の実証（要約 35% vs 検索 93%、
   「反復精錬で generic and lossy」）が繰り返し警告する劣化リスクを負う。
6. サブエージェント分離（Anthropic 公式・crush agentic_fetch・Letta
   Context Repositories）は事故の根本原因を構造的に防ぐが、委譲設計側の
   論点であり rig-memory の差し替えでは済まない。
7. lost in the middle と context rot は「生き残った文脈の中の劣化」を
   示すが、Horizon の現状はそれ以前に「そもそも消える」問題であり深刻度が上。
8. rig-memory 0.39 は `apply_with_demoted`（軽量、即採用可）と
   `DemotingPolicyMemory`/`CompactingMemory`（`ConversationMemory` 化が
   前提で構造変更を伴う）の2段の道具を既に持つ。
9. 4案（instruction を preamble 側に固定/demoted 活用/カスタム
   Compactor/サブエージェント分離）は排他ではなく組み合わせ可能。
10. 決定はしていない — オーナー相談用の材料。
