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

Non-goals for this baseline (deferred; see the last section): web tools,
context compaction, MCP, OS sandboxing, persistent shell sessions,
plugin-provided tools, running agent commands inside terminal sessions.

## Tool Set

| Tool  | Permission       | Notes                                              |
|-------|------------------|----------------------------------------------------|
| read  | auto-allow read  | Line-windowed (offset/limit) with a default cap    |
| glob  | auto-allow read  | Dedicated tool, not shell                          |
| grep  | auto-allow read  | Dedicated tool, not shell                          |
| write | require approval | Creates parents; overwrite requires prior read     |
| edit  | require approval | Exact string replacement (below)                   |
| bash  | require approval | Fresh process per command (below)                  |

All tools require **absolute paths**; relative paths are rejected with an
actionable error (models measurably mishandle relative paths — SWE-bench-era
finding restated in Anthropic's agent guidance).

## Edit Semantics

The industry has converged on exact-string replacement with uniqueness
enforcement (Claude Code, Gemini CLI, OpenHands, goose, Cline):

- `old_string` must match **exactly** and **uniquely**. Zero matches and
  multiple matches are `is_error` results with actionable text ("found 3
  matches — include more surrounding context"), never a silent first-match.
- **Staleness gate, enforced mechanically:** a file must have been read in
  this session, and its mtime must be unchanged since that read, or the edit
  is rejected ("file changed on disk — read it again"). Read-before-edit is a
  harness invariant, not prompt discipline.
- No fuzzy-match fallback in v1. Gemini ships a four-tier fuzzy cascade;
  Claude Code deliberately ships none. Start strict, collect failure data,
  add leniency only if the data demands it.

## Bash Semantics

- Fresh process per command; the harness tracks the working directory across
  calls (`cd` persists via tracking, not via a live shell).
- Wall-clock timeout, default 120s, per-call override up to a hard max.
- Output capped in-context (~30k chars, head+tail preserved); the full output
  spills to a temp file whose path is included in the result so the agent can
  re-read selectively. (Truncate-in-context + spill-to-file is the shipping
  standard across Claude Code, goose, Cline, Codex.)
- Cancelling a turn kills the process group of any in-flight command.

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

Neither measure caps memory directly (niceness affects CPU scheduling, not
memory), so they don't replace the idempotence fix — they reduce the blast
radius of any future bug that lets a session accumulate more than one
in-flight bash call.

## Error Model and Loop Guards

- Every tool failure returns an `is_error` tool result; the loop never
  crashes on tool errors. Error text says what went wrong and what to try.
- Turn iteration cap, plus doom-loop detection: N consecutive identical
  (tool, args, result) fingerprints halt the turn with an explanatory event.
- The system prompt carries a one-line retry nudge (models otherwise tend to
  give up after a single tool failure — documented by OpenAI).

## Turn Loop and Cancellation

The current per-session loop blocks the whole OS thread inside
`block_on(turn)`, so `Command::Cancel` is structurally unreadable mid-turn.
This changes:

- The session loop becomes concurrent: commands are received while a turn is
  in flight (async loop with `select!`, or turn spawned as a task; the
  command channel becomes async-capable).
- A `tokio_util::sync::CancellationToken` scopes each turn; the streaming
  loop and tool execution `select!` against it; bash children are killed on
  cancel.
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

## Config

Provider/model selection, base URL, and the bash/fs tool tuning and
turn-loop guard values on this page all flow through Horizon's single TOML
config file plus environment variables (env wins) — see `AGENTS.md`'s
"Configuration" section and `config.example.toml` for the full precedence
and knob list. The API key stays environment-only. No configuration UI.

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

- **web_search / web_fetch** — `curl` via bash covers development use.
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
- Codex CLI source (exec/unified_exec, apply_patch); Gemini CLI source
  (edit.ts match cascade); goose source (developer extension).
- Agent Client Protocol (agentclientprotocol.com) — cancellation and
  permission-request semantics.
