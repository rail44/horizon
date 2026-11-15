# Sandbox Area Grants

Status: proposed 2026-11-15, owner decision (a): the main checkout becomes
writable through an approved area grant, not by a special case. Supersedes
`ApprovalKind::GitOperation`'s up-front derivation as the *primary* way a
session widens its sandbox; the grant-shape exclusions Git introduced
(`hooks`, `config`) survive as a property of *any* grant that covers them.

This document is a design record. Nothing here is implemented yet.

## Problem

A session's sandbox has exactly one writable root: its workspace root
(`crates/horizon-agent/src/tools/bash/exec/sandboxed/prepare.rs:85-90`,
`writable_roots: vec![workspace_root.to_path_buf()]`). Everything else is
readable and unwritable, enforced by the kernel (Landlock/seccomp), not by
an approval check.

Today a session can widen that range in exactly three ways, and all three
are **reactions**, never requests:

1. a `bash` call is denied, and the daemon folds a
   `FilesystemDenialRetry` candidate (`horizon-agentd/src/session/completion/retry.rs:130-190`);
2. a `bash` command *looks like* it will write Git metadata — derived
   before execution from the command text
   (`crates/horizon-agent/src/tools/bash/git.rs:230-271`,
   `policy.rs:65-68`) — and a `GitOperation` candidate is raised;
3. the owner pre-authorises trees in `[[grants.project]]`
   (`crates/horizon-config/src/grants.rs`).

The second is the only one that guesses *before* running, and it guesses
from shell syntax. It is where the design has accreted: a per-segment
recognizer, a prefilter that has already been retired once
(`docs/agent-approval-design.md`, board #38), a git-only judge prompt
(`crates/horizon-agent/src/judge/request.rs:59-83`), and a grant shape
that carries git-specific exclusions
(`crates/horizon-agent/src/tools/bash/git.rs:370-415`).

### Evidence from the owner's real event log

`~/.local/share/horizon/agent-events.jsonl` (703 MB, all sessions to
2026-11-15) parsed for approval kinds:

| kind | count |
|---|---|
| `Standard` | 145 |
| `GitOperation` | 137 |
| `DomainDenialRetry` | 41 |
| `FilesystemDenialRetry` | 36 |
| `DomainGrant` | 5 |

`GitOperation` is the **largest non-Standard category** — the syntax
guess is not an edge case, it is the normal way sessions widen their
sandbox.

Of those 137, 28 run commands that write the **working tree**, not only
metadata:

- `cd ~/src/horizon && git merge --ff-only horizon/cedb7609`
- `cd ~/src/horizon && git checkout -f`
- `cd ~/src/horizon && git update-ref refs/heads/main 779e2ed7 a3521c17`
- `…/worktrees/fe74bcea && git rebase -i db0d334f`
- `cd ~/src/horizon && git checkout -- crates/horizon-agent/…`

These are `GitOperation` approvals whose real need was **a writable
working tree**, which a git-metadata grant cannot provide. The mismatch is
not hypothetical: on 2026-11-15 a session approved a `git merge --ff-only`
via `GitOperation`, the sandbox refused every working-tree file
(`unable to unlink old 'src/main.rs': Permission denied`), and the merge
had to be finished by the owner by hand. The prompt said "allow write
access to `.git/…`"; the command needed more than that.

## Decision

Introduce **area grants**: a session may *ask* for a writable area, before
anything fails, and receives it only through the ordinary approval
pipeline.

### The tool

```
request_sandbox_area(path, access, reason)
```

The tool **does not grant anything**. Its handler returns
`Execution::AwaitApproval(ApprovalKind::SandboxAreaGrant { … })`, exactly
as a denial-folded candidate would. The approval boundary stays in one
place — `resolve_*` in `crates/horizon-agent/src/tools/approval.rs` — and
`ToolSessionState::approve_filesystem_grants`
(`crates/horizon-agent/src/tools/state.rs:451-465`) keeps its single
sanctioned caller path.

This is the constraint the whole design rests on: a tool that both asks
and receives would collapse the boundary documented in
`approval.rs:301-306` ("an approval buys scoped, contained access, never
an unconfined execution") and `state.rs:78-90`.

### The approval kind

```
ApprovalKind::SandboxAreaGrant {
    grants: Vec<FilesystemGrant>,
    reason: String,
}
```

Approval appends `grants` to the session's grant list (the existing
`approve_filesystem_grants`), and the session's later calls — `bash`,
`fs_write`, `fs_edit` — see them through the normal snapshot. Nothing is
re-run: the point of asking first is that there is no failed call to
retry.

Clamping is mandatory and is the same function the denial path uses:
`horizon_sandbox::suggest_grants` / `revalidate_grant`
(`crates/horizon-sandbox/src/grant.rs:186-238`, `:108-156`), so `$HOME`,
`/`, and the system roots stay ungrantable
(`is_overbroad_tree`, `grant.rs:78-90`).

### The main checkout

Owner decision (a): the main checkout **is** grantable as an area, and
writes to it stay approval-gated. This matches what sessions have actually
done (28 of 137 GitOperation approvals above); the rule being replaced —
"a session never writes the main checkout" — was never enforced as such,
it was merely an unstated consequence of the main checkout falling outside
`writable_roots`.

What changes for the primary checkout flag: today
`primary_checkout` is set only for a session whose root *is* the primary
checkout (`horizon-agentd/src/session/environment.rs:163-178`), and
`prepare.rs:77-84` then makes the root exclude itself until approved. An
area grant on the main checkout must feed that same flag, so that the
self-exclusion lifts exactly when an approved area covers it and at no
other time.

### Exclusions survive, and stop being git's

`.git/hooks` executes on the next host-side git invocation, and
`.git/config` can set `core.fsmonitor`, `core.pager`, and credential
helpers — all host-side execution
(`git.rs:370-378`, board #38,
`docs/research/shell-approval-evidence-2026-08-17.md`). Any area grant
that covers a repository's metadata must carry those exclusions.

The **rule** is not git-specific; only the **discovery** of which paths
are metadata is (`git_metadata_paths`, `exclude_git_metadata` in
`tools/bash/exec/sandboxed/result.rs:161-197`). That discovery stays. The
`GitOperation` approval *kind* does not.

## What is removed

Once `SandboxAreaGrant` exists, these have no remaining job:

- `ApprovalKind::GitOperation` and `resolve_git_operation`
  (`approval.rs:366-417`);
- the pre-execution recognizer `command_requires_metadata_write` /
  `segment_requires_metadata_write` (`git.rs:230-271`) and the policy arm
  that classifies by it (`policy.rs:65-68`);
- `metadata_writable_roots`' role as an up-front derivation
  (`git.rs:289-368`) — the agent asks for the area instead;
- `SandboxedApprovalOrigin::ManualGitOperation` and its no-op annotator
  (`tools/bash/job.rs:174`);
- the git-only judge prompt `GIT_INSTRUCTIONS`
  (`judge/request.rs:59-83`) and the `git_metadata_operation` request
  field, replaced by the grant list the judge already receives.

A `bash` call that writes git metadata then needs no special
classification at all: it is `Contained` like any other, and if it needs an
area the session did not have, the ordinary denial path folds a
`FilesystemDenialRetry` — or the session asked first. The two paths meet
in the same grant list.

**Wire impact:** `GitOperation` is on the ACP extension wire
(`crates/horizon-acp`, `contract.rs:1072`). Removing it is a protocol
change and needs `HORIZON_ACP_EXT_VERSION` bumped, which per `AGENTS.md`
requires a full Horizon restart rather than a runtime reload. The change
should be additive first (add `SandboxAreaGrant`, keep `GitOperation`
deserializable) and remove it in a later release.

## Audit

Every area grant records who approved it. Today `approval_source` is
written by two annotators only (`tools/output/annotations.rs:102,111`);
`GitOperation`, `DomainDenialRetry`, `DomainGrant`, and
`MachServiceGrant` record nothing. That is a gap against invariant 7
("Audit") of `docs/containment-denial-narrow-grants-design.md`.

The fix is not per-kind: `dispatch_approval`
(`tools/approval.rs:196-214`) receives `approval_source` for every kind
already (`:91` human, `:139` judge) and each arm decides whether to use
it. The record moves to one place — the approval's exit — so every kind,
including the new one, carries `approval_source` and `approval_scope=area_grant`.
A single test iterates all `ApprovalKind` variants and asserts the field
is set.

## Open questions

1. **Lifetime.** Existing grants last for the session and persist across
   resume and daemon restart
   (`horizon-agent/src/live.rs:296-300`, `resume.rs:448-455`). Area grants
   should match, but "the session asked for `$HOME/src` once" surviving a
   resume is a stronger commitment than a denial retry and should be
   stated in the prompt.
2. **Prompt wording.** A denial retry says "retry the same call"
   (`retry.rs:174-178`). An area request has no call to retry; it must say
   what the area buys and for how long.
3. **Base rate.** With area grants the session will ask *before* running,
   so the judge sees a grant list rather than a command. Whether that
   reads better or worse to the judge is unmeasured; the 137 GitOperation
   verdicts in the log are the comparison point.
4. **Ordering with removal.** Whether `GitOperation` is removed in the
   same release as `SandboxAreaGrant` lands, or after a period where both
   exist and the log shows which one sessions choose.
