//! The GPUI projection of the shared workspace model
//! (`crates/horizon-workspace`): tab strip, recursive split rendering as a
//! plain flex tree with self-owned resize handles (weight-native, no
//! `gpui_component::resizable` -- see `docs/split-resize-design.md`), pane
//! focus, and workspace mode with spatial navigation. The model owns all
//! layout truth; this module only renders it and translates GPUI actions
//! into model operations.
//!
//! The key bindings registered by [`init`] (via `bindings::derive_bindings`) are
//! M2 stand-ins wired straight to model calls — M3 replaces them with the
//! command model (`CommandId` + keymap config), at which point every
//! handler here becomes a binding to a command instead.
//! `bindings::derive_bindings`'s `[keybindings]` layer (`keymap::resolve_keybindings`/
//! `keymap::workspace_mode_keystroke`) is the first piece of that:
//! config-bound chords dispatch through [`RunCommand`] to
//! [`WorkspaceShell::execute`] instead of a model-call handler.
//! `Reload Config` (`CommandId::ReloadConfig`) re-derives and re-applies
//! this same binding set live — see `bindings::apply_bindings`'s doc comment for
//! how a stale chord gets unbound.
//!
//! Split (2026-07-18) into responsibility-focused submodules -- a pure
//! move, no behavior change: [`bindings`] (keybinding derivation/apply),
//! [`session_lifecycle`] (session creation, sessiond resume/reload,
//! `reconcile`), [`commands`] (`execute`/`execute_external` and the
//! session-targeted `external_*` family), [`modals`] (the palette/
//! session-manager/view-chooser lifecycles), and [`render`]
//! (`render_tab_strip`/`render_node`/the `Render` impl, plus the
//! pane-chrome pure functions). This file keeps the `WorkspaceShell`
//! struct, its constructor, `init`, and the cross-cutting glue methods
//! every submodule calls into (`reconcile`'s sibling helpers like
//! `focus_active`/`persist_workspace`) -- the same shape `src/theme/`
//! and `src/agent/turns/` already split into. No call site outside
//! `src/workspace/` changed.

use std::collections::HashMap;

use gpui::*;
use gpui_component::list::ListState;
use horizon_workspace::commands::CommandId;
use horizon_workspace::{PaneId, PaneKind, SessionId, Workspace, WORKSPACE_STATE_VERSION};

use crate::agent::{AgentSession, AgentView};
use crate::palette::PaletteDelegate;
use crate::session_manager::SessionManagerDelegate;
use crate::sessiond::SessiondHandle;
use crate::terminal::{TerminalSession, TerminalView};
use crate::terminal_focus::focus_transition;
use crate::theme_settings::ThemeSettingsView;
use crate::view_chooser::{Placement, ViewChooserDelegate};
use crate::workspace_state::{InvalidState, LoadResult, WorkspaceStateStore};

mod bindings;
mod commands;
mod modals;
mod render;
mod session_lifecycle;

use render::SplitDrag;
use session_lifecycle::{PendingAgentSpawn, PendingTerminalSpawn};

pub(crate) fn init(cx: &mut App) {
    bindings::apply_bindings(cx, horizon_config::load());
}

actions!(
    workspace,
    [
        ToggleWorkspaceMode,
        ModeMoveLeft,
        ModeMoveDown,
        ModeMoveUp,
        ModeMoveRight,
        ModeCommit,
        ModeCancel,
        NewTab,
        NewAgentTab,
        SplitPane,
        ClosePane,
        NextTab,
        OpenPalette,
        // Session-manager row actions
        // (`docs/session-relationship-design.md` decision 4b) -- scoped to
        // `SESSION_MANAGER_CONTEXT` (see `bindings::derive_bindings`) and
        // targeting whichever row is currently selected, rather than
        // carrying a `SessionId` of their own: gpui actions built from a
        // `KeyBinding` (unlike `RunCommand`, dispatched from the palette
        // with the id already resolved) never carry per-invocation data.
        OpenSessionDirectory,
        TerminateSessionSubtree
    ]
);

/// Key context for the session manager modal's own row-scoped actions
/// (`OpenSessionDirectory`/`TerminateSessionSubtree`), applied to the
/// modal's backdrop `div` in `render.rs` alongside their `.on_action`
/// handlers -- mirrors [`MODE_CONTEXT`]'s "context and handler live on the
/// same element" shape rather than relying on cross-level action bubbling.
const SESSION_MANAGER_CONTEXT: &str = "SessionManager";

/// A `[keybindings]`-config-driven binding to a `CommandId` — gpui actions
/// used with `KeyBinding` are compile-time types, so a config chord can't
/// bind directly to one of the many `CommandId` variants the way a unit
/// action binds to one fixed handler. `RunCommand` carries the resolved
/// id as data instead, so a single action type covers every simple
/// command a `[keybindings]` entry can name (see `keymap::command_for`).
/// `no_json`: never built from a JSON keymap (only ever constructed
/// directly in [`init`]), so it skips gpui's `Deserialize`/`JsonSchema`
/// requirements for action fields.
///
/// `pub(crate)` (stage F, `docs/agent-output-ui-amendment.md`): the agent
/// pane's stop button/status-line stop affordance dispatch this same
/// action from `src/agent/view.rs`, rather than reaching into
/// `AgentSession::cancel` directly, so a pointer-driven cancel goes
/// through the same command-model path as the palette and `[keybindings]`
/// chords (AGENTS.md's "operations go through the command model").
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct RunCommand {
    pub(crate) id: CommandId,
}

const MODE_CONTEXT: &str = "WorkspaceMode";

fn load_workspace_state(store: &mut WorkspaceStateStore) -> (Workspace, bool, bool) {
    match store.load(u64::from(WORKSPACE_STATE_VERSION)) {
        Ok(LoadResult::Valid(json)) => match Workspace::from_persisted_json(&json) {
            Ok(workspace) => (workspace, true, false),
            Err(horizon_workspace::WorkspaceStateError::UnsupportedVersion {
                found,
                supported,
            }) => {
                eprintln!(
                    "workspace state version {found} is unsupported (expected {supported}); preserving the file"
                );
                (Workspace::mvp(), false, false)
            }
            Err(error) => {
                eprintln!("ignoring invalid workspace state: {error}");
                (Workspace::mvp(), false, true)
            }
        },
        Ok(LoadResult::Missing) => (Workspace::mvp(), false, true),
        Ok(LoadResult::Invalid(InvalidState::UnsupportedVersion { found, supported })) => {
            eprintln!(
                "workspace state version {found} is unsupported (expected {supported}); preserving the file"
            );
            (Workspace::mvp(), false, false)
        }
        Ok(LoadResult::Invalid(InvalidState::Corrupt(error))) => {
            eprintln!("ignoring corrupt workspace state: {error}");
            (Workspace::mvp(), false, true)
        }
        Err(error) => {
            eprintln!("failed to load workspace state: {error}");
            (Workspace::mvp(), false, false)
        }
    }
}

/// Bring the workspace back to a state with at least one pane after
/// `Reload Session Runtime` (`session_lifecycle::reload_session_runtime`)
/// terminates every terminal session ahead of restarting the daemon --
/// its one remaining caller. A zero-tab workspace is now a valid,
/// persistable state (`WorkspaceState::validate` accepts it), so every
/// *other* termination path (`TerminateActiveSession`, the session
/// manager's secondary-confirm terminate, `external_terminate`, a PTY
/// exit via `handle_terminal_exited`) leaves the workspace empty as-is
/// rather than calling this -- auto-creating a terminal there would
/// silently work against a user closing or terminating everything on
/// purpose (2026-07-18 owner clarification, superseding `704657b`'s
/// blanket guard). The reload path is different: killing every terminal
/// session is an operational side effect of restarting the runtime, not
/// something the user asked to empty, so it still gets a pane back.
fn ensure_workspace_has_pane(workspace: &mut Workspace) -> Option<SessionId> {
    (workspace.tab_count() == 0)
        .then(|| workspace.open_tab_with_new_session_activated(PaneKind::Terminal, true))
}

/// One pane's view, by session kind -- plus one variant per first-party
/// [`ViewKind`] (`docs/theme-settings-view-design.md`), which has no
/// session at all.
#[derive(Clone)]
enum PaneView {
    Terminal(Entity<TerminalView>),
    Agent(Entity<AgentView>),
    ThemeSettings(Entity<ThemeSettingsView>),
}

impl PaneView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            PaneView::Terminal(view) => view.focus_handle(cx),
            PaneView::Agent(view) => view.focus_handle(cx),
            PaneView::ThemeSettings(view) => view.focus_handle(cx),
        }
    }

    fn element(&self) -> AnyElement {
        match self {
            PaneView::Terminal(view) => view.clone().into_any_element(),
            PaneView::Agent(view) => view.clone().into_any_element(),
            PaneView::ThemeSettings(view) => view.clone().into_any_element(),
        }
    }
}

pub(crate) struct WorkspaceShell {
    workspace: Workspace,
    workspace_state: WorkspaceStateStore,
    persistence_ready: bool,
    restoring_workspace: bool,
    workspace_restore_failed: bool,
    // This instance's control socket — every spawned pane gets it as
    // HORIZON_SOCKET so CLIs invoked inside reach back here.
    socket_path: std::path::PathBuf,
    // The session store — the GPUI shell's Registry counterpart: PTY
    // sessions live here keyed by SessionId, independent of pane views,
    // so closing a pane detaches (session survives, scrollback intact)
    // and terminating is the explicit destructive path.
    sessions: HashMap<SessionId, Entity<TerminalSession>>,
    agent_sessions: HashMap<SessionId, Entity<AgentSession>>,
    // Staged by `external_new_session` (a role-tagged create, e.g.
    // `new-config-agent`) and consumed by `reconcile` when it actually
    // starts the session — the model's `open_tab_with_new_session_*`
    // call only yields a `SessionId`, so the role has nowhere else to
    // ride until reconcile turns that id into a live agent session.
    pending_roles: HashMap<SessionId, horizon_agent::roles::RoleId>,
    // Staged before session-creating workspace mutations and consumed by
    // reconcile. Sessiond resolves the source session's live cwd; Horizon
    // carries only the source id and fallback spawn input.
    pending_terminal_spawns: HashMap<SessionId, PendingTerminalSpawn>,
    // Same staging shape, for agent spawns' own two knobs (source pane +
    // isolation) -- see `PendingAgentSpawn`.
    pending_agent_spawns: HashMap<SessionId, PendingAgentSpawn>,
    // Created eagerly before the first reconcile. Its raw FIFO accepts
    // terminal and agent requests while connect/Hello proceeds in the
    // background.
    sessiond: Option<SessiondHandle>,
    reload_in_progress: bool,
    panes: HashMap<PaneId, PaneView>,
    // This window — needed by `Reload Session Runtime`'s post-resume step,
    // which rebuilds pane views from a background thread's async
    // continuation (no `&mut Window` of its own to reuse).
    window: AnyWindowHandle,
    // Focused while workspace mode is active, so mode keys dispatch here
    // instead of reaching the terminal.
    focus_handle: FocusHandle,
    palette: Option<Entity<ListState<PaletteDelegate>>>,
    _palette_subscription: Option<Subscription>,
    session_manager: Option<Entity<ListState<SessionManagerDelegate>>>,
    _session_manager_subscription: Option<Subscription>,
    view_chooser: Option<Entity<ListState<ViewChooserDelegate>>>,
    _view_chooser_subscription: Option<Subscription>,
    // The placement the open view chooser will apply on confirm.
    pending_placement: Option<Placement>,
    // Snapshot of workspace mode's dim/cursor pattern (the cursor pane, if
    // the mode was active), taken by `freeze_scrim_before_modal_exit`
    // right before a modal-opening handler calls
    // `Workspace::exit_workspace_mode`. `render_node`
    // (`effective_scrim_pattern`) substitutes this for the (now-inactive)
    // live mode state while any control-surface modal is open, for both
    // the scrim and the cursor-pane border (2026-07-15 round 3: modal-open
    // is fully chrome-neutral, not just scrim-neutral) -- see
    // `effective_scrim_pattern`'s doc comment and `docs/theme-design.md`'s
    // scrim section. `None` means the mode wasn't active when the modal
    // opened (or no modal is open).
    scrim_freeze: Option<PaneId>,
    // Live state for an in-progress split-handle drag (`render_node`'s
    // `LayoutNode::Split` arm) -- set on a handle's `on_mouse_down`,
    // updated on the split container's `on_mouse_move` (live reflow),
    // cleared and persisted on `on_mouse_up`/`on_mouse_up_out`. View-only
    // scratch state: never touches the `horizon_workspace` model
    // directly, see `SplitDrag`'s own doc comment.
    active_split_drag: Option<SplitDrag>,
    // The terminal session `sync_terminal_focus` last sent `Focus(true)`
    // to, so a transition can send `Focus(false)` to the one it's about
    // to stop being true for. See `focus_transition`.
    last_focused_terminal: Option<SessionId>,
    // Handed to every `TerminalSession::spawn` (cloned per session) so a
    // PTY-side shell exit can notify the shell to terminate that workspace
    // session -- see the `terminal_exit_rx` pump spawned in `new`.
    terminal_exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
}

impl WorkspaceShell {
    pub(crate) fn new(
        socket_path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workspace_state = WorkspaceStateStore::from_environment();
        let (workspace, restoring_workspace, persistence_ready) =
            load_workspace_state(&mut workspace_state);
        let (sessiond, host_tool_rx, workspace_root_rx) =
            SessiondHandle::start(&horizon_agent::socket::default_socket_path(), &socket_path);
        let (terminal_exit_tx, terminal_exit_rx) = futures::channel::mpsc::unbounded();
        let mut shell = Self {
            workspace,
            workspace_state,
            persistence_ready,
            restoring_workspace,
            workspace_restore_failed: false,
            socket_path,
            sessions: HashMap::new(),
            agent_sessions: HashMap::new(),
            pending_roles: HashMap::new(),
            pending_terminal_spawns: HashMap::new(),
            pending_agent_spawns: HashMap::new(),
            sessiond: Some(sessiond.clone()),
            reload_in_progress: false,
            panes: HashMap::new(),
            window: window.window_handle(),
            focus_handle: cx.focus_handle(),
            palette: None,
            _palette_subscription: None,
            session_manager: None,
            _session_manager_subscription: None,
            view_chooser: None,
            _view_chooser_subscription: None,
            pending_placement: None,
            scrim_freeze: None,
            active_split_drag: None,
            last_focused_terminal: None,
            terminal_exit_tx,
        };
        // Window activation/deactivation doesn't otherwise touch the
        // model, so it needs its own observer alongside `focus_active`'s
        // call to `sync_terminal_focus` (every model mutation that can
        // change the active pane).
        cx.observe_window_activation(window, |shell, window, cx| {
            shell.sync_terminal_focus(window, cx);
        })
        .detach();
        shell.wire_host_tools(sessiond.responder(), host_tool_rx, cx);
        shell.wire_workspace_root_updates(workspace_root_rx, cx);
        shell.wire_terminal_exit(terminal_exit_rx, cx);
        if shell.restoring_workspace {
            shell.spawn_workspace_restore(sessiond, cx);
        } else {
            shell.reconcile(window, cx);
            shell.focus_active(window, cx);
            shell.spawn_terminal_resume(sessiond.clone(), cx);
            shell.spawn_agent_resume(sessiond, cx);
        }
        shell
    }

    fn persist_workspace(&mut self) {
        if !self.persistence_ready || self.restoring_workspace {
            return;
        }
        let json = match self.workspace.to_persisted_json() {
            Ok(json) => json,
            Err(error) => {
                eprintln!("workspace state is not persistable: {error}");
                return;
            }
        };
        if let Err(error) = self.workspace_state.save(&json) {
            eprintln!(
                "failed to save workspace state to {}: {error}",
                self.workspace_state.path().display()
            );
        }
    }

    fn fail_workspace_restore(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        if !self.restoring_workspace {
            return;
        }
        eprintln!("workspace restore failed: {error}");
        self.workspace_restore_failed = true;
        cx.notify();
    }

    /// Focuses the cursor pane's view, or -- when there is none, e.g. an
    /// empty (zero-tab) workspace -- the shell root's own `focus_handle`
    /// instead of leaving focus wherever it happened to land. Reachability
    /// depends on this: the root `div` (`render::render`) is the one
    /// element `track_focus`-ing `focus_handle`, and both `ctrl+'` and `:`
    /// opening the palette are registered on it, so with no pane left to
    /// hold focus, window focus must still land somewhere that routes
    /// those bindings. `:` no longer needs `ctrl+'` first once the
    /// workspace is empty (2026-07-19 owner clarification, superseding
    /// 2026-07-18's two-step version of this same guarantee: with zero
    /// panes there is no pane input to protect, so the empty workspace is
    /// an implicit command surface -- see `Workspace::
    /// is_workspace_mode_active`'s doc comment); either way, the palette
    /// is the only reachable path back to `New Tab…` once every pane is
    /// gone, so it must stay reachable.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self
            .workspace
            .cursor_pane_id()
            .and_then(|id| self.panes.get(&id))
        {
            Some(view) => window.focus(&view.focus_handle(cx), cx),
            None => window.focus(&self.focus_handle, cx),
        }
        self.sync_terminal_focus(window, cx);
        self.persist_workspace();
    }

    /// Composes Horizon's own window focus with which pane is active into
    /// a single `TerminalCommand::Focus` transition to the session store
    /// — the GPUI counterpart of the Floem shell's
    /// `app::runtime::wire_focus_reporting`. Only the active pane's
    /// terminal ever believes it has focus: an agent pane active (or the
    /// window itself losing OS focus) means "no terminal focused," so the
    /// previously-focused terminal (if any) gets `Focus(false)` and the
    /// newly-focused one (if any) gets `Focus(true)` — never both to the
    /// same session, and nothing at all when the composed target hasn't
    /// changed. Called from every mutation that can change the active
    /// pane ([`Self::focus_active`], `render::activate_pane`) and from
    /// the window-activation observer registered in [`Self::new`].
    fn sync_terminal_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        let (unfocus, focus) = focus_transition(
            window.is_window_active(),
            self.workspace.active_terminal_session_id(),
            self.last_focused_terminal,
        );
        if unfocus.is_none() && focus.is_none() {
            return;
        }
        if let Some(session_id) = unfocus {
            self.send_terminal_focus(session_id, false, cx);
        }
        if let Some(session_id) = focus {
            self.send_terminal_focus(session_id, true, cx);
        }
        self.last_focused_terminal = focus;
    }

    fn send_terminal_focus(&self, session_id: SessionId, focused: bool, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get(&session_id) {
            session.read(cx).send_focus(focused);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::load_workspace_state;
    use horizon_workspace::Workspace;

    use crate::workspace_state::WorkspaceStateStore;

    fn state_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-workspace-shell-{label}-{}.json",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn missing_workspace_state_starts_fresh_and_enables_persistence() {
        let path = state_path("missing");
        let mut store = WorkspaceStateStore::new(path);
        let (workspace, restoring, persistence_ready) = load_workspace_state(&mut store);

        assert_eq!(workspace.tab_count(), 1);
        assert!(!restoring);
        assert!(persistence_ready);
    }

    #[test]
    fn valid_workspace_state_enters_the_restore_barrier() {
        let path = state_path("valid");
        let source = Workspace::mvp();
        let json = source.to_persisted_json().unwrap();
        let mut store = WorkspaceStateStore::new(path.clone());
        store.save(&json).unwrap();

        let (workspace, restoring, persistence_ready) = load_workspace_state(&mut store);

        assert_eq!(workspace.to_persisted_json().unwrap(), json);
        assert!(restoring);
        assert!(!persistence_ready);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unsupported_workspace_state_is_never_overwritten() {
        let path = state_path("newer");
        let contents = r#"{"version":999}"#;
        std::fs::write(&path, contents).unwrap();
        let mut store = WorkspaceStateStore::new(path.clone());

        let (_, restoring, persistence_ready) = load_workspace_state(&mut store);

        assert!(!restoring);
        assert!(!persistence_ready);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        std::fs::remove_file(path).unwrap();
    }
}
