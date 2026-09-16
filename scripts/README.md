# Board flow integration check

**Status, 2026-09-16:** the replacement fixture is written. Matching workspace
build, final gate and real-daemon fixture results remain to be recorded during
integration validation. No live migration is performed by this script.

Build matching binaries from this checkout before running:

```sh
cargo build --workspace
python scripts/check-board-flow.py --bin-dir target/debug
```

The check requires host permission to bind localhost/Unix sockets, start daemons
and create Git worktrees. It does not run as part of the sandboxed nextest
profile. A fixture result is valid only after an actual successful invocation;
Python syntax validation alone does not verify the runtime behavior.

The deterministic local provider exercises:

- Automatic task registration, priority ordering and prerequisites.
- Consultation before worktree creation and same-session implementation after
  a settled daemon restart and owner response.
- Activation at an explicit Git base, two implementation commits, independent
  review, correction, and re-review of the resulting three-commit change. Each
  request creates a fresh ordinary reviewer session and a separate exact-tip
  worktree; deliberate uncommitted task changes must not enter either snapshot.
- The fixture project's direct-integration policy and prerequisite completion
  notification to an existing waiting task session.
- Stable message identities and absence of duplicate task registration.

All repositories, logs, configuration, sockets and Git changes stay under a
fresh temporary directory. Provider traffic goes only to the local HTTP fixture;
inherited Horizon/OpenAI/Exa overrides are removed. Failure preserves daemon
logs and provider requests for diagnosis. Use `--keep` to retain successful
artifacts and `--timeout 180` to extend each asynchronous wait.

This replaces the retired `check-board-milestone.py` workflow fixture. It expects
the ordinary `board-organizer`, `board-task`, and `board-reviewer` roles, the new
board tool contracts, and automatic startup registration of the daemon's project
root. It deliberately does not migrate or touch an existing board.
