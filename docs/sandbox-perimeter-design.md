# Containment Perimeter and the Worktree Tool — Design

Status: decided on board #65 (2026-10-18), not yet implemented. This
document describes the current design, the target design, and the code
the change touches. The decisions themselves and the measurements that
motivated them are recorded on board #65; this page does not restate
them as policy.

## Summary

Today a `bash` or `fs.write`/`fs.edit` call is auto-approved only inside
a session that was *spawned* isolated (its own worktree) and only when
the OS sandbox is available. Every other session asks a human for each
call, and an approved `bash` call then runs **outside** the sandbox with
the host process's authority.

The target design makes the sandbox the perimeter and drops isolation
from the policy inputs: there is one session kind, `bash` always runs
sandboxed, and approvals exist only for crossings of the containment
boundary (git metadata, paths outside the root, network). A session that
wants a disposable copy of the repository asks for one with a tool,
`worktree.enter`, instead of being spawned as a different kind of
session.

## Current design (as of 2026-10-18)

- **Classification** (`crates/horizon-agent/src/policy.rs`,
  `classify_input`): `bash` is `Contained` only when
  `session_isolated && sandbox_available` and the command writes no git
  metadata; `fs.write`/`fs.edit` are `Contained` only when
  `session_isolated`. Everything else is `AlwaysAsk`.
- **The `AlwaysAsk` outcome for `bash`** is a `Standard` approval whose
  effect is one host-authority run: the result records
  `sandboxed: false`, `approval_scope: host_execution_once`.
- **Sandbox policy** (`tools/bash/exec/sandboxed/prepare.rs`):
  `writable_roots = [workspace_root]`, `ReadableScope::Full`, network
  through the session proxy or disabled, plus the session's filesystem
  grants (`[grants.project]` trees, judge/human-approved grants).
- **Isolation is a spawn-time attribute**
  (`docs/session-relationship-design.md`, decision 3): palette origin
  defaults to shared, CLI origin defaults to isolated, with an explicit
  override (`isolate` in the ACP `_meta.horizon` session request, the
  CLI's `--share`, the view chooser's "Agent (Isolated Worktree)…"
  choice). The predicate is persisted in every event-log record's
  `session_context.isolated_worktree`; restore re-adopts the worktree
  (`session/resume.rs`, `session/resume/environment.rs`) before tier-1
  eligibility returns.
- **Why the gitdir boundary holds today only for isolated sessions**: a
  linked worktree's gitdir lives under the main checkout's
  `.git/worktrees/<id>`, outside the worktree's writable root, so
  `git commit` and friends need the `GitOperation` approval
  (`docs/containment-denial-narrow-grants-design.md`). A plain checkout
  has `.git/` *inside* its root; under the current policy a sandboxed
  command there could write hooks and config.

## Target design

### One session kind, sandbox as the perimeter

`classify_input` loses its `session_isolated` input:

| tool | sandbox available | classification |
|---|---|---|
| `bash`, no git metadata write | yes | `Contained` → `SandboxedBash` |
| `bash`, git metadata write | yes | `AlwaysAsk` → `GitOperation` approval (unchanged path) |
| `bash` | no | `AlwaysAsk` → `Standard` host-execution approval (fallback only) |
| `fs.write`, `fs.edit` | either | `Contained` |

`fs.write`/`fs.edit` do not use the OS sandbox; their confinement to
`workspace_root` is the tool's own path check (`tools/state.rs`), so
their classification does not depend on sandbox availability. The
host-execution approval survives only where `horizon_sandbox::is_available()`
is false; on a host with a working sandbox the approval kinds that can
reach a human are `GitOperation`, outside-root filesystem paths,
`DomainGrant`/web, and `FilesystemGrant` denial retries.

### Writable range: the working tree, never the gitdir

A sandboxed command may write anywhere under `workspace_root` except the
repository's gitdir. For a linked worktree this is already the layout
(the gitdir is outside the root). For a plain checkout the policy must
exclude `.git/` explicitly.

Mechanism: `horizon_sandbox::FilesystemGrant` already carries
`excluded_subpaths` (enforced by the seccomp supervisor on Linux —
`crates/horizon-sandbox/src/linux/mod.rs`; advisory on macOS, see
`policy.rs`). `SandboxPolicy.writable_roots` has no exclusion list. Two
implementation shapes fit: express the root itself as a `DirectoryTree`
grant with `excluded_subpaths: [".git"]`, or add an exclusion list to
`SandboxPolicy`'s roots. The second keeps "root" and "grant" distinct in
evidence and denial reports and is the lean here; the choice is an
implementation decision.

Git metadata writes keep the existing `GitOperation` path.
`tools/bash/git.rs::metadata_writable_roots` already handles a plain
checkout (a `.git` directory yields that directory as the metadata root),
and the approved call runs sandboxed with that grant and the existing
`hooks/`/`config` exclusions.

### `worktree.enter`

A built-in tool, `worktree.enter`, in the `<domain>.<verb>` vocabulary
of `fs.read`, `board.update`, `workspace.snapshot`. The domain is git's
own term; `workspace.*` is taken by the shell's tab/pane structure.

- **Input**: none in v1.
- **Effect**: create a worktree branched from the session's current
  `HEAD` at `<repo>/.horizon/worktrees/<session id>` (today's
  `create_isolated_worktree` in `crates/horizon-agentd/src/worktree.rs`)
  and move the session's workspace root to it.
- **Approval**: automatic, like `workspace.snapshot` and `board.read` —
  a fixed, validated operation performed by the daemon, not an arbitrary
  command.
- **Result**: the new root, branch name, and base commit, so the receipt
  shows where the session now works.
- **Not in v1**: entering an existing worktree by path, and the inverse
  (`worktree.leave`). They are added when a flow needs them.

Moving the root mid-session is a new path. What follows the root today
and what must be made to follow it:

| state | today | target |
|---|---|---|
| `ToolSessionState` root (`tools/state.rs`, `Inner.workspace_root`) | fixed at build | replaced through the environment activation path (`session/environment.rs`, "activate a replacement durably") |
| sandbox `writable_roots` | derived per call from the root | follows automatically |
| `bash` tracked cwd | persisted across calls | reset to the new root on enter |
| system prompt environment section | built once at spawn (`providers::rig::session::session_extra_sections`) | re-announced through the existing `Event::EnvironmentActivated` handshake |
| shell-facing root (`WorkspaceRootResolved`) | sent once at setup / attachment replay | re-sent on enter (pane title abbreviation, "open its directory") |
| persisted `session_context.workspace_root` | written per record | updated from the enter onward |
| lineage edge, terminate cleanup (`git worktree remove`, decision 5) | recorded at isolated spawn | recorded when the tool creates the worktree; cleanup unchanged |

### Restore without the isolation predicate

`session_context.isolated_worktree` is retired. Restore keys on the
persisted root: a root under `.horizon/worktrees/` is re-adopted with
`adopt_isolated_worktree` (which already validates that the worktree
belongs to this session and refuses an ordinary checkout); any other root
is restored as is. The `EnvironmentActivated` identity path
(`restore_worktree`) is unchanged. Dropping a field from the persisted
context is an event-log record change; existing records keep the field
and readers ignore it.

### Spawn surface

The `isolate` knob goes away everywhere it is plumbed: the ACP
`_meta.horizon` session request (`crates/horizon-acp/src/meta.rs`), the
CLI (`new-agent --share`), the control-plane dispatcher
(`src/control_plane.rs`), `PendingAgentSpawn.isolate`
(`src/workspace/session_creation.rs`), and the view chooser's dedicated
choice (`src/view_chooser.rs`). The remaining spawn-time knob is the
directory to start in (decision 3's *workspace_root source*: inherit the
source pane's cwd). Removing the ACP field is a non-additive change to
the `_meta.horizon` vocabulary and bumps `HORIZON_ACP_EXT_VERSION`
(AGENTS.md: full Horizon restart, not a runtime reload).

Delegation changes shape accordingly: a parent spawns a child in a
directory, and a child that needs a disposable copy calls
`worktree.enter` as its first step. Nothing is spawned "isolated".

### Documents and skills this supersedes in part

- `docs/agent-approval-design.md`: the three tiers' isolation wording,
  the "Non-isolated sessions keep today's per-action gate" paragraph, and
  the classification bullet of the 2026-07-19 consultation.
- `docs/session-relationship-design.md`: decision 3's isolation knob and
  its origin-based default; decisions 4 and 5 stand.
- `docs/cli-control-plane-design.md`: the `isolate` origin default.
- `.claude/skills/horizon-dogfood/SKILL.md`: "spawn isolated agents" and
  the `--share` control case.

## Staging

Three slices, each landing on its own:

1. **Perimeter without the tool.** `classify_input` drops
   `session_isolated`; the gitdir exclusion lands in the sandbox policy;
   host execution becomes the sandbox-unavailable fallback. This alone
   removes the failure mode measured on board #65 for sessions started in
   the main checkout. No wire change.
2. **`worktree.enter`.** The tool, the mid-session root change, and
   restore by persisted root. The spawn-time isolation still exists in
   this slice, so both ways of getting a worktree coexist briefly.
3. **Spawn surface removal.** `isolate` leaves the ACP meta, CLI,
   control plane, shell, and chooser; the wire version bumps; docs and
   the dogfood skill are updated.

Slice 1 is independent and smallest. Slice 3 is last because it bumps
the wire and retires the spawn-time isolation that delegation still uses
until slice 2 has landed.
