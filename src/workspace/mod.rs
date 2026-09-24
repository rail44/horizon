//! The GPUI projection of the shared workspace model
//! (`crates/horizon-workspace`): tab strip, recursive split rendering as a
//! plain flex tree with self-owned resize handles (weight-native, no
//! `gpui_component::resizable` -- see `docs/split-resize-design.md`), pane
//! focus, and workspace mode with spatial navigation. The model owns all
//! layout truth; this module only renders it and translates GPUI actions
//! into model operations.
//!
//! `commands` owns command dispatch; `session_creation` resolves spawn intent;
//! `session_events` routes asynchronous notifications; `runtime_reload` keeps
//! the two daemon replacement contracts separate. `session_lifecycle` adopts
//! sessions and reconciles views; `restore` owns persisted workspace recovery.
//! `preview` handles session-less preview targets. The shell retains model,
//! session and pane ownership, focus coordination and persistence.

use std::collections::HashMap;

use gpui::*;
use gpui_component::list::ListState;
use gpui_component::StyledExt as _;
use horizon_terminal_core::TerminalNotification;
use horizon_workspace::commands::CommandId;
use horizon_workspace::{PaneId, SessionId, Workspace, WORKSPACE_STATE_VERSION};

use crate::agent::{AgentSession, AgentView};
use crate::board_pane::BoardPaneView;
use crate::model_picker::ModelPickerDelegate;
use crate::palette::PaletteDelegate;
use crate::preview::{PreviewPane, PreviewTarget};
use crate::runtime::{AgentdHandle, TerminaldHandle, TerminaldSlot};
use crate::session_manager::SessionManagerDelegate;
use crate::terminal::{TerminalSession, TerminalView};
use crate::terminal_focus::focus_transition;
use crate::theme_settings::ThemeSettingsView;
use crate::view_chooser::{Placement, ViewChooserDelegate};
use crate::workspace_state::{InvalidState, LoadResult, WorkspaceStateStore};

mod bindings;
mod commands;
mod modals;
mod navigation;
mod preview;
mod recovery;
mod render;
mod restore;
use recovery::WorkspacePhase;
mod runtime_reload;
mod session_creation;
mod session_events;
mod session_lifecycle;

use render::SplitDrag;
use session_creation::{PendingAgentSpawn, PendingTerminalSpawn};

pub(crate) fn init(cx: &mut App) {
    crate::agent::auxiliary::reload(horizon_config::load(), cx);
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
        PrevTab,
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

/// The tab strip's per-tab close button (`render_tab_close_button`):
/// closes the tab the user clicked, by its render-time index. Carries the
/// index the way [`RunCommand`] carries its `CommandId` -- gpui actions
/// built from a `KeyBinding` never carry per-invocation data, so no config
/// chord can target a specific tab; `CommandId::CloseActiveTab` stays the
/// keyboard path and this action the pointer path. `no_json`: same
/// reasoning as [`RunCommand`].
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct CloseTab {
    pub(crate) index: usize,
}

const MODE_CONTEXT: &str = "WorkspaceMode";

fn load_workspace_state(store: &mut WorkspaceStateStore) -> (Workspace, WorkspacePhase) {
    match store.load(u64::from(WORKSPACE_STATE_VERSION)) {
        Ok(LoadResult::Valid(json)) => match Workspace::from_persisted_json(&json) {
            Ok(workspace) => (workspace, WorkspacePhase::Restoring),
            Err(horizon_workspace::WorkspaceStateError::UnsupportedVersion {
                found,
                supported,
            }) => {
                eprintln!(
                    "workspace state version {found} is unsupported (expected {supported}); preserving the file"
                );
                (Workspace::mvp(), WorkspacePhase::PreservingFile)
            }
            Err(error) => {
                eprintln!("ignoring invalid workspace state: {error}");
                (Workspace::mvp(), WorkspacePhase::Ready)
            }
        },
        Ok(LoadResult::Missing) => (Workspace::mvp(), WorkspacePhase::Ready),
        Ok(LoadResult::Invalid(InvalidState::UnsupportedVersion { found, supported })) => {
            eprintln!(
                "workspace state version {found} is unsupported (expected {supported}); preserving the file"
            );
            (Workspace::mvp(), WorkspacePhase::PreservingFile)
        }
        Ok(LoadResult::Invalid(InvalidState::Corrupt(error))) => {
            eprintln!("ignoring corrupt workspace state: {error}");
            (Workspace::mvp(), WorkspacePhase::Ready)
        }
        Err(error) => {
            eprintln!("failed to load workspace state: {error}");
            (Workspace::mvp(), WorkspacePhase::PreservingFile)
        }
    }
}

/// Bring the workspace back to a state with at least one pane after
/// `Reload Terminal Runtime`
/// (`session_lifecycle::reload_terminal_runtime`) terminates every terminal
/// session ahead of restarting `horizon-terminald` -- its one remaining
/// caller. (Before the terminald split this belonged to `Reload Session
/// Runtime`, which killed the terminals as collateral; it now belongs to
/// the command that kills them on purpose.) A zero-tab workspace is now a valid,
/// persistable state (`WorkspaceState::validate` accepts it), so every
/// *other* termination path (`TerminateActiveSession`, the session
/// manager's secondary-confirm terminate, `control_plane_terminate`, a PTY
/// exit via `handle_terminal_exited`) leaves the workspace empty as-is
/// rather than calling this -- auto-creating a terminal there would
/// silently work against a user closing or terminating everything on
/// purpose (2026-07-18 owner clarification, superseding `704657b`'s
/// blanket guard). The reload path is different: killing every terminal
/// session is an operational side effect of restarting the runtime, not
/// something the user asked to empty, so it still gets a pane back.
fn ensure_workspace_has_pane(workspace: &mut Workspace) -> Option<SessionId> {
    (workspace.tab_count() == 0).then(|| {
        workspace
            .open_tab_with_new_session_activated(horizon_workspace::SessionKind::Terminal, true)
    })
}

/// One pane's view, by session kind -- plus one variant per first-party
/// [`ViewKind`] (`docs/theme-settings-view-design.md`), which has no
/// session at all.
#[derive(Clone)]
enum PaneView {
    Cached(CachedPaneLeaf),
    Composite(CompositePane),
}

/// A pane whose domain entity fills definite layout-tree bounds. Keeping
/// these variants behind one type makes cached rendering the only element
/// conversion available to fixed-size leaves.
#[derive(Clone)]
enum CachedPaneLeaf {
    Terminal(Entity<TerminalView>),
    ThemeSettings(Entity<ThemeSettingsView>),
    Board(Entity<BoardPaneView>),
    Preview(Entity<PreviewPane>),
}

/// A pane that owns narrower cache boundaries internally. Composite panes must
/// remain ordinary entities: wrapping them in another cached ancestor would
/// disable descendant reuse whenever that ancestor misses.
#[derive(Clone)]
enum CompositePane {
    Agent(Entity<AgentView>),
}

impl CachedPaneLeaf {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Terminal(view) => view.focus_handle(cx),
            Self::ThemeSettings(view) => view.focus_handle(cx),
            Self::Board(view) => view.focus_handle(cx),
            Self::Preview(view) => view.focus_handle(cx),
        }
    }

    fn element(&self) -> AnyElement {
        let style = || StyleRefinement::default().v_flex().size_full();
        match self {
            Self::Terminal(view) => view.clone().cached(style()).into_any_element(),
            Self::ThemeSettings(view) => view.clone().cached(style()).into_any_element(),
            Self::Board(view) => view.clone().cached(style()).into_any_element(),
            Self::Preview(view) => view.clone().cached(style()).into_any_element(),
        }
    }
}

impl CompositePane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Agent(view) => view.focus_handle(cx),
        }
    }

    fn element(&self) -> AnyElement {
        match self {
            Self::Agent(view) => view.clone().into_any_element(),
        }
    }
}

impl PaneView {
    fn terminal(view: Entity<TerminalView>) -> Self {
        Self::Cached(CachedPaneLeaf::Terminal(view))
    }

    fn agent(view: Entity<AgentView>) -> Self {
        Self::Composite(CompositePane::Agent(view))
    }

    fn theme_settings(view: Entity<ThemeSettingsView>) -> Self {
        Self::Cached(CachedPaneLeaf::ThemeSettings(view))
    }

    fn board(view: Entity<BoardPaneView>) -> Self {
        Self::Cached(CachedPaneLeaf::Board(view))
    }

    fn preview(view: Entity<PreviewPane>) -> Self {
        Self::Cached(CachedPaneLeaf::Preview(view))
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Cached(view) => view.focus_handle(cx),
            Self::Composite(view) => view.focus_handle(cx),
        }
    }

    fn element(&self) -> AnyElement {
        // This exhaustive type split is the cache-topology decision point for
        // every future pane kind. Callers cannot render a fixed leaf without
        // its cache or wrap a composite in an outer cache.
        match self {
            Self::Cached(view) => view.element(),
            Self::Composite(view) => view.element(),
        }
    }
}

pub(crate) struct WorkspaceShell {
    workspace: Workspace,
    workspace_state: WorkspaceStateStore,
    workspace_phase: WorkspacePhase,
    // This instance's control socket — every spawned pane gets it as
    // HORIZON_SOCKET so CLIs invoked inside reach back here.
    socket_path: std::path::PathBuf,
    // The session store — the GPUI shell's Registry counterpart: PTY
    // sessions live here keyed by SessionId, independent of pane views,
    // so closing a pane detaches (session survives, scrollback intact)
    // and terminating is the explicit destructive path.
    sessions: HashMap<SessionId, Entity<TerminalSession>>,
    agent_sessions: HashMap<SessionId, Entity<AgentSession>>,
    // Staged by `control_plane_new_session` (a role-tagged create, e.g.
    // `new-agent --role config`) and consumed by `reconcile` when it actually
    // starts the session — the model's `open_tab_with_new_session_*`
    // call only yields a `SessionId`, so the role has nowhere else to
    // ride until reconcile turns that id into a live agent session.
    pending_roles: HashMap<SessionId, horizon_agent::roles::RoleId>,
    // Staged before session-creating workspace mutations and consumed by
    // reconcile. The daemon resolves the source session's live cwd; Horizon
    // carries only the source id and fallback spawn input.
    pending_terminal_spawns: HashMap<SessionId, PendingTerminalSpawn>,
    // Same staging shape, for agent spawns' own two knobs (source pane +
    // isolation) -- see `PendingAgentSpawn`.
    pending_agent_spawns: HashMap<SessionId, PendingAgentSpawn>,
    // Created eagerly before the first reconcile. Its op queue accepts
    // agent requests while connect/hello proceeds in the background.
    agentd: Option<AgentdHandle>,
    // The terminal daemon's own client runtime, started alongside
    // `agentd` and reloaded independently
    // (`docs/terminald-split-design.md`): `Reload Agent Runtime` replaces
    // only the field above, so every PTY -- and whatever interactive CLI is
    // running in it -- survives an agent-runtime restart.
    terminald: Option<TerminaldHandle>,
    // A live-reading mirror of `terminald` above, kept in sync at every
    // write site (`new`, `ReloadTerminalRuntime`,
    // `reload_terminal_runtime`'s resume) -- handed to panes that can
    // outlive a single runtime instance (the theme settings view, which
    // re-pushes the terminal color scheme) so a clone captured mid-`Reload
    // Terminal Runtime` never gets stuck on a stale `None`. See
    // `TerminaldSlot`'s doc comment.
    terminald_slot: TerminaldSlot,
    // Guards both reload commands: one daemon restart at a time, whichever
    // daemon it targets.
    reload_in_progress: bool,
    panes: HashMap<PaneId, PaneView>,
    // What each `ViewKind::Preview` pane shows: the `.wasm` artifact and
    // the name of the preview inside it. Keyed by pane id and kept out of
    // the workspace model on purpose -- `ViewKind` stays `Copy` and the
    // persisted schema stays a bare tag, so a restored preview pane comes
    // back empty (see `ViewKindState::Preview`).
    preview_targets: HashMap<PaneId, PreviewTarget>,
    // This window — needed by `Reload Agent Runtime`'s post-resume step,
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
    model_picker: Option<Entity<ListState<ModelPickerDelegate>>>,
    _model_picker_subscription: Option<Subscription>,
    // The agent session the open model picker will switch, captured at open
    // (parent task #1's Phase 2) -- the confirm path resolves the
    // daemon-side session id from it. Cleared on close/cancel so a stale
    // confirm can never apply to a session the picker wasn't opened for.
    model_picker_target: Option<Entity<AgentSession>>,
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
    // Handed to every `TerminalSession::spawn`/`AgentSession::new` (cloned
    // per session) so a content-derived title (a terminal's OSC 0/2 title,
    // an agent's first user message) reaches the workspace model -- the
    // `session_title_rx` pump spawned in `new`
    // (`wire_session_title_updates`).
    session_title_tx: futures::channel::mpsc::UnboundedSender<(SessionId, Option<String>)>,
    // Handed to every `TerminalSession::spawn` (cloned per session) so an
    // OSC 9/777 desktop-notification request reaches the shell, which
    // gates it on window/pane focus (`wire_terminal_notifications` +
    // `should_surface_notification`) before the OS hop
    // (`crate::desktop_notify`).
    terminal_notify_tx: futures::channel::mpsc::UnboundedSender<(SessionId, TerminalNotification)>,
}

impl WorkspaceShell {
    pub(crate) fn new(
        socket_path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workspace_state = WorkspaceStateStore::from_environment();
        let (workspace, workspace_phase) = load_workspace_state(&mut workspace_state);
        let (agentd, host_tool_rx, workspace_root_rx) = AgentdHandle::start(
            &horizon_wire::socket::default_agentd_socket_path(),
            &socket_path,
        );
        let terminald = TerminaldHandle::start(
            &horizon_wire::socket::default_terminald_socket_path(),
            &socket_path,
        );
        let (terminal_exit_tx, terminal_exit_rx) = futures::channel::mpsc::unbounded();
        let (session_title_tx, session_title_rx) = futures::channel::mpsc::unbounded();
        let (terminal_notify_tx, terminal_notify_rx) = futures::channel::mpsc::unbounded();
        let mut shell = Self {
            workspace,
            workspace_state,
            workspace_phase,
            socket_path,
            sessions: HashMap::new(),
            agent_sessions: HashMap::new(),
            pending_roles: HashMap::new(),
            pending_terminal_spawns: HashMap::new(),
            pending_agent_spawns: HashMap::new(),
            agentd: Some(agentd.clone()),
            terminald: Some(terminald.clone()),
            terminald_slot: TerminaldSlot::new(Some(terminald.clone())),
            reload_in_progress: false,
            panes: HashMap::new(),
            preview_targets: HashMap::new(),
            window: window.window_handle(),
            focus_handle: cx.focus_handle(),
            palette: None,
            _palette_subscription: None,
            session_manager: None,
            _session_manager_subscription: None,
            view_chooser: None,
            _view_chooser_subscription: None,
            pending_placement: None,
            model_picker: None,
            _model_picker_subscription: None,
            model_picker_target: None,
            active_split_drag: None,
            last_focused_terminal: None,
            terminal_exit_tx,
            session_title_tx,
            terminal_notify_tx,
        };
        // Window activation/deactivation doesn't otherwise touch the
        // model, so it needs its own observer alongside `focus_active`'s
        // call to `sync_terminal_focus` (every model mutation that can
        // change the active pane).
        cx.observe_window_activation(window, |shell, window, cx| {
            shell.sync_terminal_focus(window, cx);
        })
        .detach();
        shell.wire_host_tools(agentd.responder(), host_tool_rx, cx);
        shell.wire_workspace_root_updates(workspace_root_rx, cx);
        shell.wire_terminal_exit(terminal_exit_rx, cx);
        shell.wire_session_title_updates(session_title_rx, cx);
        shell.wire_terminal_notifications(terminal_notify_rx, cx);
        shell.wire_notification_responses(cx);
        if shell.workspace_phase.blocks_mutation() {
            shell.spawn_workspace_restore(agentd, terminald, cx);
        } else {
            shell.reconcile(window, cx);
            shell.focus_active(window, cx);
            shell.spawn_terminal_resume(terminald, cx);
            shell.spawn_agent_resume(agentd, cx);
        }
        shell
    }

    fn persist_workspace(&mut self) {
        if !self.workspace_phase.can_save() {
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
        if !self.workspace_phase.fail_restore() {
            return;
        }
        eprintln!("workspace restore failed: {error}");
        cx.notify();
    }

    fn discard_failed_workspace_restore(&mut self) -> bool {
        if !self.workspace_phase.discard_failed_restore() {
            return false;
        }
        self.workspace = Workspace::mvp();
        self.persist_workspace();
        true
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
        if self.workspace_phase.blocks_mutation() {
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

    /// Whether an OSC 9/777 notification from `session_id` should escalate
    /// to the OS notification center. The rule mirrors how the terminal
    /// itself treats focus: the user must not already be looking at the
    /// session — either the window is inactive, or the session isn't the
    /// active terminal pane (a background pane finishing a build while the
    /// user works elsewhere still deserves its banner; the pane they are
    /// typing in does not). Workspace restore is excluded like every other
    /// session-driven reaction (`sync_terminal_focus`'s guard).
    fn should_surface_notification(&self, session_id: SessionId, window: &Window) -> bool {
        if self.workspace_phase.blocks_mutation() {
            return false;
        }
        !window.is_window_active()
            || self.workspace.active_terminal_session_id() != Some(session_id)
    }

    fn send_terminal_focus(&self, session_id: SessionId, focused: bool, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get(&session_id) {
            session.read(cx).send_focus(focused);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{load_workspace_state, WorkspacePhase};
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
        let (workspace, phase) = load_workspace_state(&mut store);

        assert_eq!(workspace.tab_count(), 1);
        assert_eq!(phase, WorkspacePhase::Ready);
    }

    #[test]
    fn valid_workspace_state_enters_the_restore_barrier() {
        let path = state_path("valid");
        let source = Workspace::mvp();
        let json = source.to_persisted_json().unwrap();
        let mut store = WorkspaceStateStore::new(path.clone());
        store.save(&json).unwrap();

        let (workspace, phase) = load_workspace_state(&mut store);

        assert_eq!(workspace.to_persisted_json().unwrap(), json);
        assert_eq!(phase, WorkspacePhase::Restoring);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unsupported_workspace_state_is_never_overwritten() {
        let path = state_path("newer");
        let contents = r#"{"version":999}"#;
        std::fs::write(&path, contents).unwrap();
        let mut store = WorkspaceStateStore::new(path.clone());

        let (_, phase) = load_workspace_state(&mut store);

        assert_eq!(phase, WorkspacePhase::PreservingFile);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        std::fs::remove_file(path).unwrap();
    }
}
