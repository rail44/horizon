# Backlog — small known issues, not yet missions

Discovered during dogfooding; promote to a numbered mission when picked
up. Numbering is stable and shared with the archive: resolved and closed
entries live in `backlog-resolved.md` keeping their original numbers
(split 2026-07-18).

28. **[PARTIALLY RESOLVED 5c3f725, e478e6e] `horizon-agentd` socket e2e
    flakes under the full parallel nextest run** — `terminal_create_diff_
    reconnect_attach_and_shutdown_over_the_real_socket` (`crates/
    horizon-agentd/tests/e2e.rs`) spawns a real PTY backed by a real
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
    horizon-agentd/src/terminal.rs`'s `TerminalHost::create` --
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
    Observation 2026-07-19 (nono-merge gate): same signature (this test,
    exactly the 120s ceiling) at ~3/8 full-suite runs in the shared main
    checkout while the owner's live GUI + several sessiond processes were
    running -- and reproduced 1/6 on a pre-nono baseline worktree
    (0d00c5c) in the same conditions, confirming it tracks host load, not
    the sandbox-backend migration. The same baseline round also showed
    one fast (0.05s) one-off failure of the bwrap-era
    `bash_auto_executes_sandboxed_in_an_isolated_session_with_an_engaged_
    sandbox` -- unreproduced, consistent with a backlog-43 shared-cache
    artifact or a latent tier-1 test race; the bwrap variant is deleted
    post-migration, so only worth chasing if the nono-era tier-1 tests
    ever show the same shape.




50. **[MOOT 2026-07-30 — terminald split] Decide `Reload Session
    Runtime`'s residual auto-reseed.** The
    2026-07-19 empty-workspace correction removed auto-reseed from every
    termination path but deliberately kept `ensure_workspace_has_pane`
    in `reload_session_runtime` (killing every terminal session there is
    an operational side effect of restarting the daemon, not a user
    emptying the workspace on purpose — see its doc comment). Whether
    that distinction holds or reload should also restore-to-empty is an
    owner call; one small site either way.
    **Moot as posed:** `Reload Agent Runtime` no longer terminates a
    single terminal session (`docs/terminald-split-design.md` decision 2),
    so it never empties the workspace and never reseeds. The reseed moved,
    unchanged, to `reload_terminal_runtime` — where the "operational side
    effect, not a user emptying the workspace" reading is no longer a
    judgement call but the literal contract of an explicitly destructive
    command. Nothing left to decide.

51. **Session-protocol version mismatch is treated as transient: the UI
    retries the hello forever instead of surfacing an actionable state.**
    First hit live on the v5→v6 bump (2026-07-19): the owner's
    long-lived `horizon-agentd` (started before the bump, speaking v5)
    rejected the new UI's v6 `Hello`, and the UI looped
    "hello transport failed, retrying" on stderr indefinitely. The
    daemon *surviving UI restarts is the design*, so every wire-shape
    bump guarantees each live daemon hits this; the handshake failing is
    correct, the presentation isn't. The version mismatch is already
    detected and named on the daemon side ("this build speaks v5,
    received v6") — the UI should classify it as permanent, stop
    retrying, and present the remedy ("session runtime is older than
    this build — Reload Agent Runtime; its terminal sessions will
    end"), ideally as a one-action prompt rather than log spam.
    **Worse (owner observation, same incident): the app is inoperable
    during initial load while the hello retry loops, so the palette —
    and with it Reload Agent Runtime, the in-app remedy — is
    unreachable; the only exit today is killing the daemon process
    externally.** That contradicts `src/sessiond/`'s stated
    non-blocking connect/spawn intent: whatever the fix surfaces, the
    shell must stay operable while runtime connect fails at startup.
    Start at `src/sessiond/` (connect/hello retry, the startup
    operability gap) and the hello error surface (`horizon_wire::HubError`
    plus each hub's `hello`). Recorded 2026-07-19.
    **Surface doubled 2026-07-30 (terminald split):** there are now two
    daemons that can be stale independently, so this item's fix has to
    cover both connections. What changed in its favour: the two runtimes'
    failures no longer bleed into each other (separate route tables), the
    remedy differs per daemon (`Reload Agent Runtime` vs. the
    destructive `Reload Terminal Runtime`), and a stale *terminald* is
    much more likely than a stale sessiond because that daemon is
    deliberately long-lived — which is also why the terminald connection
    already refuses cleanly with a named remedy instead of retrying
    forever (decision 6's post-hello probe). The sessiond side is still
    as reported. Whoever takes this should read the terminald refusal
    path first: it is the shape this item asks for, on one of the two
    connections, and the honest question is whether the *presentation*
    (an error event into panes plus stderr) is enough or a one-action
    prompt is still wanted.

70. **Done 2026-07-31.** Subsumed into
    `docs/runtime-crate-alignment-design.md` (phase 1 there) and finished
    by its phase 2: `horizon-wire` holds the domain-free foundation, each
    hub moved into its own runtime crate, and `horizon-session-protocol`
    is deleted. `cargo tree -p horizon-terminald -e normal` now has
    neither `horizon-agent` nor libduckdb (the one remaining agent edge is
    `tests/e2e.rs`'s dev-dependency, which drives a real agentd to prove
    the split's acceptance property). Original entry follows.
    **Carve the protocol crate's domain-free foundation out of
    `horizon-session-protocol`.** After the terminald split
    (`docs/terminald-split-design.md`), `horizon-terminald` depends on
    `horizon-session-protocol` for the codec pin, version negotiation,
    size caps, and its own `TerminalHub` — and that crate also names the
    agent vocabulary, so `horizon-agent` (and therefore DuckDB) is in the
    terminal daemon's link graph without one symbol being used. Runtime
    independence is unaffected (separate process, socket, and hub trait);
    the cost is conceptual plus a needless libduckdb link on the binary
    whose whole point is being boring and long-lived. Shape: a small
    domain-free `horizon-wire` crate (codec, `VersionRange`/`ClientHello`,
    the `*_MAX_ITEM_BYTES` caps, `DecodeSkipLog`, `channel_schema`) that
    both protocol crates depend on, with `horizon-session-protocol`
    re-exporting it so no call site moves. Deliberately deferred out of
    the split itself so the wire changed exactly once. Recorded
    2026-07-30.

65. **Verify `replace_all` retires the bash editing escapes.** Baseline,
    nineteen days of logs to 2026-07-26: 272 `fs.edit` calls, 9 editing
    operations escaped to bash (`perl -i` ×5, `sed -i` ×3, one python
    rewrite) because a global replacement could not be expressed, plus 2
    multiple-match errors. After `fs.edit` gained `replace_all` +
    `occurrences` (merge `6cbb24e`), re-measure over a comparable window:
    the escapes should drop to ~0 and multi-match errors should convert
    to `replace_all` uses. If escapes persist, the routing (tool
    description) is the next suspect, not the capability. Recorded
    2026-07-26.
    Measurement note 2026-07-28: `fs.edit` became list-only the same week
    (`fs.patch` deleted with it, see `docs/agent-tools-design.md`), so
    "one `fs.edit` call" is no longer "one replacement" — count
    replacements from each result's `edits` array, not from call rows, or
    the post-change window will look artificially cheap next to the 272
    baseline. The escape-count question itself is unaffected.

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
    Partly addressed 2026-07-28: the identity primitive landed —
    `contract::OccurrenceId`, minted at `providers::rig::mapping::
    rig_tool_call_request` and again at `sessiond::session::approval::
    begin_reissued_approval` for every reissue, carried on
    `ToolCallRequest`/`ToolCallResult`/`ApprovalRequest` (additive on the
    wire), matched occurrence-first in `transcript::tool_call::
    build_tool_call_views`, and stored as an `occurrence_id` column on the
    DuckDB tool-call/result/approval projections. **The named UI symptoms
    did not move**: `ToolCallView` still carries only `call_id`, so
    `tool_call_body`/`expanded_rows` keying and the colliding
    `running-row-{call_id}` `ElementId`s are unchanged — expanding the
    older duplicate row still shows the newest occurrence's body. What
    remains is exactly that view-layer work: put the occurrence on
    `ToolCallView`, key `expanded_rows` and every per-row `ElementId` on
    it, and decide what the row header shows when two rows share a
    `call_id`.

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
    `crates/horizon-agentd/src/terminal.rs`'s `spawn_terminal` calls
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
    `strace`/`gdb` to a hung `horizon-agentd` test process before its
    `Drop`-triggered cleanup fires) to actually confirm or rule out the
    fork-safety hypothesis. Worth tracking because this isn't just a test
    hazard -- if real, it means a real user's terminal spawn could
    occasionally still fail (now reported as an error rather than freezing
    forever, per the mitigation above) under heavy host load. Recorded
    2026-07-12.
    **Bounded root-cause investigation (2026-07-19, owner-approved):
    hypothesis CONFIRMED at source level; recommendation = accept the
    mitigation as the practical ceiling.** Findings: (a) 0.9.0 is the
    newest release AND matches wezterm git HEAD (last push 2026-07-16)
    -- no upstream fix exists; the `read_dir`-based `close_random_fds`
    has never been touched since 2020. (b) Two open, unanswered
    upstream issues (wezterm/wezterm#7742, #7893) hit a *different*
    symptom of the same function (it closes std's exec-error pipe,
    turning exec failure into abort) -- an upstream fix for those would
    very likely also fix this; that is the revisit trigger. (c) The one
    technically-correct small patch -- replace the enumeration with
    async-signal-safe `close_range(2)` -- is Linux-only (no macOS
    equivalent in libc 0.2.186), so a vendor patch needs per-platform
    branching validated on a mac: not "small and obviously correct",
    left undone deliberately. (d) The mitigation
    (`TERMINAL_SPAWN_TIMEOUT`/`MAX_SPAWN_ATTEMPTS`/`install_if_vacant`)
    was re-verified intact. Roadmap item stays open only as the
    owner's close/keep call; no further engineering is queued.

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
53. **[RESOLVED ca36ea9-follow-up] `horizon-agentd`'s worktree tests
    leaked real `git` operations onto the enclosing repository.** Merged
    same-day incident (`ca36ea9`, 2026-07-19): `crates/horizon-agentd/
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
    consecutive `cargo nextest run -p horizon-agentd worktree` passes
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
    **Resolved 2026-07-28** (owner decision: emit a terminal outcome, not
    a view-side collapse). 42's identity primitive had already made each
    attempt cleanly *attributable* — the parked first-attempt result
    reaches the row that produced it — but the stuck row was unchanged,
    because attribution was never the cause: `fold_domain_denied`/
    `fold_filesystem_denied` emit no result for the abandoned attempt at
    all, so on the *approve* path the first row never got one. What
    shipped: `tools::approval::superseded_by_retry_result` closes the
    abandoned occurrence with a non-error `ToolCallFinished` carrying a
    `superseded_by_retry` marker, folded on the approve branch of
    `resolve_domain_denial_retry`/`resolve_filesystem_denial_retry`
    (the deny branch still forwards the parked result unchanged — the two
    halves cross-reference each other). The row renders closed with a
    muted "superseded by retry" register rather than success or error
    styling (`ToolCallView::superseded`, `transcript::tool_call::classify`,
    `view::rows::tool_call_glyph`), and the collapsed receipt counts one
    conceptual call once. The fold predicate had to become
    occurrence-aware for this: `should_fold_completion` now reads
    `AgentFrame::has_live_occurrence_finished` — the call_id-keyed
    `has_tool_call_finished` sees the abandoned attempt's close (which
    lands after the reissued request) and would have swallowed the
    retry's own genuine result; the duplicate-approve/deny guard in
    `try_execute` deliberately keeps the call_id-keyed reading. No wire
    change: it is an ordinary `ToolCallFinished`, and the DuckDB
    projection's occurrence-targeted `mark_approval_outcome` leaves the
    retry's pending approval alone.

56. **Sandboxed bash loses the CPU-niceness hardening.** The
    unsandboxed bash path applies `setpriority` via `pre_exec`;
    `horizon_sandbox::spawn` rebuilds the `Command` and
    `std::process::Command` exposes no getter for `pre_exec` hooks, so
    the sandboxed path runs without it. Options: a niceness knob on
    `horizon-sandbox`'s policy/API, or `setpriority` on the returned
    child's pid post-spawn (racy but adequate). Recorded 2026-07-19
    from the leg-3 review.


59. **Resolved for proxy-domain and Linux `openat`/`openat2` denials
    2026-07-21; other syscall families remain.** Tier-1's former "denial ->
    retry without sandbox" flow (`docs/agent-approval-design.md`'s historical
    "Denial UX", the now-removed `horizon_sandbox::is_likely_sandbox_denied`)
    classifies against the wrapped command's own exit code and merged
    output. A piped command masks both: `curl ... | head -n 1` under
    network-off observed empty output with exit 0 (`head`'s own success
    code is what `$?` reports, even though `curl` failed) — no denial
    classification, no retry-without-sandbox offer, the model just sees
    silence. Domain denials are now proxy-recorded and Linux filesystem-open
    denials are supervisor-recorded independently of exit status; neither can
    produce an unsandboxed retry. The original weaker options were: (a) `set -o
    pipefail` in the bash tool's wrapper script
    (`tools::bash::exec::wrapped_script`) — reports the last *failing*
    stage's exit code instead of the last stage's, but changes the
    command's actual semantics (a script relying on the current
    non-pipefail behavior would see a different `$?`), so needs care;
    (b) stderr-pattern-based denial detection independent of exit code
    (weaker signal, but doesn't touch command semantics). Recorded
    2026-07-19 from the tier-1 `/tmp` containment-hole fix's review —
    found while auditing denial classification around that fix, not
    itself part of it.

60. **Prior-art evaluation: nono (nolabs-ai/nono) vs Horizon's
    self-built sandbox — owner-deferred.** nono is a mature (3k stars,
    ~daily commits, v0.68, Apache-2.0, Sigstore-team pedigree)
    kernel-enforced AI-agent sandbox in Rust, whose `crates/nono` is an
    explicitly policy-free, FFI-embeddable library covering exactly
    Horizon's OS-sandbox surface (Landlock/Seatbelt, no daemon,
    per-command). Directly relevant to a "build vs depend" decision for
    `horizon-sandbox`. Concrete borrowables regardless of that decision:
    (a) their Landlock-floor + seccomp-notify-gate + fd-injection design
    that closes the TOCTOU race SECCOMP_USER_NOTIF_FLAG_CONTINUE leaves
    open — a reference for our Landlock/bwrap non-coexistence follow-up
    (the helper-binary idea); (b) an `ApprovalBackend` trait seam (our
    LLM judge would be one backend — and note we're AHEAD of nono here,
    it has only a terminal y/N backend, LLM judge is not built); (c)
    approval rate-limiting (10 req/s, burst 5, deny-on-exceed) as a
    concrete judge-DoS mitigation; (d) their profile/registry
    (JSON, `extends`/groups) as a data-driven counter-example to a
    code-level agent-kinds abstraction; (e) their Capability Broker
    (nested per-tool child sandboxes with per-hop credential scoping) as
    prior art if bash-subprocess granularity ever matters. `security-
    model.mdx` is the rigorous public write-up of the Landlock-vs-mount-
    ns / why-not-DYLD tradeoffs worth reading before re-deriving them.
    Owner will schedule if/when to weigh depend-on-nono; recorded
    2026-07-19, not a request to act now.
    **SDK-feasibility verdict (2026-07-19, built against nono 0.68.0 and
    ran a real sandboxed spawn on this host):** all four acceptance
    criteria MET at the library level — `crates/nono` is crates.io-
    published, CLI/profile-decoupled, a pure programmatic `CapabilitySet`
    builder + `Sandbox::apply_auto` self-apply primitive (no `spawn`
    convenience — it restricts the *calling* process, meant for a forked
    child pre-exec); `nono-proxy` is library-usable and richer than our
    `horizon-sandbox-proxy` (TLS-intercept, credential injection, OAuth);
    macOS Seatbelt is real-CI-tested (macos-14), stronger than our
    compile-only state. BUT full build-vs-depend replacement is NOT
    advised — three obstacles: (1) nono's Linux backend is Landlock+
    seccomp only, **zero namespace isolation** (no mount/PID/UTS/IPC),
    so a nono-sandboxed process still sees the full process list/mounts/
    hostname — a real capability regression vs our bwrap; (2) its
    apply-to-self-then-exec pattern needs async-signal-safe `pre_exec`
    engineering from our multi-threaded sessiond that our
    bwrap-as-separate-binary design currently avoids; (3) even
    `default-features=false` pulls sigstore/reqwest/tokio/hyper
    unconditionally — 278 crates for "just the mechanism". Churn is
    concentrated in `nono-cli`, not the library (library history is
    additive). **Owner-preferred axis was "can we use it as an
    SDK/library" — answer: yes, but partial adoption is the sensible
    shape, not full replacement.** Live options: keep bwrap for Linux;
    adopt nono for macOS (its real-CI Seatbelt beats our unverified
    backend) and/or `nono-proxy` as a proxy upgrade. Owner decision on
    A(all-self-built)/B(partial)/C(full-depend, not advised) is deferred
    while implementation continues on the self-built stack.
    **Refinement (2026-07-19 owner consult): the "zero namespace
    isolation" obstacle largely dissolves under scrutiny.** (a) /tmp:
    the private-tmpfs substitution is a bwrap convenience, not a
    requirement -- a harness-provisioned per-session temp dir + `TMPDIR`
    + a Landlock write rule covers TMPDIR-respecting tools, and
    hardcoded-`/tmp` failures are visible and adaptable; owner: this
    provisioning is harness work, consistent with nono's policy-free
    stance. (b) same-uid `/proc/<pid>/environ` secret exposure (e.g.
    `OPENAI_API_KEY` in sessiond's exec-time environ): owner accepts
    the risk; independently shrinkable by not passing secrets via env
    (note `/proc` environ shows the exec-time block -- `remove_var`
    does not scrub it). (c) signal reach: the original claim was
    WRONG for modern kernels -- verified in nono 0.68.0 source
    (`src/sandbox/linux.rs`, `src/capability.rs`) that `SignalMode`
    maps to Landlock ABI v6 `LANDLOCK_SCOPE_SIGNAL` (kernel 6.12+),
    scoping signals to the sandbox domain; `AllowSameSandbox` fails
    closed on older kernels, `Isolated` silently degrades (ABI-gradient
    caveat). Enforced on the dev machine (kernel 7.0.9). Remaining
    real Linux obstacles are therefore: the 278-crate dependency tax,
    and apply-to-self needing a helper-binary shape (a tiny
    self-applying exec helper -- the same separate-binary shape bwrap
    already has) instead of `pre_exec` from multi-threaded sessiond.
    Net: option C is more viable than first recorded; A/B/C remains
    owner-deferred.
    **DECIDED 2026-07-19: option C (full nono adoption, both OSes).**
    An integration spike (`experiments/nono-spike/`, branch
    `worktree-agent-afb6d8b9e874320c8`, commit `533554b` — standalone
    project, root workspace untouched) resolved the two standing
    obstacles empirically on this host (kernel 7.0.9, Landlock ABI V6):
    (1) apply-to-self needs NO `pre_exec` — nono's `Sandbox::apply_auto`
    is a plain blocking call on an ordinary thread, and our CURRENT
    bwrap backend already spawns from a throwaway `std::thread::spawn`
    that applies seccomp then spawns+joins the child
    (`horizon-sandbox/src/linux/mod.rs`), so nono drops into the exact
    same thread shape — verified thread-scoped, no TSYNC leakage to
    sibling threads; (2) the 278-crate dependency tax is accepted by
    the owner (single-process async is the codebase's direction anyway,
    so tokio/hyper become shared, not a tax). Spike proved fs/network/
    signal containment, TMPDIR-replaces-tmpfs, and that the leg-4a
    UDS-bridge proxy survives on nono needing only the baseline `/` Read
    grant (simpler than bwrap's bind-mount plumbing). The one remaining
    real regression — no PID/mount/UTS/IPC namespace, so `ps`/`/proc`
    show the host's full process list — the owner accepts as the same
    category as the already-accepted `/proc/<pid>/environ` visibility.
    New capabilities nono ADDS over bwrap+seccompiler: explicit signal
    scoping (`SignalMode`→`LANDLOCK_SCOPE_SIGNAL`) and works where
    unprivileged userns is disabled (bwrap can't run there at all).
    Spike friction to carry into implementation: nono's
    `allow_unix_socket` is INERT under library-only `apply_auto` (needs
    the seccomp-notify supervisor) — irrelevant for us since baseline
    Read covers the bridge socket. Follow-up: macOS backend can only be
    verified on a mac (spike host is Linux). Migration tracked in the
    roadmap's approval-trust-model entry.

61. **Darwin cross-typecheck from Linux is blocked by nono's dependency
    graph.** `cargo check --target x86_64-apple-darwin -p horizon-sandbox`
    fails host-wide: nono unconditionally pulls
    sigstore-verify->reqwest->rustls->aws-lc-rs, whose `aws-lc-sys` build
    script needs an Apple-aware C toolchain (`-arch`/
    `-mmacosx-version-min`) this Linux host lacks. Discovered during the
    macOS backend migration (2026-07-19); the old SBPL backend's
    "compile-only" bar was met instead via an API-faithful local stub of
    nono patched in with `[patch.crates-io]`, checked, then fully
    reverted (recorded with the macOS-backend branch handoff).
    Options if this bar needs to be routine (e.g. on nono bumps): keep
    the stub as a checked-in dev tool, set up osxcross, or accept
    review-only for darwin paths. Real-mac runtime verification of the
    whole macOS backend (helper exec handoff, profile application, exact
    `ProxyOnly` endpoint, baseline dirs) is the standing open follow-up.

62. **gpui-component's `ColorPicker` builds its palette panel every render
    even while closed.** Upstream (rev `0775df3`,
    `crates/ui/src/color_picker.rs:785` in the pinned checkout) passes
    `self.render_colors(window, cx)` eagerly into the Popover child with
    no `state.open` gate, so a *closed* picker still constructs ~111
    swatch elements per render (`render_item` per swatch plus a fresh
    `color_palettes()` `Vec<Vec<Hsla>>` rebuild). Theme settings renders
    7–8 pickers at once (`src/theme_settings/mod.rs:351-387`), ≈800–900
    element builds per render while that pane's tab is frontmost —
    measured at 4.4% of GUI main-thread samples in the 2026-07-19
    profile. The cost is transient, not standing: `WorkspaceShell`
    renders only the active tab (`src/workspace/render.rs:716-720`), so
    it vanishes on tab switch (the pane does persist and restore via
    `ViewKindState::ThemeSettings`, so it can come back frontmost).
    Decision (owner, 2026-07-19): accept as-is — lazy-unmount buys
    nothing (background tabs already don't render) and a bespoke palette
    UI is overkill. The proper fix is upstream: gate the palette panel
    on `state.open` (a few lines); adopt by bumping the pinned rev when
    it lands, or fork+pin if it ever becomes urgent. Optional
    horizon-side nibble meanwhile: pass `.featured_colors(...)`
    explicitly to skip the per-render 12-color rebuild from
    `cx.theme()`.

66. The pre-commit hook's touch-based rebuild does not displace every
    stale artifact from a deleted worktree. Observed 2026-07-27, twice,
    deterministically: `horizon-config`'s
    `config_example_toml_documents_grants_without_activating_any` failed
    under `cargo nextest run --workspace` with "config.example.toml must
    be readable: NotFound" — the workspace-feature-unified test binary
    in the shared build dir was compiled inside a since-removed worker
    worktree, so its baked `CARGO_MANIFEST_DIR` pointed at a deleted
    path. Touching this checkout's sources (the hook's backlog-43
    defense) did not force that variant's rebuild; `cargo nextest run
    -p horizon-config` used a different feature unification and passed,
    which made the failure look flaky at first. Workaround that worked:
    `cargo clean -p horizon-config` (safe for lib crates; avoid for
    crates whose binaries `cargo run` needs). Open: harden the hook —
    either `cargo clean -p` for workspace lib crates, or teach the gate
    to detect artifacts whose baked manifest dir no longer exists. Also
    re-examine today's earlier terminal-e2e gate timeout under this
    lens before calling it load flakiness.
    Second wave, same day: after a full `cargo clean -p <every member>`
    + `cargo build --workspace`, the next gate failed differently —
    horizon-agent's sandboxed-bash tests and horizon-sandbox's proxy
    tests spawn `horizon-sandbox-helper` at RUNTIME (resolved via env /
    current_exe fallback, not a cargo dependency edge), and during that
    gate the helper was evidently not in place when those tests ran;
    everything passed once per-package runs rebuilt things. Mechanism
    not fully pinned (build ordering vs. feature unification) — worth
    reproducing before designing the hook hardening.
    **Mechanism pinned 2026-07-28** (worker worktree, while a sibling
    worktree was building concurrently): six `horizon-agent`
    `tools::tests::` sandboxed-bash tests failed deterministically with
    a `Finished` completion carrying `exit_code: null`
    (`a_configured_grant_makes_an_out_of_workspace_write_a_non_crossing`,
    `approved_git_commit_writes_linked_metadata_once_and_stays_sandboxed`,
    `bash_auto_executes_sandboxed_and_is_killed_on_timeout`,
    `bash_auto_executes_sandboxed_in_an_isolated_session_with_an_engaged_sandbox`,
    `judge_approved_filesystem_retry_reruns_sandboxed_with_the_approved_grant`,
    `tier1_sandboxed_bash_write_to_tmp_never_leaks_to_the_hosts_real_tmp`)
    — reproduced identically on the unmodified branch base, so not
    change-induced. Cause is `horizon_sandbox::helper::resolve`'s
    fallback chain meeting the split build-dir: a *unit* test binary
    gets no `CARGO_BIN_EXE_horizon-sandbox-helper`, its `current_exe()`
    is `{CARGO_HOME}/horizon-build-dir/debug/deps/…`, and the adjacent
    and `deps/..` probes both miss because cargo uplifts final binaries
    to `<worktree>/target/debug`, **not** into the shared build-dir. So
    resolution falls through to `cargo_test_artifact`, which picks the
    *newest* `deps/horizon_sandbox_helper-*` carrying the protocol
    marker — a directory every worktree writes into, so a sibling's
    build (or that bin target's own test-harness variant, ~6.9MB next to
    the real ~75MB binary) wins the race and gets spawned as the
    supervisor. Proof: setting
    `CARGO_BIN_EXE_horizon-sandbox-helper=<worktree>/target/debug/horizon-sandbox-helper`
    turned the same run green (80/80 in `tools::tests::`, and 1539/1539
    workspace-wide). Fix candidates, in preference order: have
    `resolve()` also probe the *target-dir* (`CARGO_TARGET_DIR` /
    `<workspace-root>/target/<profile>/`) before the deps scan; or make
    `cargo_test_artifact` verify the candidate is the real bin rather
    than the newest marker-bearing file; or set the env var in a
    `build.rs`/nextest profile. Until then the gate is only reliable
    when no sibling worktree is building.
    **Resolved 2026-07-30** (with backlog 43's residue): the tracked
    `.cargo/config.toml` now sets `build.rustc-workspace-wrapper =
    ".cargo/rustc-shim"`, whose config-relative (therefore per-worktree)
    path is hashed into every workspace member's artifact metadata —
    cargo's only member-gated hash input — so member build/test artifacts
    no longer collide across checkouts while dependencies stay shared.
    The gate's clippy step carries a per-worktree build-dir via
    `--config` because `cargo clippy` overrides the wrapper through env
    (measured: a real `-D warnings` violation passed green under
    sharing). The touch-all-sources hook defense and the "no concurrent
    gates" rule are retired. Root cause, experiments, and the mechanism
    table live in the 2026-07-30 cargo-isolation investigation (session
    records); the `helper::resolve` deps-scan heuristic noted above is
    narrowed by the isolation (distinct hashes) but its newest-file scan
    remains a horizon-side sharp edge if two variants of one worktree
    coexist.

67. Running background `task` children are invisible to the user
    (owner observation, 2026-07-28, during the first async-task
    validation run). The only surface is the transcript's
    `Task <description>` row at launch; there is no live indicator of
    in-flight children on the pane, no listing in Manage Sessions
    (exploration sessions are deliberately excluded from session_list),
    and no user-facing cancel affordance for an individual child. Now
    that children survive `cancel-turn` and cross turn boundaries
    (`docs/agent-async-task-design.md` decision 4), a user can have
    work running with nothing on screen saying so. Wanted: a pane-level
    running/completed indicator (the `description` field exists
    precisely to label this) and probably per-child cancel; design
    belongs with the UI, not the daemon — the subscription seam already
    carries the needed events.

68. A rebuilt history still replays a *parallel* tool batch with its
    results out of order (found 2026-07-28 while fixing the `b182c25b`
    session death). `mapping::repair_replayed_message_pairing` folds a
    provider response's split-up events back into one assistant message,
    but only while they are adjacent: when a batch's results arrive
    interleaved with the next call's announcement, the replay reads
    `assistant(A,B) → tool(B) → assistant(C) → tool(C) → tool(A)`, so
    `tool(A)` sits behind an assistant message that announced `C`. Live
    history never has this shape (one assistant message holds the whole
    batch, results follow it). Harmless against every template observed
    so far — MiniMax-M3's only requires *some* tool call on the nearest
    assistant message, which holds — but a template that matches the
    call id would reject it. Measured on the real `b182c25b` log: 4
    occurrences in 200 messages, all inside parallel batches. Fixing it
    properly wants turn grouping in the replay; `Record::turn_id` exists
    but `rig_messages_from_horizon_events` only receives `&[Event]`.

69. **libduckdb crashes on an aggregate over a statically-empty filter
    while the table carries uncommitted rows** — recorded here as the
    resolution of the first real suspect raised against AGENTS.md "Build
    setup"'s libduckdb-sys-1.10504.0-expects-1.5.4-vs-Void-ships-1.5.0
    skew caveat: it is a DuckDB bug present in *both* versions, not a
    skew symptom (evidence below). Found 2026-07-28 root-causing "DuckDB
    projection rebuild failed (INTERNAL Error: Attempted to access index
    0 within vector of size 0); live projection disabled for this run"
    against the owner's real ~121k-record agent event log, which blanked
    every session's transcript in the UI. Minimal repro:

    ```sql
    BEGIN TRANSACTION;
    INSERT INTO agent_approvals (event_id, session_id, sequence, call_id, reason)
      VALUES ('e1', 's', 1, 'c1', 'r');
    SELECT MAX(sequence) FROM agent_approvals WHERE call_id = 'c2';
    -- INTERNAL Error: Attempted to access index 0 within vector of size 0
    ```

    Every condition is load-bearing: it needs the aggregate (the same
    filter without one returns 0 rows fine), a filter the optimizer can
    *statically prove* empty from column statistics (`'c2'` is outside
    the only row's min/max), and transaction-local rows (identical query
    is fine once they are committed, or when the transaction wrote
    nothing to that table). The failure is in the optimizer at
    (re)bind time — `duckdb::Optimizer::RunOptimizer` under
    `ClientContext::RebindPreparedStatement` — not at execution, and it
    aborts the whole transaction ("Current transaction is aborted (please
    ROLLBACK)"), whose `COMMIT` then commits nothing.

    **Not a skew artifact, and the AGENTS.md ABI-compatibility statement
    stands**: reproduced identically on the system libduckdb 1.5.0 *and*
    on libduckdb-sys 1.10504.0's own bundled 1.5.4 (`cargo test -p
    horizon-agent --features bundled-duckdb`, which reports
    `version() = v1.5.4`), so a Void catch-up would not have fixed it and
    nothing here argues for a system upgrade. The `ORDER BY ... LIMIT 1`
    replacement was checked on both too (`Ok(None)` when the filter
    selects nothing, the row when it does).

    Horizon's own occurrence is fixed by not writing the pattern
    (`projection::Store::most_recent_pending_approval` uses `ORDER BY ...
    LIMIT 1` instead of `MAX(...)`); the rule to carry forward is
    **prefer a top-N lookup over an aggregate anywhere a filter may
    select nothing**, especially inside the batch transactions
    `import::apply_records` runs. Not reported upstream (see the
    upstream-posting policy); worth checking against a newer DuckDB
    before writing any new aggregate in that position.
