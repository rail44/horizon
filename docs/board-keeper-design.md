# Board Keeper — Package Design

**Status:** v1 (manual launch). v2 destination: automatic wake-up from
`horizon-logd`'s board-event watch.

## 1. Decision: board as a package, not just data

The task board is not just a data store with a view. It is a **package**:
the feature (the board store) plus the **agent definition** that operates it
plus the **skill** that guides that agent. The board keeper is the first
concrete instance of this shape.

This is an early, concrete example of the future extension/plugin
abstraction. The owner's decision (2026-08-06) is to start from a concrete
case rather than building the abstraction first — designing an abstraction
from a single state is premature. **This extraction premise must be recorded
in this doc and the implementation must not hard-code board-specific
assumptions that prevent generalization.** The two registration seams
(`roles::register_external` and `skills::register_external_skill_sources`)
are deliberately generic: they accept any `RoleDefinition` / `&'static str`
pair, not board-specific types.

## 2. The keeper's role

The keeper reads the board's items and their comments, reconstructs missing
context from the codebase, conversation logs, and docs, and writes that
context back as **comments** on the items that need it — so that anyone
(owner, integrator, or a future worker) can read an item and understand what
it is about, what the open questions are, and why it is in its current state,
without any prior conversation.

Concrete example: the owner comments "I don't understand the problem here"
on an item. The integrator reconstructs the context from conversation logs,
code, and docs, and writes a comment that explains — in a form readable by
someone with zero prior context — what the problem is, what the decision
points are, and why it is on hold. This is the keeper's job.

## 3. Dependency direction

**Decision:** `horizon-agentd` depends on both `horizon-agent` and
`horizon-board`. No Cargo edge between `horizon-agent` and `horizon-board` in
either direction.

- `horizon-board` does not depend on `horizon-agent` — it never has, and that
  boundary is an owner requirement (`crates/horizon-board/src/lib.rs:12-14`:
  "Zero-dependency on `horizon-agent` or any other Horizon crate. The crate
  boundary is a seam for future extension/plugin abstraction.").
- `horizon-agent` does not depend on `horizon-board` — owner decision. A
  `horizon-agent → horizon-board` edge would pull DuckDB/rig-core (and
  everything `horizon-agent` links) into the board crate's dependency closure,
  which would break the board crate's zero-dependency boundary and make it
  impossible to use the board store from lightweight contexts (the CLI, the
  shell) without the entire agent runtime.
- `horizon-agentd` is the **composition root**: it already depends on
  `horizon-agent` (it hosts agent sessions) and now also depends on
  `horizon-board` (it constructs the board store for board tool execution and
  assembles the keeper role from board's data). It wires the two together at
  startup.

**Why not a shared type crate?** The `RoleDefinition` struct lives in
`horizon-agent` and its fields are all `&'static str` / `&'static [&'static str]`
— compile-time references. `horizon-board` exports the keeper role's fields as
`pub const` data (`crates/horizon-board/src/keeper.rs`). `horizon-agentd`
copies these by value into a `RoleDefinition` it constructs at startup. No
new crate is needed because the data is thin references, not complex types.
A separate "small type-location" crate would be justified if a third
contributor needed the same `RoleDefinition` type — but with one role from
one external crate, the composition root is sufficient.

## 4. Implementation

### 4.1 Role registration (`roles::register_external`)

`horizon-agent`'s `roles.rs` gains a runtime-registration seam alongside its
compile-time `static ROLES` slice:

```rust
static EXTERNAL_ROLES: OnceLock<Vec<RoleDefinition>> = OnceLock::new();

pub fn register_external(roles: Vec<RoleDefinition>) { ... }

pub fn resolve(role_id: &RoleId) -> Option<&'static RoleDefinition> {
    // Check ROLES first, then EXTERNAL_ROLES
}
```

`horizon-agentd`'s `main` calls `register_external` once at startup, before
any session is spawned, with the keeper `RoleDefinition` assembled from
`horizon_board::keeper`'s `pub const` fields. The `OnceLock` is populated
once and read lock-free thereafter (every session-start path calls
`resolve`).

This is the "external definition reception route" the owner asked for —
the role mechanism accepts definitions from outside `horizon-agent` without
adding the external crate to `horizon-agent`'s Cargo dependencies.

### 4.2 Skill registration (`skills::register_external_skill_sources`)

`horizon-agent`'s `skills.rs` gains a parallel seam:

```rust
static EXTERNAL_SKILL_SOURCES: OnceLock<Vec<&'static str>> = OnceLock::new();

pub fn register_external_skill_sources(sources: Vec<&'static str>) { ... }
```

`embedded_skills()` (the function that builds the cached `Vec<Skill>` from
`include_str!` constants) now appends the externally-registered sources to
its own four embedded skills before parsing. `horizon-agentd`'s `main`
registers `horizon_board::keeper::SKILL_SOURCE` (an `include_str!` of the
board crate's `skills/board-keeper/SKILL.md`) at startup.

The skill lives in `crates/horizon-board/skills/board-keeper/SKILL.md` and
is `include_str!`-loaded by the board crate itself (not by the agent crate),
so the board crate owns both the role data and the skill content as a single
package. The agent crate's skill mechanism treats it identically to its own
embedded skills once registered.

### 4.3 Board tools (`board.read`, `board.comment`)

Two new tools in `horizon-agent`'s catalog:

- **`board.read`** — read the board (list items or show one with comments).
  `AutoAllowRead`.
- **`board.comment`** — append a comment to an item. `AutoAllowRead` (the
  board event log is the audit trail, same reasoning as `knowledge.write`).

**The seam:** `horizon-agent` cannot depend on `horizon-board`, so board
operations go through a `BoardHost` trait (mirroring `ExplorationHost`):
a daemon-provided capability handle, installed on `ToolSessionState` at
session construction. `horizon-agentd` implements `BoardHost` using
`horizon_board::Store`. Board reads are synchronous file folds; board writes
(comment) are async (one remoc rtc round-trip to `horizon-logd`), blocked on
a per-call current-thread tokio runtime since the session thread is a plain
OS thread.

**Why separate tool ids, not one `board.write`:** the allowlist is
per-tool-id, not per-field-within-a-tool. To express "comments only, no
status/rank/assignee changes" structurally (not just in the prompt), the
keeper role's `allowed_tool_ids` lists `board.comment` but no
state-mutation tool — and no state-mutation tool exists in the catalog at
all. This is the same pattern as `config.read`/`config.write` being separate
tools. If a future role needs board state mutations, it would get its own
tool ids (`board.set_status`, etc.) and its own catalog entries.

### 4.4 The keeper role definition

| Field | Value | Rationale |
|---|---|---|
| `id` | `"keeper"` | Wire/persistence identity |
| `title` | `"Board Keeper"` | View chooser |
| `allowed_tool_ids` | `fs.read`, `fs.grep`, `fs.glob`, `board.read`, `board.comment`, `skill.read`, `knowledge.read`, `recall.search`, `recall.read` | Read-only + board comment. No bash, no file writes, no board state mutation |
| `model` | `None` | Use provider default |
| `iteration_cap` | `None` | Interactive, not one-shot |
| `include_repository_instructions` | `true` | Needs project conventions to reconstruct context |
| `skill_ids` | `["board-keeper"]` | The keeper skill |
| `summarize_on_cap` | `false` | Interactive, not a delegated report |

### 4.5 v1 permissions — why this narrow set

- **Write: board comments only.** Status / rank / assignee / item addition
  are the owner's and integrator's decisions. The keeper's job is context
  restoration, not task management. Structural enforcement (the allowlist
  lists `board.comment` and no state-mutation tool exists) means the model
  cannot express a state-changing call even if it tried — the tool is
  simply not advertised.
- **Read: same as an ordinary session.** The keeper cannot reconstruct
  context without reading code (`fs.*`), docs (`fs.*`), conversation history
  (`recall.*`), and the project knowledge layer (`knowledge.read`).
  `skill.read` is needed to read its own skill.
- **bash: not given.** The keeper's work is reading and commenting — it does
  not need to run commands. Adding bash would expand the attack surface
  (bash can write files, change the board via CLI, etc.) with no benefit to
  the keeper's task. If a future keeper task needs bash, the decision to add
  it should be deliberate and documented, not a default.

## 5. Launch

**v1: manual.** The keeper is launched by specifying the `keeper` role when
creating a session, through the existing role-specified session-creation
path (the same path `config` and `explore` roles use). No new UI is needed.

**v2 destination: automatic wake-up.** The keeper should wake automatically
when the board changes — specifically, when `horizon-logd`'s board-event
watch fires (a new comment or item is appended). This is **not built in v1**
but is the intended evolution: `horizon-logd` already exposes a subscribe
stream (`Store::subscribe`) that pokes on each appended event; a v2 keeper
would subscribe to that stream and launch when an item needs attention. The
manual launch in v1 proves out the role, skill, and tool design first; the
automatic wake-up is a scheduling concern that can be added without changing
the role definition or tools.

## 6. Extraction premise

This implementation is deliberately not board-specific in its mechanism:

- `register_external` accepts any `Vec<RoleDefinition>`, not a board type.
- `register_external_skill_sources` accepts any `Vec<&'static str>`, not a
  board skill.
- `BoardHost` is a trait with `list`/`show`/`comment` — a future package
  with different operations would define its own host trait.
- The keeper role's `allowed_tool_ids` references generic tool ids
  (`board.read`, `board.comment`), not board-internal types.

A future extension/plugin system would generalize these three seams (role
registration, skill registration, host-trait injection) into a single
"package" trait or registration call. The owner's decision is to wait for a
second concrete package before designing that abstraction.
