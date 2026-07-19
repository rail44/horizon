# Backlog — small known issues, not yet missions

Discovered during dogfooding; promote to a numbered mission when picked
up. Numbering is stable and shared with the archive: resolved and closed
entries live in `backlog-resolved.md` keeping their original numbers
(split 2026-07-18).

18. **Web search tool** — give the agent outward web search (the "search
    tool" the owner originally meant, 2026-07-07). Needs its own
    consultation: provider choice (crush shells out to DuckDuckGo Lite
    HTML scraping = free but brittle; opencode uses Exa/Parallel over
    MCP endpoints = API key required; or a plain search API), the
    trust-boundary/network-access approval design, and whether it sits
    behind a crush-`agentic_fetch`-style throwaway subagent (one outer
    approval, inner search/fetch chain) — a shape close to Horizon's
    delegation + skill mechanism. See docs/research on crush/opencode
    tools (in the session transcript, not yet a doc).
19. **Public-code / symbol search** — crush exposes `sourcegraph`
    (public GitHub via Sourcegraph GraphQL, no API key) and
    `lsp_references` (LSP-backed symbol references); opencode has an
    experimental `lsp` tool (default off). Separate discussion from web
    search — LSP integration is a larger commitment (language-server
    lifecycle) and overlaps with future viewer/plugin work. Recorded
    2026-07-07.
28. **[PARTIALLY RESOLVED 5c3f725, e478e6e] `horizon-sessiond` socket e2e
    flakes under the full parallel nextest run** — `terminal_create_diff_
    reconnect_attach_and_shutdown_over_the_real_socket` (`crates/
    horizon-sessiond/tests/e2e.rs`) spawns a real PTY backed by a real
    interactive shell (`/bin/sh -i`). Under **realistic** load -- a plain
    `cargo nextest run --workspace` with no extra synthetic stress, the
    actual shape of the original flake reports -- this is dramatically
    better but not literally zero: 30/30 clean runs across two rounds
    before the retry fix below, then 14/15 in a third round run after a
    follow-up correctness fix to that same retry (same failure signature:
    `terminal_create_diff_...` at exactly the 120s ceiling). One failure
    in 45 realistic-mode runs is a large improvement over the original
    "6+ times in one day" merge-tax rate, but the honest statement is
    "much rarer," not "eliminated." Under a **deliberately extreme**
    synthetic stress (a tight loop of `cargo build -p horizon --release`
    + `cargo clean`, sustained for many minutes, well beyond "another
    worker's live build") a residual failure rate persists -- roughly
    10-20% per run across several measurement rounds -- and traces to a
    genuine, well-evidenced (though not live-captured) upstream hazard
    rather than plain scheduling slowness: see the `portable-pty`
    fork-safety backlog entry. Two rounds of raising `read_terminal_
    update`'s fixed timeout (10s to 60s to 120s, `TERMINAL_UPDATE_TIMEOUT`
    in `tests/e2e.rs`) each got defeated by a failure landing at *exactly*
    the new ceiling -- the signature of a genuine stall, not merely
    "slower under load" -- which is what prompted the deeper
    investigation. Landed fixes: (1) the timeout raise above, generous
    for the realistic case; (2) a nextest test-group
    (`.config/nextest.toml`) serializing every test in the `horizon-
    sessiond::e2e` binary against each other (`max-threads = 1`), removing
    self-contention as a variable; (3) a production fix in `crates/
    horizon-sessiond/src/terminal.rs`'s `TerminalHost::create` --
    previously a stuck PTY spawn could wedge that connection's entire
    message loop forever (`Command::spawn` blocks its calling thread with
    no way to interrupt it); now each spawn attempt is bounded to 10s
    (`TERMINAL_SPAWN_TIMEOUT`) on its own thread with up to 3 retries
    before reporting a `TerminalUpdate::Error`, converting an unrecoverable
    freeze into a bounded, retriable failure; (4) a follow-up correctness
    fix to (3) caught in review: the original retry design let a late,
    abandoned attempt's success unconditionally overwrite a session an
    earlier retry had already installed, and both attempts' threads would
    then call `forward_updates` for the same id, interleaving two
    different shells' output into one pane. `TerminalHost::install_if_
    vacant` now makes installation first-wins (`HashMap::Entry`, checked
    under the lock) and kills/shuts down a losing late duplicate rather
    than letting it live on unobserved (`HostedTerminal` has no `Drop`
    impl, so this must be explicit) -- covered by two unit tests
    (`terminal::tests::install_if_vacant_*`). None of this is a full fix
    for the root cause -- which, per the residual failure rate above, is
    not eliminated under the extreme synthetic case, and now demonstrably
    not fully eliminated under realistic load either. Left open for a
    follow-up: see the `portable-pty` backlog entry for options (upgrade,
    vendor patch, or accept the retry mitigation as the practical ceiling
    given how rare it now is).
44. **SGR text styles never reach frames — `TerminalSpan` has no style
    field.** Found during the 2026-07-18 background-fill investigation:
    alacritty_terminal parses italic, underline (including styled
    underlines/undercurl), and strikethrough, but `core/render.rs`'s
    span production threads only fg/bg — the frame vocabulary cannot
    express text styles at all. Real-world surface: nvim probed
    undercurl support via DECRQSS (`4:3m` then `DCS $q m`) and got
    silence. Fixing needs a frame-shape addition (style bits on
    `TerminalSpan`, protocol-affecting) plus paint support — a designed
    contract extension, not a patch. Recorded 2026-07-18.

47. **Event-log turn tracker: `turn_id` goes permanently null after a
    turn's first approval.** `persistence/event_log/turn.rs` opens a
    turn on a user message and closes it on `WaitingForApproval`/
    `WaitingForUser`, never reopening until the next user message — so
    everything after a turn's first approval (198/248 approval-gated
    calls in the current log) carries `turn_id=null`, making per-turn
    analytics structurally blind to exactly the approval bursts they
    should measure. Same identity family as backlog 42. Recorded
    2026-07-19.

48. **Model resubmits byte-identical `fs.edit` calls it already
    applied.** In the worst same-file run (22 consecutive edits, session
    `05254b6a`), 3 calls were exact duplicates of an earlier
    `old_string`+`new_string` 10–18 minutes later — one even reusing the
    same provider `call_id` — i.e. the model lost track of an edit that
    had already landed. Candidate fixes live on the tool-feedback side
    (e.g. a clearer "already applied / old_string absent because you
    already changed it" result) rather than the approval side. Related:
    42/47. Recorded 2026-07-19 from the event-log analysis.

50. **Decide `Reload Session Runtime`'s residual auto-reseed.** The
    2026-07-19 empty-workspace correction removed auto-reseed from every
    termination path but deliberately kept `ensure_workspace_has_pane`
    in `reload_session_runtime` (killing every terminal session there is
    an operational side effect of restarting the daemon, not a user
    emptying the workspace on purpose — see its doc comment). Whether
    that distinction holds or reload should also restore-to-empty is an
    owner call; one small site either way.

51. **Session-protocol version mismatch is treated as transient: the UI
    retries the hello forever instead of surfacing an actionable state.**
    First hit live on the v5→v6 bump (2026-07-19): the owner's
    long-lived `horizon-sessiond` (started before the bump, speaking v5)
    rejected the new UI's v6 `Hello`, and the UI looped
    "hello transport failed, retrying" on stderr indefinitely. The
    daemon *surviving UI restarts is the design*, so every wire-shape
    bump guarantees each live daemon hits this; the handshake failing is
    correct, the presentation isn't. The version mismatch is already
    detected and named on the daemon side ("this build speaks v5,
    received v6") — the UI should classify it as permanent, stop
    retrying, and present the remedy ("session runtime is older than
    this build — Reload Session Runtime; its terminal sessions will
    end"), ideally as a one-action prompt rather than log spam.
    **Worse (owner observation, same incident): the app is inoperable
    during initial load while the hello retry loops, so the palette —
    and with it Reload Session Runtime, the in-app remedy — is
    unreachable; the only exit today is killing the daemon process
    externally.** That contradicts `src/sessiond/`'s stated
    non-blocking connect/spawn intent: whatever the fix surfaces, the
    shell must stay operable while runtime connect fails at startup.
    Start at `src/sessiond/` (connect/hello retry, the startup
    operability gap) and the hello error surface in
    `crates/horizon-session-protocol`. Recorded 2026-07-19.

43. **Shared build-dir serves stale lib artifacts across worktrees —
    phantom E0432 on freshly-added exports.** Observed twice on
    2026-07-18: a workspace-wide test build resolved
    `horizon_terminal_core` against a stale cached rlib missing the
    just-added `DEFAULT_SCROLLBACK_LINES` export (first in a worker
    worktree mid-task, then in the main checkout right after merging —
    the second occurrence made a pre-commit gate fail on code that was
    correct). `cargo clean -p horizon-terminal-core` fixes it
    immediately both times. Same shared-`build.build-dir` family as
    items 36/40 but a different shape (lib fingerprint/rmeta staleness,
    not binary uplift or env-baked paths). Diagnostic signature: E0432
    on an import that grep confirms exists, while `cargo check -p
    <crate>` alone passes. Workaround is cheap; root-causing (cargo
    fingerprint interaction with concurrent worktree builds) is open.
    Also process-relevant: plain `git merge` commits bypass the
    pre-commit hook, so a merge integrating such a false-negative (or a
    real breakage) can reach main ungated — the project session now
    runs the gate manually between merge and push.
    *Second signature confirmed same day*: while two workers rebuilt
    `horizon-agent` concurrently, `cargo nextest run --workspace` in one
    worktree repeatedly linked a stale `horizon_agent` rlib carrying the
    *other worktree's* API ("Fresh" misdetermination), while `cargo
    check -p horizon-agent` alone stayed correct; `find -name '*.rs'
    -exec touch` + rerun fixed it each time. A stale *test binary* can
    also misreport the workspace test COUNT — successive gate runs on
    the same tree flapped between 998 and 1008 until a `cargo clean -p`
    of the churned crates settled the true count — so a count that
    disagrees between runs is itself the diagnostic, and neither reading
    is trustworthy without a clean rebuild of the crates in flux.
    AGENTS.md's build-dir section now carries the caveat.
    *Sixth occurrence, worst shape yet (2026-07-19)*: a sibling
    worktree's WORK-IN-PROGRESS semantics (an unmerged redesign of the
    zero-tab persistence invariant) leaked through a stale
    `horizon-workspace` rlib into main-checkout test runs, making a
    main test fail "deterministically" — and `git stash`-based
    clean-tree verification does NOT catch this (stash restores source,
    not the artifact cache). Only `cargo clean -p <crate>` of the
    leaked crate tells the truth. A "deterministic" cross-crate failure
    contradicting a recent green run should be treated as this bug
    until a post-clean rerun says otherwise.

42. **Tool-call rows have no per-occurrence identity when a provider
    reuses a call_id.** The 2026-07-18 reused-call_id fix (`1d86521`)
    made approval attribution and proposal bodies follow the most
    recent occurrence, but `ToolCallView`/`tool_call_body` still key
    purely on `call_id`: with a duplicate id, manually expanding the
    *older*, already-finished row shows the latest occurrence's body
    instead of its own, and GPUI element ids (`running-row-{call_id}`)
    collide across the two rows. Both need the rare duplicate-id
    condition plus interaction with the stale row specifically; a
    proper fix is a per-occurrence identity model — design judgment,
    not mechanical. Recorded 2026-07-18 from the fix's review.

31. **Suspected upstream fork-safety hazard in `portable-pty` 0.9.0's PTY
    spawn — can wedge a terminal spawn under extreme concurrent load, at a
    rate a bounded retry only partially masks** — found while validating
    the backlog-27/28 fix (raised e2e timeouts to a generous 120s, then hit
    *exactly* that ceiling twice in a row -- once at 60.071s against the
    old ceiling, again at 120.084s against the new one -- while
    stress-testing with a continuous `cargo build -p horizon --release` +
    `cargo clean` loop). Hitting the ceiling exactly, on the very first PTY
    update after `Create`, both times is the signature of a genuine hang,
    not merely "slower under load" (a scaling delay would show up at
    varying points below the ceiling, not pinned to it).
    `crates/horizon-sessiond/src/terminal.rs`'s `spawn_terminal` calls
    `portable_pty`'s `MasterPty::spawn_command`
    (`portable-pty-0.9.0/src/unix.rs:228`), which sets a `pre_exec` closure
    run in the fork()'d child *before* `execve`. That closure calls
    `close_random_fds()` (`unix.rs:152`), which does `std::fs::
    read_dir("/dev/fd")` — a heap-allocating, non-async-signal-safe
    operation. Rust's own `pre_exec` docs warn exactly about this: if
    another thread in the (heavily multi-threaded, Tokio + per-session +
    per-terminal OS threads) parent held e.g. glibc's malloc arena lock at
    the instant of `fork()`, the child inherits that lock permanently
    locked (the thread that would release it doesn't exist in the child's
    copy), so any allocation in the child -- exactly what `read_dir` does --
    blocks forever, and the process never reaches `execve`.
    **Not confirmed live**: a follow-up run instrumented with per-step
    diagnostic logging (entry to `create`, before/after each spawn thread,
    before/after `recv_timeout`) failed to reproduce the hang across 25
    consecutive tries under similar stress, so the exact stall point was
    never actually observed mid-hang -- this remains a well-evidenced
    hypothesis from the code and Rust's documented hazard, not a proven
    live capture.
    **Mitigation landed, not a full fix**: `TerminalHost::create` now
    bounds each spawn attempt to 10s on its own thread and retries up to 3
    times before reporting a `TerminalUpdate::Error`, so a stuck spawn can
    no longer wedge a connection's message loop forever (see backlog-28).
    A review pass on that mitigation caught a second, distinct bug it
    introduced (fixed in the same follow-up): the original design let a
    late, abandoned attempt's success unconditionally overwrite a session
    an earlier retry had already installed, so both attempts' `forward_
    updates` loops could end up running for the same `session_id`,
    interleaving two different shells' output into one pane.
    `TerminalHost::install_if_vacant` now makes installation first-wins
    and kills a losing late duplicate instead. Measured with the full
    mitigation in place: focused stress runs of just this test under the
    same sustained synthetic load still showed a residual failure rate
    around 10% (4/40 in one round), i.e. the retry reduces but does not
    eliminate the observed rate under *extreme* synthetic contention.
    Under realistic load (plain `cargo nextest run --workspace`, no extra
    synthetic stress -- the actual shape of the original flake reports)
    it held at 30/30 clean across two rounds before the install-race
    follow-up, then hit once in 15 runs after it (same failure signature,
    same test, same ~120s ceiling) -- so "rare," not "eliminated," is the
    honest characterization even under realistic load; see backlog-28 for
    the combined count.
    Out of scope to fix at the root here (patching/vendoring a third-party
    crate is a separate, larger decision) -- options for a follow-up:
    upgrade `portable-pty` if a fixed release ever ships, `[patch]` a local
    fork that drops or reworks `close_random_fds`, increase `MAX_SPAWN_
    ATTEMPTS`/add inter-attempt backoff in `terminal.rs` if the residual
    rate ever proves disruptive in practice, or get a live capture (attach
    `strace`/`gdb` to a hung `horizon-sessiond` test process before its
    `Drop`-triggered cleanup fires) to actually confirm or rule out the
    fork-safety hypothesis. Worth tracking because this isn't just a test
    hazard -- if real, it means a real user's terminal spawn could
    occasionally still fail (now reported as an error rather than freezing
    forever, per the mitigation above) under heavy host load. Recorded
    2026-07-12.

52. **`split_pane_in_tab` corrupts state when the active tab is
    missing.** Found during the backlog-49 fix (2026-07-19): with zero
    tabs, the View-kind split path (`split_active_tab_with_view` →
    `split_tab` → `split_pane_in_tab` in `horizon-workspace`) fails to
    find the active tab but still pushes an orphan `Pane` into
    `self.panes` and points `active_tab` at a nonexistent tab id.
    Command enablement now gates the only user-reachable entry
    (`c325cd0`), so this is defense-in-depth: make the model operation
    itself refuse (early return) when the active tab isn't found, with
    a regression test calling it directly. Mechanical.
53. **[RESOLVED ca36ea9-follow-up] `horizon-sessiond`'s worktree tests
    leaked real `git` operations onto the enclosing repository.** Merged
    same-day incident (`ca36ea9`, 2026-07-19): `crates/horizon-sessiond/
    src/worktree.rs`'s "isolated" tests shell out to `git -C <TempDir>
    ...`, which looks correctly scoped, but `-C` does not override an
    already-set `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` environment
    variable — it only changes cwd. Every session in this repo works from
    a *linked* git worktree, and `git commit` run from a linked worktree
    (as `hooks/pre-commit` does — it runs the full `cargo nextest run
    --workspace` gate) exports an absolute `GIT_DIR` (pointing at
    `$GIT_COMMON_DIR/worktrees/<name>`) into the hook's environment;
    nothing downstream (cargo, nextest, the test binary) sanitizes it, so
    every "isolated" git call in the test module silently operated on the
    real repository instead of its scratch `TempDir`. Confirmed by
    reproduction in a disposable fake repo+worktree pair under `/tmp`
    (never against the real repo): with that env leaked, `git init` in
    the scratch dir reinitializes the leaked git-dir instead and flips
    `core.bare` to `true` on the *shared* config (explains the observed
    `core.bare=true`, since that setting isn't per-worktree); `repo_root`
    resolution goes through `--git-common-dir`, which always returns the
    shared common dir regardless of which worktree leaked, so
    `.horizon/worktrees/<slug>` gets created as a real directory at the
    main checkout's root (explains the `.horizon` skeleton); and
    `commit_file`'s `add`+`commit` land a real commit — tree content
    matching the scratch fixture (`README.md` = `"root\n"`) — on whatever
    branch was checked out in the leaking worktree (explains the
    `README.md` content once that worktree's working tree next synced to
    its now-corrupted HEAD). The flaky first-run-vs-rerun
    `remove_worktree_if_clean_keeps_a_dirty_worktree` was a symptom of the
    same leak: with GIT_DIR shared, every worktree test across the whole
    `--workspace` nextest run raced on one real git-dir instead of each
    getting its own independent repo. Fix: every git invocation in the
    module (production `run_git` and the test module's own `git()`
    helper, plus two ad-hoc `Command::new("git")` calls in tests) now
    strips every inherited `GIT_*` env var before spawning
    (`scrub_git_env`), making `-C` the sole source of truth again. Added
    an `EnclosingRepoGuard` hermeticity canary to every real-git test:
    it snapshots the enclosing repo (found once via `git rev-parse` from
    `CARGO_MANIFEST_DIR`, a compile-time constant immune to the same
    class of leak) — `core.bare`, `git status --porcelain`, and any stray
    `refs/heads/horizon/*` — and re-asserts it unchanged on drop, so any
    future escape fails loudly at the offending test. Verified: 13
    consecutive `cargo nextest run -p horizon-sessiond worktree` passes
    with no flake, canary green throughout.

54. **Owner-deferred design consultation: shared-spawn lineage
    semantics.** First dogfooding of the session-relationship model
    (2026-07-19) surfaced a gap between the owner's felt model and the
    shipped one: opening a session from an existing pane (a plain, non-
    isolated spawn) *feels* like it should make the new session a child
    in the derivation tree, but `docs/session-relationship-design.md`
    decisions 2/3 deliberately create an edge only via isolation — a
    shared spawn co-locates with its source (same directory) but derives
    nothing, keeping the tree pure derivation rather than accreting a
    reference/co-location edge. Not a request to change now — the owner
    will schedule the discussion; options seen so far: (a) keep the
    tree as-is (isolation-only edges, shared spawns stay unrelated
    roots); (b) surface shared spawns as a distinct, weaker edge kind
    (e.g. "spawned alongside," exempt from subtree-terminate's cascade,
    per decision 5) so the session manager's lineage view can still show
    the relationship the owner expects without conflating it with a real
    derivation/worktree-branch edge.

55. **Sandbox-denial retry leaves a double transcript row.** The
    denial-retry mechanism (leg 3, `207392c`) reissues a fresh
    `ToolCallRequested` for the same call_id, and
    `build_tool_call_views` starts a new row per occurrence — so one
    conceptual bash call shows two rows: the abandoned sandboxed
    attempt (started, never finished, no approval state) and the
    retry that actually completes. Functionally correct, cosmetically
    misleading. Same per-occurrence-identity family as backlog 42 —
    fixing 42's identity model is likely the real fix; a targeted
    "superseded by retry" row state is the cheaper alternative.
    Recorded 2026-07-19 from the leg-3 review.

56. **Sandboxed bash loses the CPU-niceness hardening.** The
    unsandboxed bash path applies `setpriority` via `pre_exec`;
    `horizon_sandbox::spawn` rebuilds the `Command` and
    `std::process::Command` exposes no getter for `pre_exec` hooks, so
    the sandboxed path runs without it. Options: a niceness knob on
    `horizon-sandbox`'s policy/API, or `setpriority` on the returned
    child's pid post-spawn (racy but adequate). Recorded 2026-07-19
    from the leg-3 review.

57. **`new-agent` should print the created session id (at least under
    `--json`).** The CLI dogfooding loop (`.claude/skills/
    horizon-dogfood/SKILL.md`) has to infer the new session via
    `horizon sessions --json` right after spawning — racy if two
    spawns interleave. Echoing the created id in the invoke response
    closes the loop cleanly. Small, additive. Recorded 2026-07-19.

58. **`skill.read`'s registry still discovers from the daemon's cwd.**
    The 2026-07-19 prompt-cwd fix (`cb8e41f`) routed the session's
    workspace_root into prompt assembly and the prompt-side skill
    listing, but `horizon-sessiond`'s own `SkillRegistry::discover(&cwd)`
    (feeding the `skill.read` tool, `run_session` in
    crates/horizon-sessiond/src/session.rs) still walks from the daemon
    process's cwd — an isolated session reading skills sees the
    daemon's repo, not its own worktree (usually the same repo today,
    but wrong once sessions span repositories). Same-shape fix: thread
    the session root in. Recorded 2026-07-19 from the fix's review.
