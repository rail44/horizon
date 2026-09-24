//! Resolve session-creation intent and stage spawn metadata for reconciliation.

use super::WorkspaceShell;
use crate::{theme, view_chooser::Placement};
use gpui::*;
use horizon_terminal_core::{TerminalSize, TerminalSpawnSpec, DEFAULT_SCROLLBACK_LINES};
use horizon_workspace::{types::SessionKind, PaneKind, SessionId, SplitAxis};

#[derive(Clone)]
pub(super) struct PendingTerminalSpawn {
    pub(super) source_session_id: Option<SessionId>,
    pub(super) fallback_cwd: std::path::PathBuf,
}

/// Staged the same way as [`PendingTerminalSpawn`] (before a
/// session-creating workspace mutation, consumed by `reconcile`), but for
/// agent spawns' own two knobs (`docs/session-relationship-design.md`
/// decision 3): the pane this spawn derives from, and whether agentd
/// should give it an isolated worktree. `Default` is "no source, not
/// isolated" -- `reconcile`'s fallback when nothing staged anything (e.g. a
/// resumed/attached session, never a fresh spawn).
#[derive(Clone, Default)]
pub(super) struct PendingAgentSpawn {
    pub(super) source_session_id: Option<SessionId>,
    pub(super) isolate: bool,
}

/// Kind-agnostic spawn-source resolution: an explicit target (e.g. the
/// `--split`/split-target session) wins, else the caller-supplied fallback.
/// The palette passes the active session as the fallback (a "child of the
/// current pane" gesture); the control plane passes the issuer instead (see
/// [`control_plane_spawn_source`]). Shared by terminal cwd inheritance and
/// agent lineage/isolation -- both need exactly the same "spawned from"
/// pane.
fn resolve_spawn_source(
    explicit_source: Option<SessionId>,
    fallback: Option<SessionId>,
) -> Option<SessionId> {
    explicit_source.or(fallback)
}

/// Control-plane (CLI) spawn-source resolution: an explicit `--split`
/// target wins, else the issuing session (carried in the request from
/// `HORIZON_SESSION_ID`), else `None` (root). The focused pane is
/// deliberately *not* consulted -- a CLI dispatch parents to its issuer,
/// never to whatever pane happened to be focused (issue 013).
fn control_plane_spawn_source(
    explicit_split: Option<SessionId>,
    issuer: Option<SessionId>,
) -> Option<SessionId> {
    resolve_spawn_source(explicit_split, issuer)
}

fn terminal_fallback_cwd(
    current_dir: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    current_dir
        .or(home)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// `docs/session-relationship-design.md` decision 4a's "Open Terminal in
/// Session Directory" spawn request: the cwd is pinned to `workspace_root`
/// directly, with no spawn-source pid inheritance (`source_session_id:
/// None`) -- unlike a plain new terminal, the target directory is already
/// known exactly, so there is nothing to inherit.
fn pinned_terminal_spawn(workspace_root: std::path::PathBuf) -> PendingTerminalSpawn {
    PendingTerminalSpawn {
        source_session_id: None,
        fallback_cwd: workspace_root,
    }
}

impl WorkspaceShell {
    fn pending_terminal_spawn(&self, source_session_id: Option<SessionId>) -> PendingTerminalSpawn {
        PendingTerminalSpawn {
            source_session_id,
            fallback_cwd: Self::default_terminal_cwd(),
        }
    }

    /// Stages an agent spawn's source pane and isolation choice for
    /// `reconcile` to consume -- `isolate` here is already the fully
    /// resolved per-spawn choice (origin default folded with any explicit
    /// override; see `create_session`/`control_plane_new_session`), not a
    /// further default to apply.
    fn pending_agent_spawn(
        &self,
        source_session_id: Option<SessionId>,
        isolate: bool,
    ) -> PendingAgentSpawn {
        PendingAgentSpawn {
            source_session_id,
            isolate,
        }
    }

    pub(super) fn default_terminal_cwd() -> std::path::PathBuf {
        terminal_fallback_cwd(
            std::env::current_dir().ok(),
            std::env::var_os("HOME").map(std::path::PathBuf::from),
        )
    }

    pub(super) fn terminal_spawn_spec(&self, pending: PendingTerminalSpawn) -> TerminalSpawnSpec {
        // `[terminal] shell_args`/`term`/`scrollback_lines` were retired in
        // the 2026-07-18 config-narrowing wave (see AGENTS.md's
        // "Configuration" section): each is now fixed. `shell` keeps its
        // existing $SHELL-else-/bin/sh logic, minus the former file
        // override.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        TerminalSpawnSpec {
            shell,
            args: Vec::new(),
            term: "xterm-256color".to_string(),
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            color_scheme: theme::terminal_color_scheme(),
            control_socket: self.socket_path.clone(),
            fallback_cwd: pending.fallback_cwd,
            spawn_source_session_id: pending.source_session_id.map(SessionId::as_uuid),
            initial_size: TerminalSize::new(80, 24),
        }
    }

    /// The one interactive session-creation path: what the view chooser
    /// confirms with. Terminal cwd and agent role ride the same staging
    /// maps `reconcile` consumes. `isolate` is the view chooser's own
    /// per-choice override of decision 3's palette-origin default (shared);
    /// `false` for every choice except the dedicated "Agent (Isolated
    /// Worktree)…" one (`ViewChoice::isolate`) -- ignored for a
    /// session-less `PaneKind::View` choice, same as `role_id`.
    pub(super) fn create_session(
        &mut self,
        kind: PaneKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        isolate: bool,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_phase.blocks_mutation() {
            return;
        }
        self.workspace.exit_workspace_mode();
        let kind = match kind {
            PaneKind::Terminal => SessionKind::Terminal,
            PaneKind::Agent => SessionKind::Agent,
            PaneKind::View(view_kind) => {
                // A session-less first-party view: no session id to create,
                // no agent-runtime spawn, and no `pending_terminal_spawns`/
                // `pending_roles` bookkeeping -- those exist only for the
                // session-backed kinds handled below.
                match placement {
                    Placement::NewTab => {
                        self.workspace.open_tab_with_view_activated(view_kind, true);
                    }
                    Placement::SplitRight | Placement::SplitDown => {
                        let axis = if placement == Placement::SplitRight {
                            SplitAxis::Horizontal
                        } else {
                            SplitAxis::Vertical
                        };
                        self.workspace.split_active_tab_with_view(view_kind, axis);
                    }
                }
                self.reconcile(window, cx);
                self.focus_active(window, cx);
                return;
            }
        };
        // Palette origin: the new session is a child of the focused pane
        // (the "current pane" gesture) -- the active session is the spawn
        // source, no explicit target. Contrast `control_plane_new_session`'s
        // control-plane path, which parents to the issuer instead (issue
        // 013).
        let active = self.workspace.active_session_id();
        let terminal_spawn =
            matches!(kind, SessionKind::Terminal).then(|| self.pending_terminal_spawn(active));
        // Palette origin defaults to shared, not isolated (`docs/session-
        // relationship-design.md` decision 3); `isolate` is the caller's
        // explicit opt-in (the view chooser's dedicated "Agent (Isolated
        // Worktree)…" choice), not a further default to apply here.
        let agent_spawn =
            matches!(kind, SessionKind::Agent).then(|| self.pending_agent_spawn(active, isolate));
        let session_id = match placement {
            Placement::NewTab => Some(
                self.workspace
                    .open_tab_with_new_session_activated(kind, true),
            ),
            Placement::SplitRight | Placement::SplitDown => {
                let axis = if placement == Placement::SplitRight {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                };
                self.workspace.active_session_id().and_then(|target| {
                    self.workspace
                        .split_session_with_new_session(target, kind, axis, true)
                })
            }
        };
        if let Some(session_id) = session_id {
            if let Some(spawn) = terminal_spawn {
                self.pending_terminal_spawns.insert(session_id, spawn);
            }
            if let Some(spawn) = agent_spawn {
                self.pending_agent_spawns.insert(session_id, spawn);
            }
            if let Some(role_id) = role_id {
                self.pending_roles.insert(session_id, role_id);
            }
        }
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// `CommandId::OpenTerminalInSessionDirectory`
    /// (`docs/session-relationship-design.md` decision 4a): opens a new
    /// terminal tab pinned to `workspace_root`'s cwd -- v1's placement is a
    /// new tab, matching `create_session`'s own `Placement::NewTab` default
    /// for a plain new terminal. `execute` (`workspace/commands.rs`) is the
    /// only caller, having already resolved the active session's
    /// `workspace_root`; this stays a plain `PathBuf` parameter (not a
    /// `SessionId` re-lookup) since v1 targets only the active session --
    /// per-row "open its directory" on an arbitrary session is a later
    /// slice (see the design doc's decision 4b).
    pub(super) fn open_terminal_in_directory(
        &mut self,
        workspace_root: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_phase.blocks_mutation() {
            return;
        }
        self.workspace.exit_workspace_mode();
        let session_id = self
            .workspace
            .open_tab_with_new_session_activated(SessionKind::Terminal, true);
        self.pending_terminal_spawns
            .insert(session_id, pinned_terminal_spawn(workspace_root));
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// Control-plane operations — the CLI's verbs, mirroring the Floem
    /// shell's `external_commands` semantics. The Rust family was renamed
    /// from `external_*` to `control_plane_*` so the prefix names the
    /// caller (the CLI's stable verb surface), not the session; the
    /// published string names are untouched. `activate: false` never
    /// steals focus. `prompt` (agent sessions only) sends
    /// the first user message right after the session starts — the
    /// create-with-prompt composite from the CLI design. `role_id` is
    /// fixed by the caller (e.g. `new-agent --role config`), never client-supplied
    /// — see `pending_roles`. `isolate` is agent sessions' own per-spawn
    /// override of `docs/session-relationship-design.md` decision 3's
    /// origin default (CLI/control-plane origin defaults to isolated,
    /// mirroring `activate`'s opposite default): `None` applies that
    /// default, `Some` is an explicit override (the CLI's `--share`) --
    /// `control_plane::dispatch_invoke` already rejects a non-`None` value
    /// for a terminal spawn, so this never has to.
    ///
    /// `issuer` is the session that dispatched this request (the CLI's
    /// `HORIZON_SESSION_ID`, carried in the request's `"issuer"` arg).
    /// The spawn source resolves as [`control_plane_spawn_source`]:
    /// explicit `--split` target wins, else `issuer`, else `None` (root)
    /// -- the focused pane is never consulted, unlike
    /// [`WorkspaceShell::create_session`]'s palette path (issue 013).
    /// When `issuer` is a *terminal* session (the common CLI case: a
    /// command-line tool running in a terminal pane dispatches
    /// `new-agent`), `horizon-agentd` does not host it, so
    /// `session_directory` returns `None` for its id and the
    /// isolated-worktree base falls back to the root-spawn behavior
    /// (`resolve_and_create_isolated_worktree`'s `fallback_dir`). That
    /// degradation is acceptable: the terminal's cwd would be the honest
    /// base, but carrying it on the wire is a separate task; this change
    /// does not touch the wire. The lineage edge (parent = the terminal)
    /// is still recorded correctly on worktree-creation success, since
    /// the parent is `spawn_source_session_id` itself, not the
    /// worktree-derivation result.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn control_plane_new_session(
        &mut self,
        kind: SessionKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        split: Option<(SessionId, SplitAxis)>,
        issuer: Option<SessionId>,
        activate: bool,
        prompt: Option<String>,
        isolate: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if self.workspace_phase.blocks_mutation() {
            return Err("workspace restore is still in progress".to_string());
        }
        let source = control_plane_spawn_source(split.map(|(target, _)| target), issuer);
        let terminal_spawn =
            matches!(kind, SessionKind::Terminal).then(|| self.pending_terminal_spawn(source));
        let agent_spawn = matches!(kind, SessionKind::Agent)
            .then(|| self.pending_agent_spawn(source, isolate.unwrap_or(true)));
        let session_id = match split {
            Some((target, axis)) => self
                .workspace
                .split_session_with_new_session(target, kind, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_new_session_activated(kind, activate),
        };
        if let Some(spawn) = terminal_spawn {
            self.pending_terminal_spawns.insert(session_id, spawn);
        }
        if let Some(spawn) = agent_spawn {
            self.pending_agent_spawns.insert(session_id, spawn);
        }
        if let Some(role_id) = role_id {
            self.pending_roles.insert(session_id, role_id);
        }
        self.reconcile(window, cx);
        if let Some(prompt) = prompt {
            if let Some(session) = self.agent_sessions.get(&session_id) {
                session.read(cx).send_user_message(prompt);
            }
        }
        if activate {
            self.focus_active(window, cx);
        }
        Ok(session_id.as_uuid().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        control_plane_spawn_source, pinned_terminal_spawn, resolve_spawn_source,
        terminal_fallback_cwd,
    };
    use horizon_workspace::SessionId;
    #[test]
    fn explicit_split_target_wins_over_the_fallback() {
        let explicit = SessionId::new();
        let fallback = SessionId::new();
        assert_eq!(
            resolve_spawn_source(Some(explicit), Some(fallback)),
            Some(explicit)
        );
    }

    #[test]
    fn missing_explicit_source_falls_back_to_the_provided_session() {
        let fallback = SessionId::new();
        assert_eq!(resolve_spawn_source(None, Some(fallback)), Some(fallback));
    }

    #[test]
    fn no_explicit_source_and_no_fallback_is_a_root_spawn() {
        assert_eq!(resolve_spawn_source(None, None), None);
    }

    /// Issue 013: the control-plane path must parent to the issuer (or the
    /// explicit `--split` target), never to the focused pane. The focused
    /// pane is not even a parameter to `control_plane_spawn_source` -- the
    /// function signature itself enforces that the active session is never
    /// consulted.
    #[test]
    fn control_plane_source_prefers_split_then_issuer_then_root_ignoring_focus() {
        let split = SessionId::new();
        let issuer = SessionId::new();

        // Explicit split wins over issuer.
        assert_eq!(
            control_plane_spawn_source(Some(split), Some(issuer)),
            Some(split)
        );
        // No split: issuer is the parent.
        assert_eq!(control_plane_spawn_source(None, Some(issuer)), Some(issuer));
        // No split, no issuer: root spawn (never the focused pane).
        assert_eq!(control_plane_spawn_source(None, None), None);
    }

    #[test]
    fn terminal_fallback_prefers_current_dir_then_home_then_dot() {
        let cwd = std::path::PathBuf::from("/workspace");
        let home = std::path::PathBuf::from("/home/test");
        assert_eq!(
            terminal_fallback_cwd(Some(cwd.clone()), Some(home.clone())),
            cwd
        );
        assert_eq!(terminal_fallback_cwd(None, Some(home.clone())), home);
        assert_eq!(
            terminal_fallback_cwd(None, None),
            std::path::PathBuf::from(".")
        );
    }

    #[test]
    fn open_terminal_in_directory_pins_the_target_cwd_with_no_source_inheritance() {
        // `docs/session-relationship-design.md` decision 4a: the spawn
        // request's cwd must be exactly the target session's
        // `workspace_root`, with no spawn-source pid inheritance (unlike a
        // plain new terminal, which sources from the active pane instead --
        // see `resolve_spawn_source`) since the directory is already known.
        let workspace_root = std::path::PathBuf::from("/some/agent/workspace");

        let spawn = pinned_terminal_spawn(workspace_root.clone());

        assert_eq!(spawn.fallback_cwd, workspace_root);
        assert_eq!(spawn.source_session_id, None);
    }
}
