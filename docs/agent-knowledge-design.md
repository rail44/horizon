# Agent Knowledge Layer — Design

Status: decisions settled 2026-08-05 (owner consultation in the project
session); v1 implemented and verified the same day (merges `07d90c0`
trust gate, `0cada05` knowledge layer; feasibility validated *before*
this document was written, at the owner's direction). Evidence base:
`docs/research/letta.md` and two empirical arms recorded below.

## Problem

Horizon's runtime agent accumulates no project knowledge across
sessions. The same operational lessons — sandbox behaviors, approval
routing shapes, provider failure modes — were re-derived or re-hit by
session after session, and the supervisor was maintaining a hand-run
knowledge loop in task-brief preambles (measured: a wrong instruction
born, amended twice, and expired across 11 briefs in one day, costing
two denied approvals and ~16 minutes of blocked wall time before it
stabilized). Skills answer this only for low-churn knowledge; the
fast-churn tier had no home. The goal is token economy: knowledge
accumulated from session logs should make future briefs and
investigations shorter.

## Shape

Three tiers, distinct by churn rate and location:

```
JSONL event log            (immutable 正本, per-machine)
  │   distillation: manual, reflection over labeled history
  ▼
knowledge layer            (user-side files, fast churn — THIS design)
  │   promotion: owner-mediated, rare
  ▼
repository skills / docs   (in-repo, low churn, owner-reviewed)
```

The knowledge layer lives at
`~/.local/share/horizon/knowledge/<sanitized-root>/<id>.md` (XDG
respected; `<sanitized-root>` is the project's main-repo toplevel with
`/` → `-`). **Nothing in this layer is ever placed in the repository**
(owner decision: fast-churn material does not belong in git; the repo
is reached only by promotion into skills/docs).

One file per entry: YAML-style frontmatter (`id` slug, one-line
`description`, optional `anchors` — repo paths/symbols the knowledge
depends on, required `sources` — `session:<uuid8> seq:<range>`
references into the event log, `created`/`updated`, `status:
active | needs-review | expired`) followed by a free markdown body.

## Decisions and rationale

1. **Files are the store; DuckDB arrives later as a projection, and
   half of it is already free.** The strongest empirical finding in
   the Letta survey is that filesystem-plus-grep memory beat a
   specialized graph memory (LoCoMo 74.0% vs Mem0 68.5%): familiar
   primitives over clever structure. Files are also owner-editable —
   the human prune is part of the quality mechanism, not a
   convenience. The owner's idea of representing entries relationally
   (scores, staleness metadata, relations to log events) is deferred,
   not rejected: when wanted, it must be a **rebuildable projection
   over the files** — the same `agent-events.jsonl` →
   `agent-state.duckdb` pattern this repo already runs — never a
   second authority. Meanwhile usage tracking costs nothing today:
   `knowledge.read`/`write` are tools, so every read and write already
   lands in the event log with a session id, and session outcome
   labels are already projected — "did sessions that read entry X fare
   better" is a DuckDB join, not new telemetry.
2. **Every entry cites sources; refined knowledge must be
   re-refinable.** Letta's own post-mortem of refinement ("memories
   become generic and lossy after repeated refinements") plus the
   MemGPT DMR result (93.4% retrieval vs 35.3% recursive summary) are
   the survey's core warning. The raw log stays 正本; an entry that
   degrades is re-refined from its cited event range, never from its
   own previous wording. Presenting entries as verifiable log-derived
   facts (rather than subjective memory) also counters the measured
   model tendency to distrust its own memory under pressure
   (letta.md §16).
3. **Delivery is two-tier, matching skills' progressive disclosure.**
   An always-loaded index (id + description only, 16k-char cap,
   newest-`updated`-first on overflow) rides the system prompt as a
   fifth extra section; `knowledge.read` pulls one entry's body on
   demand. Rationale: the always-loaded part is what saves tokens (a
   retrieval round-trip costs a provider round; the index is a cached
   static prefix), and description quality drives discovery (skills
   self-discovery cost −6.5% in Context-Bench Skills). The feasibility
   probe's "delivery gap" finding — two lessons fully documented in
   docs/ still cost 8 of 25 tool errors in a day — is why the index is
   always-on rather than search-only.
4. **The agent writes directly; validation is structural, audit is the
   event log.** `knowledge.write` (upsert) requires a slug id, a
   non-empty description, and non-empty sources; it is not
   approval-gated (owner decision: per-item review gates are
   unnecessary in a personal project — Letta's +36.8%-with-human-
   feedback result is served by the owner pruning the store and by the
   integrator grading distillation output, not by a blocking gate).
   fs/bash confinement is untouched — the store is reachable only
   through the tool, so writes are always attributable events.
5. **Trust gates the whole layer, and trust is not a knowledge-layer
   concept.** The consultation surfaced that repository content
   already reached prompts unconditionally on two paths (repository
   skills — which shadow embedded skills by id — and AGENTS.md
   ingestion) under a risk acceptance noted in `skills.rs` since
   2026-07-07. `trusted_projects` (user config, absolute repo
   toplevels, worktree-resolved like `grants.project`) now gates all
   prompt-influence paths: an unlisted root gets embedded skills only,
   no instructions, no knowledge index, no knowledge tools, plus a
   one-line prompt note. Action defense stays with sandbox/approvals;
   this gates prompt influence only.
6. **Distillation is manual.** The right cadence is unknown; the owner
   runs it (or asks a session to) when sessions have accumulated.
   Sleep-time-style automation (letta.md §4) is a later investment —
   the survey itself grades it as low-evidence and advises synchronous
   simplicity first. The embedded `horizon-distill` skill carries the
   procedure (signal priorities, dedup-against-index, write
   requirements); its original 2026-07-07 form drafted repository
   skills directly, which conflated the fast-churn and low-churn tiers
   and was reworked to target this layer.
7. **Staleness is detected mechanically, not guessed.** Anchors make
   "did the anchored files change since `updated`" a git-diff check; a
   distillation pass re-verifies flagged entries against current code
   plus their cited log ranges. Contradiction resolution is
   overwrite-not-append (the same discipline as the store's upsert
   semantics). Full automation of the flag pass is future work.

## Evidence (both arms ran before this document)

**Arm 1 — signal existence** (strong reader over one day's log, 63MB /
74,845 events / 11 task sessions): 11 candidate entries, 4 fully novel
(one — the bash timeout kill not reaching hook descendants, now issue
017 — unknown even to the supervising integrator), 5 partially novel,
2 documented-but-still-costing. Deterministic labels located lessons
(`is_error` best; denials and mid-course user corrections 100%
precision; `Completed` actively untrustworthy). The corpus also
contained the hand-run loop described under Problem, including an
expiry event — evidence the mechanism replaces paid labor rather than
inventing work.

**Arm 2 — extraction by the production model** (`syn:large:text`,
prompted distillation pass over sessions the probe never saw): one
honest novel entry (`full-suite-covers-every-test`, with sources,
anchors, and an unprompted cross-reference to a sibling entry),
correct dedup of five friction classes against the seeded index, and
honest zero-yield accounting of 165 cancelled turns. Extraction is not
gated on a stronger model, though the reflection model remains
swappable later (same env-override pattern as the judge's).

Store at v1 close: 11 seeded entries (probe output, integrator-graded)
plus 1 distilled = 12 active.

## Non-goals and deferred

- **Embeddings / semantic search**: deferred until grep+metadata
  proves insufficient (survey evidence points the other way; DuckDB
  VSS can slot in behind the same tool surface later without contract
  change).
- **Relational/graph representation of entries**: deferred; must be a
  projection over the files (decision 1).
- **Automatic distillation cadence**: deferred (decision 6).
- **Per-item review gates**: rejected for now (decision 4).
- **In-repo placement of any dynamic entry**: rejected (Shape).

## Pointers

- Implementation: `crates/horizon-agent/src/knowledge/`, tools in
  `crates/horizon-agent/src/tools/knowledge/`, index assembly in
  `providers/rig/session_prompt.rs`, trust in `crates/horizon-config`
  (`trusted_projects`) and `crates/horizon-agentd/src/session/setup.rs`.
- Survey: `docs/research/letta.md` (esp. §1, §14, §15, §16 and the
  cross-cutting section (b)).
- Prior art in-repo: `docs/agent-feedback-design.md` (2026-07-07
  decisions; its outcome labels are this layer's location signals).
