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

3. **Creation model.** Two independent knobs at spawn time:
   - *workspace_root source* — default: inherit the spawn-source pane's
     cwd (terminal-cwd inheritance, "start where I'm looking"); repo
     root / fresh when there is no source. The per-session
     `workspace_root` plumbing landed 2026-07-07 (`9110c7c`,
     `SessionNew.workspace_root`).
   - *isolation* — whether the new session gets its **own worktree**
     (branched appropriately) or **shares** the source's directory.
     Isolation is what creates the derivation edge: an isolated child
     has a child worktree branched from the parent; a non-isolated
     spawn merely shares a directory and is not a lineage child.
   - *base ref is derived, not chosen*: root (non-derived) → fresh from
     `origin/<default>`; derived child → from the parent's worktree
     HEAD. `fresh` alone is wrong for multi-level work (a child of an
     agent's worktree must branch from that worktree, not origin/main).
   - *isolation's default is an origin property*, mirroring the
     `activate` decision (`docs/cli-control-plane-design.md`): **palette
     origin → default parallel/shared, opt in to isolate; CLI origin →
     default isolated, opt out to share.** This fits delegation exactly
     — an agent spawning a delegate over the control plane gets an
     isolated child worktree by default; a human opening an agent beside
     their terminal works in the shared directory by default. Isolation
     is a per-spawn attribute (not a role/global setting) with this
     origin-based default and an explicit override.

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
  from the start; whether a terminal itself can be spawned as an
  isolated child is deferred (agents are the immediate isolation
  motive).

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
`horizon-sessiond::terminal::resolve_cwd` is spawn-time-only and keyed by
*terminal* session id, not exposed to the shell), so a terminal active
session simply disables the command rather than fabricating an
approximate answer; (2) it's not persisted -- a session resumed via
`Reload Session Runtime` or a workspace restore goes back to
`workspace_root: None` until it's recreated, since only the
brand-new-agent-session path in `reconcile` ever calls the setter.
Per-row "open its directory" on an arbitrary session (decision 4b) landed
alongside the lineage view itself -- see below.

Decisions 1-3 and 5's core landed: the lineage tree lives daemon-side, in
`horizon-sessiond`'s in-memory `SessionEntry` (`parent_session_id`/
`workspace_root`/`worktree`) -- additive over the wire as `SessionSummary.
parent_session_id`/`workspace_root` and `SessionNew.spawn_source_session_id`/
`isolate` (no `CONTRACT_VERSION` bump; `SessionEntry`'s lineage doesn't
survive a `horizon-sessiond` process restart, the same accepted gap
`workspace_root` already had). An isolated spawn gets a real git worktree
at `<repo_root>/.horizon/worktrees/<slug>` on branch `horizon/<slug>`
(`.horizon` ignored via that repo's own `.git/info/exclude`, never its
tracked `.gitignore`); base ref is fresh from `origin/<default-branch>`
for a lineage root, or the source session's own worktree `HEAD` for a
derived child, per decision 3. The edge is recorded only when isolation
actually succeeds -- a failed worktree creation degrades to an ordinary
shared spawn with no edge. Decision 5's terminate cleanup runs `git
worktree remove` (never the branch) when a session's own thread ends,
which already refuses when the worktree is dirty. The origin-based
default is wired the same way `activate`'s was: palette spawns default
shared, with an explicit opt-in (see decision 4b's UI slice below); CLI/
control-plane spawns default isolated, with an explicit `--share`
opt-out. Terminal-as-isolated-child stays deferred, per the "agents-first"
note above.

This closes a gap decision 4a's landing above had left open: worktree
creation resolves *after* `Control::SessionNew` already returned (it's
real IO on the new session's own thread), so the shell's pre-spawn
`workspace_root` value is only ever the inherited-cwd guess for an
isolated session, not the real worktree path. `SessionSummary.
workspace_root` is `horizon-sessiond`'s authoritative answer once
resolved; `WorkspaceShell::spawn_agent_resume`/`spawn_workspace_restore`
(the two places the shell already re-lists sessions from the daemon) now
overwrite the model's stored root with it, so `OpenTerminalInSessionDirectory`
opens the real worktree once one of those sweeps has run.

2026-07-19 dogfooding fix: the "still eventual, not live" gap above is
closed. `resolve_and_create_isolated_worktree`
(`crates/horizon-sessiond/src/session.rs`) now pushes a session-scoped
`wire::Control::WorkspaceRootResolved { workspace_root, parent_session_id }`
the moment isolation resolves, mirroring `Control::SessionModel`'s own
live-announce shape and additive-`#[serde(default)]`/no-version-bump
convention. Not sent at all when isolation fails and degrades to a shared
spawn -- nothing to correct then. Shell-side, this is a new process-wide
channel alongside `host_tools` (not the per-session `agent`/`ProviderEvent`
map `SessionModel` rides, since a workspace-root correction is a model
fold, not a transcript event): `sessiond::routing::Routes` gained a
`workspace_roots` sender, `SessiondHandle::start`/`start_on_stream` return
its receiver as a third tuple element, and `WorkspaceShell::
wire_workspace_root_updates` (wired at both the same call sites as
`wire_host_tools`: `WorkspaceShell::new` and `reload_session_runtime`)
folds each arrival straight into `Workspace::set_session_workspace_root`/
`set_session_parent` and `cx.notify()`s. A session created and used within
one continuous run now sees its corrected root/parent immediately, not
just at the next resume/reload sweep -- the sweeps remain the catch-up
path for a daemon restart or UI restart, where no live connection existed
to push over in the first place.

Decision 4b's UI slice landed: the session manager modal
(`src/session_manager.rs`) renders the derivation tree instead of a flat
list. `WorkspaceSession`/`SessionSummary` grew an additive
`parent_session_id: Option<SessionId>` mirror of the wire field
(`Workspace::set_session_parent`, following `workspace_root`'s own
mirror), populated in the same two adoption/resume sweeps
(`spawn_agent_resume`/`spawn_workspace_restore` in
`src/workspace/session_lifecycle.rs`) and carrying the same "not
persisted, eventual not live within one run" caveats `workspace_root`
already has. `order_as_lineage_tree` (pure, unit-tested) turns the flat
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

The palette's own isolation opt-in (decision 3's "no override surface yet"
gap) also landed: the view chooser (`New Tab…`/`Split Right…`/`Split
Down…`) gained a dedicated "Agent (Isolated Worktree)…" choice alongside
plain "Agent" -- `ViewChoice` grew an `isolate: bool` (`true` only for that
one choice), threaded through `WorkspaceShell::create_session` to the same
`pending_agent_spawn` staging `external_new_session` (the CLI path) already
used. No redesign of the placement flow itself.

2026-07-19 dogfooding fix: session manager rows now show the session's
`workspace_root` itself, not just the small `dir`/`subtree` action-hint
text that previously stood in for it -- so an isolated child's own
worktree reads as visibly distinct from a shared session's directory at a
glance, per decision 4b's own goal. `abbreviate_workspace_root` (pure,
unit-tested, `src/session_manager.rs`) renders it relative to the
enclosing repository root when the path sits strictly inside one (e.g.
`.horizon/worktrees/calm-otter`), else `~`-relative under `$HOME`, else the
full path -- a full absolute path is too long for the list. The enclosing
repo root itself is found by a separate, deliberately `git`-subprocess-free
walk up the path's ancestors for the nearest directory-shaped `.git` (a
linked worktree's own `.git` is a *file*, so this walks past an isolated
session's own worktree to the *main* checkout it branched from, matching
the example above); cheap enough to run on every visible row's render.

Remaining is a roadmap item under shared-foundation 4: terminal-cwd
sourcing surfaced to the shell (non-agent sessions still have no
`workspace_root` at all), and parent/child navigation from the session
manager (jumping directly to an ancestor/descendant row). Messaging/
supervision layer onto the same tree in shared-foundation 4's own
consultation. A separate, owner-deferred design consultation on
shared-spawn lineage semantics (does a plain, non-isolated spawn deserve
its own edge kind, distinct from a real derivation edge?) is recorded as
backlog 54 -- not a request to change decisions 2/3 now.
