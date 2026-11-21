# Subagents — One Delegation Tool, Read and Write

Decided 2026-11-20 in an owner consultation, on the measurements and prior
art in [the subagent research record](research/agent-subagent-prior-art-2026-11-20.md).
This document is self-contained: it is the scope record for implementing
the change.

It supersedes, where they conflict:

- `docs/agent-explore-design.md` decision 7 (a 25-request cap for
  exploration sessions) and the 2026-07-27 addendum's removal of follow-up
  turns;
- `docs/agent-async-task-design.md` decision 5's "per-child iteration cap
  25" and decision 7 ("v1 children stay read-only", with write-capable
  roles left to a later round — this is that round).

The owner set aside the earlier recorded designs for this round: 「規定の
設計は特に気にしなくてよいです」.

## Problem

`task` delegates one question to a read-only child session (role
`explore`) and folds back its final report. In the week of 2026-11-13 to
2026-11-20 no exploration finished normally: 13 of 14 stopped at the
25-request cap and one at a 120 s stream timeout. Every capped run still
returned a report (the forced summary), but 6 of 11 said they were
partial, and the requester had to read on itself. The owner's reading:
「続きを読む必要が生じている時点で機能できていないと思いますが」. The
failure was also invisible: only a `capped: true` field in the result
marked it, and the owner had not noticed.

The cap counted requests, while what it was meant to protect is the
context: capped children stood at a median of 53k and at most 121k input
tokens. The requests were broad (four or five questions, "exhaustive
inventories") although the tool description already asks for several
narrow tasks over one broad one, and the design's expected recovery — the
requester asking something narrower — happened zero times. A child also
could not read Git history, having no shell.

Horizon has no write-capable child at all.

## Prior art in one paragraph

Most harnesses now have one subagent mechanism, and their "explore" is
that mechanism with fewer permissions and a search-oriented prompt
(OpenCode's `explore` vs `general`, Codex's empty `explorer` role, Crush's
`access: read | write`, Amp folding its search agent into skills on the
general Task subagent). Few cap a child's steps by default (OpenCode,
Codex, Crush, Cursor, Amp: none; Hermes: 250; Gemini's investigator: 50
turns), and those that stop a child tell the parent it stopped early only
sometimes — Hermes most explicitly. Write-capable children share the
parent's working tree by default everywhere surveyed, with a worktree as
an option in Claude Code, Hermes and Cursor; the owner's own Claude Code
use ran essentially every writing child in a worktree. Benchmarks that
hold the model fixed show small and inconsistent success differences
between harnesses, so they do not single out a design to copy.

## Decisions

Owner direction for the round: 「この方向で考えましょう」 (one subagent
mechanism, with exploration as its read-only mode), then 「全て推奨通りで
よいです」 for the points below that carry no quote of their own.

1. **One tool, two modes.** `task` keeps its name and gains
   `access: "read" | "write"`, `read` by default. The `explore` role
   becomes the read mode; a write-mode child is the same mechanism with
   write tools. Modes differ in tools, sandbox and prompt only.

2. **No budget.** 「上限は無制限でよいと思いますね」. A child has no
   request, turn, token or time cap. What stops a child is what stops any
   session: completion, the doom-loop guard, provider failure, its own
   context handling (Tier 1 clearing), or cancellation. The
   explore-only iteration cap and the forced summary on it go away.

3. **A write child works in its own worktree.** 「そうですね。そうしましょう」
   (after the count of the owner's Claude Code children above). The
   worktree is created with the existing session-worktree machinery
   (`crates/horizon-agentd/src/worktree.rs`, under
   `.horizon/worktrees/`), branched from the requester's current `HEAD`.
   A requester with uncommitted changes is told to commit before it
   launches a write child, so the child starts from what the requester
   sees. The child commits on its own branch; the requester reviews and
   integrates that branch. A child that made no commits and left its tree
   clean has its worktree removed when it ends; otherwise the worktree
   stays.

4. **Approvals go to the human through the requester.** Every child's
   approval request — read or write mode — is shown in the requester's
   pane, naming the child it comes from, and passes the same gate as the
   requester's own calls. A child starts with the requester's grants
   (decision 12). Read children are no longer unattended.

5. **Follow-up returns.** The requester can send another instruction to
   an existing child by its session id; the child continues with its
   history. This was removed on 2026-07-27 because it got in the way of
   the turn-folding work then under way
   (「委譲についてforkと会話継続を削りましょうか。動作を直しつつ(ターン畳み込みの)
   設計を相談するうえで邪魔になるので」); that work has since landed as
   the asynchronous `task`.

6. **A child that did not finish says so.** The result carries how the
   child ended — completed, or stopped and why (doom loop, provider
   failure, cancellation) — and the requester's notification and the
   task row in the agent view show an unfinished child as unfinished.
   This replaces the `capped` marker.

7. **The read mode has a read-only shell.** A read child gets `bash`,
   sandboxed with no writable root, so it can run `git log`/`git show` and
   other read-only commands while the operating system, not the prompt,
   keeps it from writing. With no writable root the sandbox provisions no
   scratch directory either, and grants that write (a project's
   configured trees among them) are dropped for it. A refused write comes
   back as ungrantable, never as a grant offer.

8. **The model is inherited** from the requester, as now.

9. **When to use each mode is stated per mode** in the tool description
   and the delegation prompt section: read for independent questions and
   whenever only the conclusion is wanted, not the file contents; write
   for a self-contained piece of work. Whether to narrow this toward
   "only when asked" is left until the new shape has been used.

### Decided 2026-11-21, for the write mode

Settled when stage 4 was reached, on how Claude Code does the same
(its sandbox lets a command in a linked worktree write the repository's
shared `.git`, except `hooks/` and `config`, so `git commit` works; a
worktree subagent commits on its branch and nothing merges it) and on
the owner's own use: 50 of the 53 worktree children in the owner's
Claude Code transcripts of 2026-10 and 2026-11 committed on their branch,
and the main session merged a worker's branch 19 times.

10. **A child's work comes back as its branch, and a session writes its
    own worktree's Git metadata without asking.** 「Aで」. The write
    child commits on its branch; the requester reviews the branch and
    integrates it. Committing wrote Git metadata only through an
    approved area (`docs/sandbox-area-grants-design.md`), and a session
    working in its own worktree was asked several times per commit --
    four approvals over 32 minutes for one commit on 2026-11-19 -- so a
    session in a worktree Horizon created for it may write that
    worktree's own metadata with no approval: its gitdir
    (`.git/worktrees/<slug>`), its own branch's ref and reflog, and the
    object store. 「推奨でよいです」 for this narrower range over Claude
    Code's whole shared `.git`: another branch's ref stays unwritable,
    and `hooks/` and `config` stay unwritable as everywhere. This holds
    for any session in its own worktree, not only a child, so a requester
    working in its own worktree integrates a child's branch without
    asking either; a requester in the primary checkout is asked as
    before.

11. **A daemon restart ends a write child and keeps its work.** 「iで」.
    The child is terminated rather than resumed, as an exploration child
    is (`docs/agent-explore-design.md` decision 8); its worktree and
    branch are kept when it made commits or left changes, and its
    requester is told which branch holds them. The child records its
    requester as its parent so that the requester can be told after the
    restart.

12. **A child starts with a copy of its requester's grants.** 「推奨で」.
    At launch the child receives the requester's filesystem grants and
    allowed domains as they are then; what the child is granted later
    does not flow back. The primary-checkout approval does not apply: a
    write child works in its own worktree.

## Removed

- `EXPLORE_ITERATION_CAP`, and `RoleDefinition::summarize_on_cap` with the
  forced summary, whose only user was the explore role.
- The explore role's `unattended` flag.
- The `capped` result marker (replaced by decision 6).

## Open — to settle while implementing

These are not owner decisions yet; each is raised when its stage is
reached.

- The wording of the per-mode guidance (decision 9).

## Proposed order

1. Read mode on the new shape: remove the cap, add the read-only shell,
   report how a child ended and show it (decisions 2, 6, 7). The `access`
   parameter of decision 1 arrives with the write mode, when there is a
   second value for it to take.
2. Follow-up (decision 5).
3. Approvals through the requester (decision 4). A child starting with
   the requester's grants moves to stage 4: a read child drops every grant
   that writes, so it is the write child the grants matter to.
4. Write mode in its own worktree (decisions 1, 3, 10, 11, 12).
