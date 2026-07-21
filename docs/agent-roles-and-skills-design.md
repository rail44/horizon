# Agent Roles, Skills, and the Configuration Agent

Written 2026-07-06 (plan `docs/plans/agent-foundation/03-roles-and-config-agent.md`,
shared foundations 1 and 2 in `docs/roadmap.md`). This document records the
design decisions behind the first role-tagged agent session — the
configuration agent — and the minimal skill mechanism it rides on, plus the
runtime config reload that makes its changes visible without a restart.

The audit that motivated the shape of all of this is
`docs/research/agent-prompting.md`: Part 2.5 added the two back-compatible
extension points this builds on (`system_prompt`'s `extra_sections`,
`RigAgentConfig.allowed_tool_ids`); Part 2.6 deliberately deferred every
other role decision "until a second role exists". The configuration agent
is that second role.

## The owner's open question (and how this implementation treats it)

Whether a "domain agent" is a **defined role** or a **generic coder
specialized by loaded skills** is deliberately undecided (owner reservation,
2026-07-06). This implementation is positioned to produce evidence for that
fork, not to settle it. Consequences:

- The role mechanism is the smallest mapping that works: a static,
  crate-local registry entry — one prompt section, a tool allowlist, an
  optional model override, a repository-instructions opt-out, and a list of
  skill ids. No config-file schema for roles, no user-facing definition
  flow (that is roadmap "Later"), no per-role approval policy.
- All domain knowledge lives in the **skill**, not the role: the role's
  prompt section only frames the job and points at the tools; everything
  the agent needs to know about Horizon's config schema is in the
  `horizon-config` SKILL.md, loaded on demand.
- The evidence section at the end of this document records what each side
  actually carried once implemented.

## The role mechanism

A role is resolved by name (`RoleId`) from a static registry in
`crates/horizon-agent` (`roles.rs`). A `RoleDefinition` is exactly:

- one extra system-prompt section (framing, not procedure — per the
  prompting audit's "environment setup, not thinking lessons" conclusion);
- `allowed_tool_ids: Option<&[&str]>` — feeds the existing
  `rig_tool_definitions` filter; `None` means every tool, `Some` narrows;
- `model: Option<&str>` — per-session override of the provider's model
  (the config role does not use it; the seat exists because the plan's
  role mapping names it and model-routing is a stated future consumer);
- `include_repository_instructions: bool` — see the trust note below;
- `skill_ids: &[&str]` — which embedded skills to announce in the prompt.

An unknown role id fails the session start loudly rather than silently
degrading to a role-less session: a config agent that quietly became a
generic coder with every tool would be a trust bug, not a fallback.

### How a role travels: wire v2

Per the audit's Part 2.6 recommendation, the untyped, never-consumed
`SessionNew.config_overrides` placeholder is replaced by a typed
`role_id: Option<RoleId>`, and `CONTRACT_VERSION` moves 1 → 2 (the
handshake already rejects mismatches, and `Reload Agent Runtime` is the
existing recovery path for a stale `horizon-agentd` binary).
`StartSession`/`Initialization` carry the same field, so a single rig
`Provider` derives a per-session `RigAgentConfig` (clone of the process
config with the role's allowlist/model applied) instead of registering one
provider per role — the alternative (a) in Part 2.6, rejected because it
duplicates session-loop wiring per role.

A session's role is persisted in the event-log `Record` envelope as
`role_id: Option<RoleId>`, mirroring `provider_id` exactly, so resume
after an agentd restart reconstructs the session with its role (and its
narrowed tools) intact. Old logs deserialize with `role_id = None`.

## The skill mechanism

The minimal form follows `docs/research/agent-prompting.md` Part 3.2's
sketch, with one adaptation: skills ship **embedded in the binary**
(`include_str!` from `crates/horizon-agent/skills/<id>/SKILL.md`), not as
files under the user's cwd — the configuration agent must work no matter
where Horizon was launched, and its knowledge versions with the binary
whose config schema it describes.

Progressive disclosure keeps its three stages:

1. **Always loaded**: a session whose role lists skills gets one extra
   prompt section — "`<name>` — `<description>`" per skill plus a single
   line telling the agent to read a skill before relying on it.
2. **Loaded on trigger**: the `skill.read` tool (auto-allowed read)
   returns a skill's markdown body.
3. **Loaded as needed**: a skill body can point at tools (here:
   `config.read`) for anything live.

Role-less sessions get no skills section and remain byte-identical to
today's prompt. `fs.read` was not reused as the loading tool (Part 3.2's
original sketch) because embedded skills are not on disk and the config
role deliberately has no filesystem access.

## The configuration agent

The `config` role is the first role and the first skill consumer. It is
launched as a named command — `New Configuration Agent` in the palette,
`new-config-agent` over the control plane / CLI (mirroring `new-agent`'s
`--prompt`/`--split`/`--active`). The external vocabulary names each
role-tagged flavor rather than exposing a free-form `--role` argument:
the set of roles stays the binary's to define, never a client-supplied
string, and an unknown role id can therefore only arise from version
skew, where it fails the session start loudly.

- prompt section: Horizon configuration assistant framing — read the
  skill and the current config before proposing changes; write the
  complete file; changes apply on approval.
- tools: `skill.read`, `config.read`, `config.write` — nothing else. No
  bash, no fs.
- `include_repository_instructions: false`. **Trust note:** repository
  instruction files (AGENTS.md/CLAUDE.md) are exactly the prompt-injection
  surface `docs/trust-boundaries.md` warns about, and this agent writes
  host-owned configuration. A repo's instructions have no legitimate
  business steering it, so it does not read them. Approval still gates
  every write; this opt-out just narrows the inputs.

### Config tools and their trust reasoning

`fs.write` cannot reach `~/.config/horizon/config.toml`: the fs tools are
confined to the session's workspace root by design. Widening that sandbox
for one file would be backwards; instead the config agent gets two
dedicated tools that can touch **only** the one host-owned path, resolved
by the same `HORIZON_CONFIG` > XDG > `~/.config` chain `horizon-agentd`
already uses for its own config read:

- `config.read` (auto-allowed): resolved path + contents, or an explicit
  "does not exist yet" (the onboarding case).
- `config.write` (requires approval): whole-file write. Rejects content
  that does not parse as TOML (the error goes back to the agent, which
  can self-correct before ever reaching the user); requires a prior
  `config.read` when the file exists and refuses if the file changed
  since (the same prior-read + staleness discipline as `fs.write`);
  creates parents; writes atomically (temp file + rename).

Cataloging these globally adds no capability — `bash` (also
approval-gated) could always write this file. The narrowing lives in the
role allowlist: the config agent can write config and nothing else, per
`docs/trust-boundaries.md`'s "a module cannot call what the host does not
hand it". Deeper schema validation (do these keys exist? are the hex
values parseable?) stays out of the tool: Horizon's loader already
warns-and-skips invalid entries without crashing, the skill teaches the
schema, and duplicating Horizon's `src/config` validation inside
`horizon-agent` would create a second source of truth to drift.

## Runtime config reload

Config was startup-only; the configuration agent needs its approved write
to become visible. The reload is **partial by design**:

- **Theme** (`[theme]`, `[theme.ansi]`, and the terminal colors derived
  from them) and **keybindings** apply live.
- Everything else ([terminal] metrics/shell, [ui] fonts/window, [agent]/
  [provider]) keeps startup semantics — [agent]/[provider] already have
  their own reload story (`Reload Agent Runtime` respawns agentd, which
  re-reads the file), and window/PTY parameters are inherently
  per-startup/per-spawn. The status line says so rather than pretending.

Mechanics: theme values were cached in `OnceLock`s and read inside floem
style closures with no reactive dependency, so a swap alone would never
re-style. The fix is inside the theme module: accessors now read a
reactive signal holding the resolved chrome/ANSI state, so every existing
call site tracks it for free and a swap re-runs styling app-wide; the
terminal's derived colors live in a separate cross-thread store, since
cell rendering happens off the UI thread (see "What the E2E shook out"
below). Keybindings just swap the global keymap — it is re-read on every
keystroke. A reload that fails to
parse keeps the currently applied values and reports the error; it does
not reset a working theme to defaults over a typo (deliberately different
from the startup fallback, where there is nothing applied yet to keep).

`Reload Config` is a first-class command (palette, keybinding id
`reload-config`, CLI `horizon reload-config`) per the "operations go
through the command model" convention. On top of that primitive, Horizon
observes a successful `config.write` tool result in the agent event stream
and executes the same command automatically — approve the write, see the
theme change.

## Design memo: the future color-picker view

Roadmap "Later" plans a first-party color-picker view (native Rust, per
the 2026-07-06 view-foundation decision). The connection point designed
for here is deliberately thin: the picker edits the same `[theme]` /
`[theme.ansi]` keys and calls the same `Reload Config` command — it does
not need the agent at all. The natural composition, when it exists, is the
configuration agent opening the picker (a pane/command) for the "choose a
color" step and receiving the chosen value back into the conversation;
nothing in the role/tool surface blocks that, and nothing anticipates it
beyond this paragraph.

## What the E2E shook out (implementation notes)

The completion criterion — a live conversational theme change visibly
recoloring the running app — failed three times before it passed, each
time on a reactive-lifetime property no unit test exercises (nothing in
tests reads signals from inside effects or across threads):

1. A lazily created global signal belongs to whatever scope is current at
   first access — in the running app, some view's style effect — and dies
   when that effect re-runs. Process-lifetime reactive state must be
   created on a detached root `Scope` (`ui::theme`'s `THEME_STATE`).
2. Thread-local reactive state cannot feed consumers on other threads:
   terminal cell colors are resolved on session threads, which saw their
   own never-reloaded copy. Cross-thread values live in a plain lock
   (`ui::theme`'s `TERMINAL_COLORS`), reactive values stay UI-side.
3. Effects created while another effect is running are its children and
   die on its next run. The CLI control-plane bridge's request pump is an
   effect, so anything `execute_command` builds that must outlive the
   command — a session's event fold — needs its own detached scope
   (`agent::agentd_runtime::fold_agent_session_events`). The same latent
   hazard exists for `reload_agent_runtime`'s responder/status effects
   when invoked over the CLI; left as-is here (pre-existing shape, one
   command deep) and worth a sweep of its own.

## Evidence: role vs. skill, from this implementation

What each side actually carried, once the configuration agent worked end
to end (live provider, 2026-07-06):

- **The role stayed a capability envelope, not a persona.** The final
  `RoleDefinition` is a dozen lines of data: one framing paragraph, three
  tool ids, `model: None`, two booleans, one skill id. Every attempt to
  put *knowledge* in it (schema, valid names, editing rules) read better
  in the skill, and the live sessions confirmed the agent treats the
  skill as the authority — each run began with `skill.read` +
  `config.read` before proposing anything, preserved unrelated config
  entries, and correctly told the user which sections apply live versus
  at restart, all of which it can only have learned from the skill body.
- **What the role did could not have been a skill.** The tool allowlist
  is enforcement, not knowledge — a skill can teach restraint but not
  impose it. The repository-instructions opt-out is a trust decision
  applied before the model reads anything. The persisted `role_id` is
  identity — it survives an agentd restart and reconstructs the narrowed
  session. All three are harness properties; none are prompt content.
- **Unused seats stayed unused.** `model: None` (the seat exists because
  the plan's mapping names it and model routing is a stated future
  consumer, but nothing about *this* role wanted a different model), and
  the prompt section never grew procedure.

**Reading for the owner's fork** (defined role vs. skill-specialized
generic coder): this implementation behaves like *both at once*, split
along an enforcement/knowledge line — a generic loop, specialized by a
skill (knowledge), inside a role-shaped capability envelope (tools,
trust, identity). Everything conversational about the "domain agent" came
from the skill; everything the skill could not do was exactly the
envelope. If that line holds for the next role, "role" in Horizon should
stay a small envelope (allowlist + trust flags + model + skills) and
never accumulate domain text — and a future user-facing agent-definition
flow (roadmap "Later") becomes "pick skills, pick an envelope" rather
than authoring prompts. The fork stays open until a second role tests
the line; this is one data point, not a verdict.

## v2: a repository skill layer, default skills, and the first generic consumer

Written 2026-07-07 (owner decisions made in-session; implementation follows
this section). Where v1 left skills embedded-only and gated behind a role,
v2 does two things: adds a second, on-disk skill source that can be edited
without rebuilding the binary, and gives skills to role-less sessions too —
so `skill.read` finally has a generic-session consumer, not just the config
role.

### The repository skill layer: `.horizon/skills/<id>/SKILL.md`

Mirroring `instructions`' `AGENTS.md`/`CLAUDE.md` walk exactly:
`skills::SkillRegistry::discover(cwd)` walks from the session's cwd up to
its git root (or just `cwd`, outside a repository — same rule as
`instructions::ancestor_dirs_from_git_root`, now shared by both), and reads
every `.horizon/skills/<id>/SKILL.md` found along the way. Same frontmatter
format as an embedded skill (`name:`/`description:` + Markdown body); a
directory whose `SKILL.md` doesn't parse warns on stderr and is skipped,
never crashing the session (the config-file-style discipline
`instructions::read_to_string_or_warn` already established).

**Override order: repository wins.** A repository skill sharing an embedded
skill's id replaces it outright for that session (not just its body — its
description too), and a repository skill discovered nearer the session's
cwd replaces one discovered at a more distant ancestor with the same id
(the same "nested refines root" composition order `instructions` uses).
This is a deliberate, accepted trust trade, not an oversight: a
`.horizon/skills/` directory is exactly the prompt-injection surface
`docs/trust-boundaries.md`'s tier reasoning warns about — the same surface
`roles::CONFIG_ROLE`'s doc comment already flags for `AGENTS.md`/
`CLAUDE.md` ingestion — and sharper here, since it can silently shadow an
embedded skill's instructions (including `horizon-config`'s, for any
session that happens to run in a repo carrying one). Accepted anyway,
because Horizon is presently a personal, single-owner project: this is a
deliberate hypothesis-testing setup, letting a skill be iterated on by
editing a file instead of rebuilding and restarting the binary, exactly
the same trade the repository-instructions ingestion already makes. Should
Horizon ever gain other users or a threat model with an untrusted
repository, this override direction is exactly what would need
revisiting (see `docs/trust-boundaries.md`).

**Discovery is cheap, reads are fresh.** Only frontmatter is consulted at
session start (there's no cheaper way to reach just the frontmatter with
this crate's hand-rolled parser, but the body is discarded immediately
after parsing) — a repository skill's `Skill` value stores a path, not a
cached body. `skill.read` re-reads and re-parses that path from disk on
every call, so an edit followed by a fresh `skill.read` in the same
session sees the new content without a session restart — the "edit,
observe" loop this layer exists for. A repository skill's body is capped
at `skills::SKILL_BODY_CAP_CHARS` (a plain constant, not a config knob —
mirroring `repository_instructions_cap_chars`'s discipline without needing
its own file-configurable knob, since there's no per-deployment reason to
tune this crate-internal limit).

### Per-session composition, not a global static

`skills::SkillRegistry` moved from v1's process-lifetime `OnceLock` (a
fixed, compile-time-known set) to a per-session value: embedded builtins
composed with whatever `.horizon/skills/` this session's cwd resolves to,
sorted by id for deterministic listing/lookup order. Built at two
independent production sites, both from the same process cwd (a cheap,
session-start-once listing, the same duplication style `instructions`'
cwd-derived state already has between call sites):

- `providers::rig::session::session_extra_sections` builds one for the
  prompt section (below).
- `horizon-agentd`'s `session::run_session` builds a second one for
  `skill.read`'s dispatch, installed onto `tools::ToolSessionState` via a
  new builder method (`ToolSessionState::with_skills`) rather than a
  `for_current_dir` parameter — mirroring how `RecallContext` was threaded
  in, except set post-construction so that constructor's signature (and
  every non-production caller: this crate's own tests, `tools::recall`'s
  tests, Horizon's UI-side dummy-tool-state test helper) stays unchanged.

### Default skills for generic sessions

v1's `skills_prompt_section` only ever ran for a role naming `skill_ids`;
a role-less session got no skills section at all, "no consumer" being the
byte-identical backward-compatible case. v2 gives every role-less session
a skills section too, listing *every* skill this session's registry
knows about (embedded + repository) — `SkillRegistry::
prompt_section_for_all`. A role-bearing session's curated `skill_ids`
listing is unchanged in shape (`SkillRegistry::prompt_section_for_ids`),
except it now resolves against this same per-session composed registry
instead of a fixed embedded set — so a repository skill can override an
embedded one even under a role's narrower envelope, consistent with the
override order above. The role's allowlist itself (`allowed_tool_ids`)
is untouched by any of this: a config-role session still cannot reach
`bash`/`fs.*` regardless of what a repository defines under
`.horizon/skills/`.

The byte-identical-prompt guarantee from v1 (an empty skill set adds no
section at all) still holds structurally, but this build never actually
exercises it in production — it always embeds at least `horizon-config`
and `horizon-cli` — so it's exercised directly against a hand-built empty
`SkillRegistry` in `skills`' own tests rather than through
`SkillRegistry::discover`.

### `horizon-cli`: the first generic-session skill

`crates/horizon-agent/skills/horizon-cli/SKILL.md` is the first skill a
role-less session can discover on its own initiative (v1's
`horizon-config` was role-gated). It teaches an agent to operate the
Horizon workspace it's running inside via the `horizon` CLI: orientation
(`sessions`/`state`), pane creation (`new-terminal`/`new-agent`/
`new-config-agent`, `--split`'s "here" vs. explicit vs. omitted placement,
`--active`'s focus-stealing caution), attach/terminate/approve/deny/
cancel-turn, and the `--yes`/destructive-command convention for running
non-interactively from `bash`. Its description frontmatter names the
trigger explicitly ("operate Horizon itself: open panes/terminals, attach
sessions, run commands in the workspace this agent lives in") so the
model can match it against a task without ever reading the body first.
Content is sourced from `crates/horizon-cli/src/cli.rs` (the real parser),
`docs/cli-control-plane-design.md`, and `AGENTS.md`'s own command list —
not re-derived from memory, so it can't drift from the actual subcommand
surface at the point it was written.

### `github-pr`: authenticated publication workflow

Dogfooding on 2026-07-21 showed a generic agent following a browser-oriented
`/pull/new/...` URL with `web_fetch`, mistaking GitHub's login page for an
unavailable PR workflow, even though the authenticated `gh` CLI was present.
The remedy is procedural knowledge, not a new capability or a permanent
system-prompt rule, so it ships as the embedded `github-pr` skill. Its trigger
covers committing, pushing, and opening/inspecting/updating a GitHub PR; its
body directs the agent to use local Git plus `gh pr create`/`gh pr view`, keep
unrelated changes out of the commit, and leave enough time for repository
hooks. Role-less sessions discover it through the existing v2 registry and
load the body only when the task matches.
