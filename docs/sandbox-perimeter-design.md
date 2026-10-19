# Containment Perimeter and the Worktree Tool — Design

Status: decided on board #65 (2026-10-18); slices 1 and 2 implemented
(see "Staging"). This document describes the current design, the target
design, and the code the change touches. The decisions themselves and
the measurements that motivated them are recorded on board #65; this
page does not restate them as policy.

## Summary

With slice 1 in place, the sandbox is the perimeter and isolation is no
longer a policy input: `bash` runs sandboxed and automatic in every
session when the OS sandbox is available, `fs.write`/`fs.edit` are
automatic inside the workspace root, and approvals exist only for
crossings of the containment boundary (git metadata, paths outside the
root, network). Host execution remains only as the fallback on a host
without a working sandbox.

With slice 2, a session that wants a disposable copy of the repository
asks for one with a tool, `worktree.enter`. The remaining target is one
session kind: the spawn-time isolation choice goes away.

## Current design

- **Classification** (`crates/horizon-agent/src/policy.rs`,
  `classify_input`): the table under "One session kind, sandbox as the
  perimeter" below. An `fs.write`/`fs.edit` target inside the workspace
  root's `.git` is not automatic: `plan_prepared_call` raises a
  `Standard` approval for it before classification.
- **The `AlwaysAsk` outcome for `bash` without a sandbox** is a
  `Standard` approval whose effect is one host-authority run: the result
  records `sandboxed: false`, `approval_scope: host_execution_once`.
- **Sandbox policy** (`tools/bash/exec/sandboxed/prepare.rs`):
  `writable_roots = [workspace_root]` with
  `writable_root_exclusions = [workspace_root/.git]` when that entry
  exists, `ReadableScope::Full`, network through the session proxy
  (started for every session when the sandbox is available) or disabled,
  plus the session's filesystem grants (`[grants.project]` trees,
  judge/human-approved grants).
- **Isolation is a spawn-time attribute**
  (`docs/session-relationship-design.md`, decision 3): palette origin
  defaults to shared, CLI origin defaults to isolated, with an explicit
  override (`isolate` in the ACP `_meta.horizon` session request, the
  CLI's `--share`, the view chooser's "Agent (Isolated Worktree)…"
  choice). The predicate is persisted in every event-log record's
  `session_context.isolated_worktree`; restore re-adopts the worktree
  (`session/resume.rs`, `session/resume/environment.rs`) before tier-1
  eligibility returns.
- **The gitdir boundary**: a linked worktree's gitdir lives under the
  main checkout's `.git/worktrees/<id>`, outside the worktree's writable
  root; a plain checkout's `.git/` is inside its root and excluded through
  `writable_root_exclusions`. Either way `git commit` and friends need the
  `GitOperation` approval
  (`docs/containment-denial-narrow-grants-design.md`), whose grant on the
  gitdir lifts the exclusion for that run and keeps `hooks/` and `config`
  excluded.
- **Enforcement of the exclusion** is the same as for a grant's
  `excluded_subpaths`: on Linux the seccomp supervisor denies write opens
  (`openat`/`openat2`) beneath the excluded path; Landlock still allows
  the root as a whole, so calls the supervisor does not mediate (`rename`,
  `unlink`, `mkdir`, `link`, `symlink`) are not refused there. On macOS
  the exclusion is advisory.

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

Mechanism: `SandboxPolicy.writable_root_exclusions`, a list of absolute
paths inside the writable roots, enforced like
`FilesystemGrant.excluded_subpaths` (the seccomp supervisor on Linux —
`crates/horizon-sandbox/src/linux/mod.rs`; advisory on macOS). A
`ReadWrite` `DirectoryTree` grant at or above an excluded path lifts that
exclusion for the run; the grant's own `excluded_subpaths` stay absolute.

Git metadata writes keep the existing `GitOperation` path.
`tools/bash/git.rs::metadata_writable_roots` already handles a plain
checkout (a `.git` directory yields that directory as the metadata root),
and the approved call runs sandboxed with that grant and the existing
`hooks/`/`config` exclusions.

### `worktree.enter`

A built-in tool, `worktree.enter`, in the `<domain>.<verb>` vocabulary
of `fs.read`, `board.update`, `workspace.snapshot`. The domain is git's
own term; `workspace.*` is taken by the shell's tab/pane structure.

- **Input**: none (`tools/input`, the `Empty` input type).
- **Approval**: automatic (`AutoAllowRead`), like `workspace.snapshot` and
  `board.read`: a fixed, validated operation performed by the daemon, not
  an arbitrary command.
- **Execution**: the `HostTools::enter_worktree` boundary
  (`tools/execution.rs`), implemented by `horizon-agentd`'s
  `session/host_tools.rs`. It refuses a session whose entry already owns a
  worktree, resolves the session root's `HEAD` to a commit, and sends
  `Command::ActivateWorktree { base }` to the calling session: the same
  command the board's `implement` action sends, without the task
  ownership check.
- **Result**: `{requested: true, base, message}`. The worktree does not
  exist yet when the call returns.
- **Activation**: the existing environment activation path. The provider
  queues the command and, at its next turn boundary, emits
  `EnvironmentReady { base }`; `SessionEnvironment::activate`
  (`session/environment.rs`) waits for tool work to settle, creates
  `<repo>/.horizon/worktrees/<slug>` on branch `horizon/<slug>` at `base`
  (`create_isolated_worktree_at`), prepares a replacement tool environment
  with `isolated: true`, persists `EnvironmentActivated`, re-registers the
  session runtime, and answers `EnvironmentPrepared { workspace_root,
  branch, trusted_project }` or `EnvironmentActivationFailed`.
- **What the model sees**: on `EnvironmentPrepared` the provider rebuilds
  its environment prompt sections and queues a note naming the new root,
  branch, and base; on failure it queues the failure message. Either note
  reaches the model on its next turn.
- **Not in v1**: entering an existing worktree by path, and the inverse
  (`worktree.leave`). They are added when a flow needs them.

What follows the root on activation:

| state | mechanism |
|---|---|
| `ToolSessionState` root (`tools/state.rs`) | replaced by the prepared environment |
| sandbox `writable_roots` | derived per call from the root |
| `bash` tracked cwd | starts at the new root in the replacement tool state |
| system prompt environment section | rebuilt by the provider on `EnvironmentPrepared` |
| shell-facing root (`WorkspaceRootResolved`) | sent on activation, as at setup and attachment replay; the shell maps it to `session_info_update` |
| persisted `session_context` | `workspace_root` is the worktree and `isolated_worktree` is true from `EnvironmentActivated` onward |
| session entry `worktree`, terminate cleanup (`git worktree remove`) | recorded on activation; cleanup unchanged |

### Restore

Restore is unchanged by slice 2. Activation persists the isolation
predicate (`session_context.isolated_worktree`) and the worktree identity
(`EnvironmentActivated`), so a restarted daemon re-adopts the worktree
through the existing path (`session/resume.rs`,
`session/resume/environment.rs`, `restore_worktree`). Whether the
predicate is retired is part of slice 3.

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
2. **`worktree.enter`.** The tool and the mid-session root change over
   the existing environment activation path; restore needs no change.
   The spawn-time isolation still exists in this slice, so both ways of
   getting a worktree coexist briefly.
3. **Spawn surface removal.** `isolate` leaves the ACP meta, CLI,
   control plane, shell, and chooser; the wire version bumps; docs and
   the dogfood skill are updated.

Status: slice 1 landed 2026-10-18.

Status: slice 2 landed 2026-10-19.

Slice 1 is independent and smallest. Slice 3 is last because it bumps
the wire and retires the spawn-time isolation that delegation still uses
until slice 2 has landed.
