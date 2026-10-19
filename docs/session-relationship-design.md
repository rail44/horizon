# Session Relationship Model — Design Decisions

Status: decided 2026-07-07 (owner consultation in the project session).
Implementation not started beyond the one landed foundation (below).
Sibling to `docs/session-daemon-design.md`; both feed shared-foundation
4 (inter-agent messaging + session daemon) in `docs/roadmap.md`.

Motive: worktree isolation and terminal-cwd inheritance are not
standalone features — they are the **filesystem projection of a
relationship between sessions**. The owner's concrete pain (to use the
output of an agent working in a worktree, you must hunt down the actual
worktree directory and `cd` there) is the symptom of Horizon having no
session-relationship model: sessions relate only spatially (the layout
tree), never by derivation. This document defines that relationship
model and how the workspace controls it. It is the same substrate the
delegation/team structure (project → domain → task) and inter-agent
messaging need, so it is designed once, here.

## Decisions

1. **Lineage is a first-class relationship, orthogonal to spatial
   layout, shaped as a tree.** The layout tree (tabs/panes) expresses
   *where* a pane sits; lineage expresses *what a session derives
   from*. These are independent: adjacent panes need not be related,
   related sessions may live in different tabs. Lineage lives in the
   session domain, not the layout. (This is why recursive layout was
   deliberately kept grouping-free — N-ary, minimally nested: the
   hierarchy the UI might have wanted was reserved for *this* model, per
   `docs/recursive-layout-design.md`'s topology/size/nav separation.)
   Each session has at most one parent; roots have none.

2. **The tree's edge is one "derivation" relationship, and it is the
   same tree delegation and messaging use.** A derivation edge carries,
   over time: the worktree base (a child's worktree branches from the
   parent's HEAD), and — layered on the *same* edge later — messaging
   reachability, supervision/visibility, and lifecycle coupling. There
   is one tree, not a separate worktree graph and messaging graph
   (delegation = supervisor→delegate is exactly a derivation edge with
   all four properties). Only the worktree base is in scope now;
   messaging/supervision are layered on the same tree when
   shared-foundation 4's consultation happens.
   **Reference is not an edge.** "Let me see another session's output"
   (a sibling's or an unrelated session's) is a workspace *operation*
   (decision 4a), not a lineage edge — so the tree stays pure
   derivation and does not accrete reference links.

3. **Creation model.** One knob at spawn time, plus a worktree a
   session enters on its own (board #65, `docs/sandbox-perimeter-design.md`):
   - *workspace_root source* — the directory the session starts in.
     Intended default: inherit the spawn-source pane's cwd
     (terminal-cwd inheritance, "start where I'm looking"); repo root /
     fresh when there is no source. The per-session `workspace_root`
     plumbing landed 2026-07-07 (`9110c7c`, `SessionNew.workspace_root`).
     Terminals inherit the source pane's cwd today; an agent spawn
     starts in the Horizon process's cwd (`WorkspaceShell::reconcile`)
     whatever its origin.
   - *worktree* — a session gets its **own worktree** only by entering
     one after it starts: the `worktree.enter` tool, or the board's
     `implement` action, both through environment activation. Spawning
     never creates a worktree; there is no per-spawn or per-origin
     isolation choice.
   - *base ref is derived, not chosen*: `worktree.enter` branches from
     the session root's own working-tree `HEAD`
     (`git rev-parse HEAD`), including local commits that have not been
     pushed to `origin/main` (the "launching session's source commit"
     rule from issue 006); `implement` branches from the base the task
     names. The repository's `origin/<default>` is *not* consulted.
     (An earlier implementation read `origin/<default>` and silently
     dropped any local commits past it -- Agent #67 worked from a stale
     `4b0acac` while its launcher sat at `f32f411`.)
   - *derivation edge*: environment activation records no
     `parent_session_id`. The edges that exist are a board review
     session's (its caller) and those persisted by sessions spawned into
     a worktree before board #65, which restore keeps reporting.

4. **Control from the workspace.**
   - (a) *The cd-pain fix is a first-class operation.* Horizon knows
     every session's `workspace_root` (including derived worktree
     paths), so "open / start a terminal in any session's directory" is
     a command (command model → palette / CLI / keybinding). This kills
     "hunt down the worktree and cd" regardless of lineage — it is the
     concrete form of decision 2's reference-as-operation.
   - (b) *Lineage is surfaced in the session manager modal.* The modal
     already lists/manages sessions; grow it to show the derivation tree
     (not a flat list), a per-row "open its directory" action (a), and
     parent/child navigation. Preferred over a new dedicated view —
     grow the existing surface, as placement-first grew the palette.

5. **Lifecycle and cleanup.** The relationship changes cleanup less than
   feared, because git worktrees are filesystem-independent: a child
   worktree branches from a commit in the shared object store and does
   not depend on the parent worktree's directory, so removing a parent
   worktree never breaks a child. Cleanup stays per-worktree-safe; the
   graph matters only for a convenience.
   - *Close (detach) is non-destructive* and touches neither worktrees
     nor children (Horizon's close-vs-terminate principle; sessions
     survive via the daemon). Parent and children all persist.
   - *Terminate is per-session*: removes that session's worktree if
     clean (no uncommitted changes; commits are preserved on the branch,
     which is never auto-deleted), keeps it if dirty. Children do **not**
     cascade-terminate by default — each is independent, its worktree
     git-independent.
   - *Subtree-terminate* is offered as an explicit opt-in (clean a whole
     lineage branch at once), being more destructive.

## Implementation notes

- **cwd sourcing is shell-independent.** Capture the PTY child pid
  (`portable-pty`'s spawned child exposes `process_id()`; not retained
  today) and sample its cwd on demand via a cross-platform process-info
  crate (`sysinfo`-style: `/proc/<pid>/cwd` on Linux, libproc on macOS)
  — Horizon targets Linux *and* macOS, so direct `/proc` is out. Not
  OSC 7 / shell hooks: OSC 7 is the shell-*dependent* mechanism (would
  tie Horizon to a fixed shell set), which the owner explicitly does not
  want. OSC 7 remains the path only if continuous cwd (e.g. header
  display) is later wanted.
- **Kind-agnostic, agents-first.** The model treats terminal and agent
  sessions as lineage nodes symmetrically (the daemon design made them
  SessionId-symmetric). Terminals participate as parents / cwd sources
  from the start; whether a terminal itself can own a worktree is
  deferred (agents are the immediate motive).

## Delivery

Foundation landed: per-session `workspace_root` on `SessionNew`
(`9110c7c`).

Decision 4a's v1 slice landed (active-session scope only): a
`CommandId::OpenTerminalInSessionDirectory` command ("Open Terminal in
Session Directory", palette + `horizon open-terminal-in-session-directory`
CLI parity) opens a new terminal tab whose cwd is pinned to the active
session's `workspace_root`, disabled when there is no active session or
its `workspace_root` is unknown. This required making `workspace_root`
visible on the *shell* side of the model for the first time:
`WorkspaceSession`/`SessionSummary` now carry an additive
`workspace_root: Option<PathBuf>`, set once (`Workspace::
set_session_workspace_root`) right before a brand-new agent session's
`SessionNew` goes out (`WorkspaceShell::reconcile`), using the same value
that's sent over the wire -- so the model and the daemon never disagree
on what a session's root is. Two scope calls worth recording: (1) it's
agent-only for now -- terminal sessions have no `workspace_root` sourcing
mechanism to read from yet (the pid-sampling cwd inheritance in
`horizon-agentd::terminal::resolve_cwd` is spawn-time-only and keyed by
*terminal* session id, not exposed to the shell), so a terminal active
session simply disables the command rather than fabricating an
approximate answer. The original second limitation (no persistence across a
session-runtime restart) is closed by the 2026-07-21 resume hardening below.
Per-row "open its directory" on an arbitrary session (decision 4b) landed
alongside the lineage view itself -- see below.

Decisions 1-2 and 5's core: the lineage tree lives daemon-side, in
`horizon-agentd`'s in-memory `SessionEntry` (`parent_session_id`/
`workspace_root`/`worktree`) -- over the wire as `SessionInfoMeta`'s
`parent_session_id`/`workspace_root` and `SessionNewMeta.
spawn_source_session_id`. A session's worktree is a real git worktree
at `<repo_root>/.horizon/worktrees/<slug>` on branch `horizon/<slug>`
(`.horizon` ignored via that repo's own `.git/info/exclude`, never its
tracked `.gitignore`), created by environment activation
(`create_isolated_worktree_at`; decision 3 above). Decision 5's
terminate cleanup runs `git worktree remove` (never the branch) when a
session's own thread ends, which already refuses when the worktree is
dirty.

There is no spawn-time isolation (board #65,
`docs/sandbox-perimeter-design.md`): `SessionNewMeta` has no isolation
field (`HORIZON_ACP_EXT_VERSION` 2), `new-agent` has no `--share`, and the
view chooser offers one plain "Agent" choice.

Worktree creation resolves after `session/new` has returned, so the
shell's pre-spawn `workspace_root` value is the directory the session
started in, not a later worktree path. `SessionSummary.
workspace_root` is `horizon-agentd`'s authoritative answer once
resolved; `WorkspaceShell::spawn_agent_resume`/`spawn_workspace_restore`
(the two places the shell already re-lists sessions from the daemon) now
overwrite the model's stored root with it, so `OpenTerminalInSessionDirectory`
opens the real worktree once one of those sweeps has run.

Live correction: environment activation
(`SessionEnvironment::activate`, `crates/horizon-agentd/src/session/environment.rs`)
pushes a session-scoped `WorkspaceRootResolved { workspace_root,
parent_session_id }` the moment the worktree exists, mirroring
`SessionModel`'s live-announce shape; attachment replay re-sends it. Not
sent when activation fails -- nothing to correct then. Shell-side, this is a new process-wide
channel alongside `host_tools` (not the per-session `agent`/`ProviderEvent`
map `SessionModel` rides, since a workspace-root correction is a model
fold, not a transcript event): `runtime::routing::AgentRoutes` gained a
`workspace_roots` sender, `AgentdHandle::start`/`start_on_stream` return
its receiver as a third tuple element, and `WorkspaceShell::
wire_workspace_root_updates` (wired at both the same call sites as
`wire_host_tools`: `WorkspaceShell::new` and `reload_session_runtime`)
folds each arrival straight into `Workspace::set_session_workspace_root`/
`set_session_parent` and `cx.notify()`s. A session created and used within
one continuous run now sees its corrected root/parent immediately, not
just at the next resume/reload sweep -- the sweeps remain the catch-up
path for a daemon restart or UI restart, where no live connection existed
to push over in the first place.

2026-07-21 restart hardening closes the remaining daemon-restart gap. Every
new event-log record carries host-authored `session_context` containing the
authoritative root, whether the session owns a worktree
(`isolated_worktree`), and its parent edge. On resume, a session without a
worktree restores its root directly; one that owns a worktree is restored
from its `EnvironmentActivated` identity, or (records from sessions spawned
into a worktree before board #65) re-adopted, only after Git and canonical-path checks
prove it is a linked worktree at the exact session-id-derived
`<repo>/.horizon/worktrees/<slug>` path and its gitdir belongs to that repo's
common worktree directory. Missing or inconsistent worktree context fails
closed by leaving the session unresumed, never by silently downgrading it to
the daemon cwd without containment. Logs predating the additive field retain
the old non-isolated fallback for compatibility and are upgraded when the
resumed session next appends a record.

Decision 4b's UI slice landed: the session manager modal
(`src/session_manager.rs`) renders the derivation tree instead of a flat
list. `WorkspaceSession`/`SessionSummary` grew an additive
`parent_session_id: Option<SessionId>` mirror of the wire field
(`Workspace::set_session_parent`, following `workspace_root`'s own
mirror), populated in the same two adoption/resume sweeps
(`spawn_agent_resume`/`spawn_workspace_restore` in
`src/workspace/session_lifecycle.rs`). `order_as_lineage_tree` (pure,
unit-tested) turns the flat
summary list into rows -- roots first, each followed immediately by its
own descendants, indented one level per generation (list-with-
indentation, no custom tree widget); a parent missing from the current
(possibly search-filtered) list demotes a child to its own root, keeping
"unrelated sessions as today" true even under search. Two new row-scoped
actions ride the modal's existing List, alongside its primary confirm
(attach/jump) and secondary confirm (plain per-session terminate): `secondary-
o` opens the selected row's directory (generalizing decision 4a's active-
session-only v1 off `WorkspaceShell::open_terminal_in_directory`, a no-op
without a known `workspace_root`), and `secondary-shift-t` terminates the
selected row's whole subtree (decision 5's explicit, more-destructive-than-
plain-terminate opt-in -- `subtree_session_ids`, pure and unit-tested,
collects the row plus every transitive descendant; a no-op unless the row
actually has children, so it can never substitute for the plain terminate a
leaf row already gets). Both are gpui actions bound to a dedicated
`SessionManager` key context (modifier-only keystrokes, since the modal's
search input would otherwise consume a bare key) rather than new
`CommandId` variants -- like the existing terminate, they act on "whichever
row is currently selected," a piece of modal-local state the workspace-wide
command model has no notion of.

2026-07-19 dogfooding fix: session manager rows now show the session's
`workspace_root` itself, not just the small `dir`/`subtree` action-hint
text that previously stood in for it -- so a session's own worktree reads
as visibly distinct from a plain checkout directory at a
glance, per decision 4b's own goal. `abbreviate_workspace_root` (pure,
unit-tested, `src/session_manager.rs`) renders it relative to the
enclosing repository root when the path sits strictly inside one (e.g.
`.horizon/worktrees/calm-otter`), else `~`-relative under `$HOME`, else the
full path -- a full absolute path is too long for the list. The enclosing
repo root itself is found by a separate, deliberately `git`-subprocess-free
walk up the path's ancestors for the nearest directory-shaped `.git` (a
linked worktree's own `.git` is a *file*, so this walks past a session's
own worktree to the *main* checkout it branched from, matching
the example above); cheap enough to run on every visible row's render.

Remaining is a roadmap item under shared-foundation 4: terminal-cwd
sourcing surfaced to the shell (non-agent sessions still have no
`workspace_root` at all), agent spawns inheriting the source pane's
directory (decision 3's intended default), and parent/child navigation from the session
manager (jumping directly to an ancestor/descendant row). Messaging/
supervision layer onto the same tree in shared-foundation 4's own
consultation. A separate, owner-deferred design consultation on
spawn lineage semantics (does a plain spawn deserve its own edge kind,
distinct from a real derivation edge?) is recorded as backlog 54.
