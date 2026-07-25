---
id: 009
title: Build verification in an isolated worktree non-deterministically demands human approval
status: open
severity: high
area: agent, sandbox
---

## Repro

1. Spawn three isolated agent sessions with the *same* implementation brief,
   one after another (`horizon new-agent --prompt ...`, default isolation).
   The brief used was a two-line edit to `crates/horizon-agent/src/tools/
   catalog.rs` ending in "Verify the crate still builds".
2. Let each run unattended.
3. Compare the `bash` tool result markers for the `cargo check -p
   horizon-agent` call each session makes.

## Observed

All three sessions issued an equivalent `cargo check` inside their own
worktree, all three had it refused by the sandbox, and all three fell back to
the retry-on-host path. **The retry was auto-approved in two runs and required
a human in the third.**

| run | session | bash result markers |
|---|---|---|
| 1 | `9087e6ce` | `sandboxed=false auto_approved=true`, no approval event |
| 2 | `0bb46ac7` | `sandboxed=false`, **`approval_requested` emitted**, `auto_approved` absent |
| 3 | `8ea4f070` | `sandboxed=false auto_approved=true`, no approval event |

Run 2's approval reason:

```
`bash` crossed the filesystem sandbox boundary: attempted
/home/satoshi/.cargo/.package-cache (suggested ReadWrite File access to
/home/satoshi/.cargo/.package-cache); attempted
/home/satoshi/.cargo/.global-cache ...
```

Run 2 sat in `WaitingForApproval` until a human approved it. Nothing in the
brief, the worktree, or the command differed between the runs.

## Expected

The same verification command, issued by the same kind of session against the
same repository, should reach the same approval decision every time. Whatever
the decision is — auto-approve the CARGO_HOME retry or always ask — an
unattended agent should not stall on a routine build in one run out of three.

## Notes

- Frequency: 1 of 3 runs of an identical brief, same day, same build
  (`162967f`). Also observed once on a differently-shaped brief in the same
  batch.
- This is not only an approval-UX problem: an agent left running unattended
  stops making progress entirely, so the failure mode is a silent stall
  rather than a visible error.
- `cargo` writes under `CARGO_HOME` on essentially every invocation, and this
  repository additionally shares one `build.build-dir` under `CARGO_HOME`
  across every worktree (see `AGENTS.md`, "Build setup"), so build
  verification always crosses the worktree boundary.
- The *set* of attempted paths differs between refusals, and so does the
  **scope of the grant the refusal suggests**. Three captured in one batch:

  | command | attempted paths | suggested grant |
  |---|---|---|
  | `cargo check -p horizon-agent` | `.cargo/.package-cache`, `.cargo/.global-cache` | ReadWrite **File** per path |
  | `cargo test -p horizon-agent` | `.cargo/.package-cache-mutate`, `.cargo/horizon-build-dir/debug/.cargo-build-lock` | ReadWrite **File** per path |
  | `cargo nextest run -p horizon-agent` | `.cargo/.global-cache-journal`, `.cargo/.package-cache-mutate`, `.cargo/horizon-build-dir/debug/.cargo-build-lock` | ReadWrite **DirectoryTree** on `/home/satoshi/.cargo` (+ two File grants) |

  The third escalates from individual lock files to the whole `CARGO_HOME`
  tree. Whether this variation is what drives the differing approval decision
  was not investigated — that is triage's job, not this report's.
- Across the batch, **three of the five sessions that attempted build
  verification stalled** (`0bb46ac7` on `cargo check`, `7bc279e3` on
  `cargo test`, `749e79fa` on `cargo nextest run`); two auto-approved.
- Filed from a dogfooding batch run for the agent context-consumption work
  (`docs/research/agent-read-navigation-prior-art-2026-07-25.md`).

## Additional observations (2026-07-25, later sessions)

- A fourth distinct path set: `cargo clippy --workspace` crossed on
  `~/.cargo/horizon-build-dir/.rustc_info.json` alone.
- One command can stall **more than once**: a `cargo test` run was
  approved for `.cargo/.package-cache-mutate`, and its retry then stalled
  again on `.cargo/horizon-build-dir/debug/.fingerprint/horizon-agent-
  <hash>/lib-horizon_agent`. Fingerprint paths embed a metadata hash that
  changes whenever the crate's source changes, so **per-path File grants
  can never converge for cargo** — the next edit invalidates the grant.
- Routing the gate through `hooks/pre-commit` needs one approval for the
  whole run; invoking `cargo check`/`clippy`/`nextest` separately needs
  one per step (three observed).
- The sandbox's own suggestions already escalate to `ReadWrite
  DirectoryTree` on `~/.cargo` in some refusals; the approval granularity
  just doesn't follow. Tree-level grants for `CARGO_HOME` + the shared
  build dir look like the only shape that converges.
