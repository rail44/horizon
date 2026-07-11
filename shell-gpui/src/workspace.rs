//! The GPUI projection of the shared workspace model
//! (`crates/horizon-workspace`): tab strip, recursive split rendering on
//! gpui-component's resizable primitives, pane focus, and workspace mode
//! with spatial navigation. The model owns all layout truth; this module
//! only renders it and translates GPUI actions into model operations.
//!
//! The key bindings registered in [`init`] are M2 stand-ins wired
//! straight to model calls — M3 replaces them with the command model
//! (`CommandId` + keymap config), at which point every handler here
//! becomes a binding to a command instead. [`init`]'s `[keybindings]`
//! layer (parsed via `keymap::gpui_keystroke`/`keymap::command_for`) is
//! the first piece of that: config-bound chords dispatch through
//! [`RunCommand`] to [`WorkspaceShell::execute`] instead of a
//! model-call handler.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::{List, ListEvent, ListState};
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizablePanelGroup};
use horizon_workspace::commands::{command_entries, CommandId, CommandState};
use horizon_workspace::types::LayoutNode;
use horizon_workspace::{Direction, PaneId, PaneKind, SplitAxis, Workspace};

use crate::agent::{wait_for_drain, AgentSession, AgentView, AgentdHandle};
use crate::keymap;
use crate::palette::PaletteDelegate;
use crate::session_manager::SessionManagerDelegate;
use crate::terminal::{sample_cwd, TerminalSession, TerminalView};
use crate::terminal_focus::focus_transition;
use crate::theme;
use crate::view_chooser::{Placement, ViewChooserDelegate};
use horizon_terminal_core::TerminalCommand;
use horizon_workspace::types::SessionKind;
use horizon_workspace::SessionId;

type AgentSessionId = horizon_agent::contract::SessionId;

fn agent_session_id(id: SessionId) -> AgentSessionId {
    AgentSessionId::from_uuid(id.as_uuid())
}

/// One pane's view, by session kind.
#[derive(Clone)]
enum PaneView {
    Terminal(Entity<TerminalView>),
    Agent(Entity<AgentView>),
}

impl PaneView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            PaneView::Terminal(view) => view.focus_handle(cx),
            PaneView::Agent(view) => view.focus_handle(cx),
        }
    }

    fn element(&self) -> AnyElement {
        match self {
            PaneView::Terminal(view) => view.clone().into_any_element(),
            PaneView::Agent(view) => view.clone().into_any_element(),
        }
    }
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
        OpenPalette
    ]
);

/// A `[keybindings]`-config-driven binding to a `CommandId` — gpui actions
/// used with `KeyBinding` are compile-time types, so a config chord can't
/// bind directly to one of the many `CommandId` variants the way a unit
/// action binds to one fixed handler. `RunCommand` carries the resolved
/// id as data instead, so a single action type covers every simple
/// command a `[keybindings]` entry can name (see `keymap::command_for`).
/// `no_json`: never built from a JSON keymap (only ever constructed
/// directly in [`init`]), so it skips gpui's `Deserialize`/`JsonSchema`
/// requirements for action fields.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
struct RunCommand {
    id: CommandId,
}

const MODE_CONTEXT: &str = "WorkspaceMode";

/// Built-in default chord for [`ToggleWorkspaceMode`] — mirrors the Floem
/// shell's `DEFAULT_WORKSPACE_MODE_CHORD`. Not bound when a
/// `[keybindings]` entry overrides it via the reserved
/// `keymap::WORKSPACE_MODE_PSEUDO_COMMAND` (see [`init`]).
const DEFAULT_WORKSPACE_MODE_KEYSTROKE: &str = "ctrl-'";

pub fn init(cx: &mut App) {
    let config = horizon_config::load();

    let workspace_mode_override = config
        .keybindings
        .iter()
        .find(|(_, command)| command.as_str() == keymap::WORKSPACE_MODE_PSEUDO_COMMAND)
        .map(|(chord, _)| chord.as_str());

    let mut bindings = vec![
        KeyBinding::new("h", ModeMoveLeft, Some(MODE_CONTEXT)),
        KeyBinding::new("j", ModeMoveDown, Some(MODE_CONTEXT)),
        KeyBinding::new("k", ModeMoveUp, Some(MODE_CONTEXT)),
        KeyBinding::new("l", ModeMoveRight, Some(MODE_CONTEXT)),
        KeyBinding::new("left", ModeMoveLeft, Some(MODE_CONTEXT)),
        KeyBinding::new("down", ModeMoveDown, Some(MODE_CONTEXT)),
        KeyBinding::new("up", ModeMoveUp, Some(MODE_CONTEXT)),
        KeyBinding::new("right", ModeMoveRight, Some(MODE_CONTEXT)),
        KeyBinding::new("enter", ModeCommit, Some(MODE_CONTEXT)),
        KeyBinding::new("escape", ModeCancel, Some(MODE_CONTEXT)),
        KeyBinding::new("t", NewTab, Some(MODE_CONTEXT)),
        KeyBinding::new("a", NewAgentTab, Some(MODE_CONTEXT)),
        KeyBinding::new("s", SplitPane, Some(MODE_CONTEXT)),
        KeyBinding::new("x", ClosePane, Some(MODE_CONTEXT)),
        KeyBinding::new("tab", NextTab, Some(MODE_CONTEXT)),
        KeyBinding::new(":", OpenPalette, Some(MODE_CONTEXT)),
    ];

    match workspace_mode_override {
        Some(chord) => match keymap::gpui_keystroke(chord) {
            Some(keystroke) => {
                bindings.push(KeyBinding::new(&keystroke, ToggleWorkspaceMode, None))
            }
            None => {
                eprintln!(
                    "horizon config: skipping keybinding `{chord}` = \
                     `{}`: unrecognized chord",
                    keymap::WORKSPACE_MODE_PSEUDO_COMMAND
                );
                bindings.push(KeyBinding::new(
                    DEFAULT_WORKSPACE_MODE_KEYSTROKE,
                    ToggleWorkspaceMode,
                    None,
                ));
            }
        },
        None => bindings.push(KeyBinding::new(
            DEFAULT_WORKSPACE_MODE_KEYSTROKE,
            ToggleWorkspaceMode,
            None,
        )),
    }

    // `[keybindings]` config entries layer on top of the built-ins above:
    // later-registered bindings take precedence in gpui at the same
    // context depth (`Keymap::bindings_for_input`'s doc comment — "the
    // ones added to the keymap later take precedence"), so pushing these
    // after the built-ins is enough for a config entry to override one
    // bound to the same chord.
    for (chord, command) in &config.keybindings {
        if command == keymap::WORKSPACE_MODE_PSEUDO_COMMAND {
            continue; // handled above
        }
        let Some(keystroke) = keymap::gpui_keystroke(chord) else {
            eprintln!(
                "horizon config: skipping keybinding `{chord}` = `{command}`: unrecognized chord"
            );
            continue;
        };
        if command == keymap::OPEN_PALETTE_PSEUDO_COMMAND {
            bindings.push(KeyBinding::new(&keystroke, OpenPalette, None));
            continue;
        }
        let Some(id) = keymap::command_for(command) else {
            eprintln!(
                "horizon config: skipping keybinding `{chord}` = `{command}`: unknown command id"
            );
            continue;
        };
        bindings.push(KeyBinding::new(&keystroke, RunCommand { id }, None));
    }

    cx.bind_keys(bindings);
}

pub struct WorkspaceShell {
    workspace: Workspace,
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
    // Staged by `new_tab`/`split_pane`/`external_new_session` (resolved
    // from the spawn-source terminal *before* the workspace mutation
    // that creates the new session activates it -- see
    // `Self::resolve_new_terminal_cwd`'s doc comment for why it must run
    // first) and consumed by `reconcile` when it actually spawns the PTY,
    // for the same "info has nowhere else to ride" reason as
    // `pending_roles`.
    pending_terminal_cwds: HashMap<SessionId, std::path::PathBuf>,
    // Lazily connected on the first agent session (the Floem shell
    // connects async at startup; lazy-blocking is the v1 tradeoff here).
    agentd: Option<AgentdHandle>,
    panes: HashMap<PaneId, PaneView>,
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
    // The terminal session `sync_terminal_focus` last sent `Focus(true)`
    // to, so a transition can send `Focus(false)` to the one it's about
    // to stop being true for. See `focus_transition`.
    last_focused_terminal: Option<SessionId>,
}

impl WorkspaceShell {
    pub fn new(
        socket_path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut shell = Self {
            workspace: Workspace::mvp(),
            socket_path,
            sessions: HashMap::new(),
            agent_sessions: HashMap::new(),
            pending_roles: HashMap::new(),
            pending_terminal_cwds: HashMap::new(),
            agentd: None,
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
            last_focused_terminal: None,
        };
        // Window activation/deactivation doesn't otherwise touch the
        // model, so it needs its own observer alongside `focus_active`'s
        // call to `sync_terminal_focus` (every model mutation that can
        // change the active pane).
        cx.observe_window_activation(window, |shell, window, cx| {
            shell.sync_terminal_focus(window, cx);
        })
        .detach();
        shell.reconcile(window, cx);
        shell.focus_active(window, cx);
        shell.spawn_startup_resume(cx);
        shell
    }

    /// Bring the session store and the PaneId → view map in line with
    /// the model. Sessions the model no longer knows (terminated) are
    /// shut down and dropped; sessions without panes stay alive
    /// (detached); every pane gets a view bound to its session's entity,
    /// so a reattached pane resumes with scrollback intact.
    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let summaries = self.workspace.session_summaries();
        let known: std::collections::HashSet<SessionId> =
            summaries.iter().map(|summary| summary.id).collect();
        self.sessions.retain(|id, session| {
            let keep = known.contains(id);
            if !keep {
                session.read(cx).shutdown();
            }
            keep
        });
        self.agent_sessions.retain(|id, session| {
            let keep = known.contains(id);
            if !keep {
                session.read(cx).shutdown();
            }
            keep
        });
        for summary in summaries {
            match summary.kind {
                SessionKind::Terminal => {
                    let id = summary.id;
                    if !self.sessions.contains_key(&id) {
                        let socket_path = self.socket_path.clone();
                        let cwd = self
                            .pending_terminal_cwds
                            .remove(&id)
                            .unwrap_or_else(Self::default_terminal_cwd);
                        self.sessions.insert(
                            id,
                            cx.new(|cx| TerminalSession::spawn(id, &socket_path, &cwd, cx)),
                        );
                    }
                }
                SessionKind::Agent => {
                    if self.agent_sessions.contains_key(&summary.id) {
                        continue;
                    }
                    let handle = match self.agentd(cx) {
                        Ok(handle) => handle,
                        Err(error) => {
                            eprintln!("agent session unavailable: {error}");
                            continue;
                        }
                    };
                    let provider_id =
                        horizon_agent::contract::ProviderRegistry::default().default_provider_id();
                    let role_id = self.pending_roles.remove(&summary.id);
                    let session_handle =
                        handle.start_session(agent_session_id(summary.id), provider_id, role_id);
                    self.agent_sessions.insert(
                        summary.id,
                        cx.new(|cx| AgentSession::new(session_handle, cx)),
                    );
                }
            }
        }

        let pane_ids = self.workspace.all_pane_ids();
        self.panes.retain(|id, _| pane_ids.contains(id));
        for pane_id in pane_ids {
            if self.panes.contains_key(&pane_id) {
                continue;
            }
            if let Some(session) = self
                .workspace
                .terminal_session_id(pane_id)
                .and_then(|id| self.sessions.get(&id).cloned())
            {
                self.panes.insert(
                    pane_id,
                    PaneView::Terminal(cx.new(|cx| TerminalView::new(session.clone(), window, cx))),
                );
            } else if let Some(session) = self
                .workspace
                .agent_session_id(pane_id)
                .and_then(|id| self.agent_sessions.get(&id).cloned())
            {
                self.panes.insert(
                    pane_id,
                    PaneView::Agent(cx.new(|cx| AgentView::new(session.clone(), window, cx))),
                );
            }
        }
        cx.notify();
    }

    /// Lazily connects to `horizon-agentd` (spawning it if needed);
    /// [`Self::spawn_startup_resume`] usually gets there first.
    fn agentd(&mut self, cx: &mut Context<Self>) -> Result<AgentdHandle, String> {
        if let Some(handle) = &self.agentd {
            return Ok(handle.clone());
        }
        let (handle, host_tool_rx) = AgentdHandle::connect(
            &horizon_agent::socket::default_socket_path(),
            &self.socket_path,
        )?;
        self.adopt_agentd(handle.clone(), host_tool_rx, cx);
        Ok(handle)
    }

    /// Stores a fresh connection and wires the host-tool responder:
    /// `workspace.snapshot` requests are answered on the UI thread from
    /// the live model, mirroring the Floem shell's
    /// `wire_host_tool_responder`.
    fn adopt_agentd(
        &mut self,
        handle: AgentdHandle,
        host_tool_rx: crossbeam_channel::Receiver<horizon_agent::wire::HostToolRequest>,
        cx: &mut Context<Self>,
    ) {
        self.agentd = Some(handle.clone());

        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(request) = host_tool_rx.recv() {
                if async_tx.unbounded_send(request).is_err() {
                    return;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(request) = async_rx.next().await {
                let output = this
                    .update(cx, |shell, _| match request.tool_id.as_str() {
                        "workspace.snapshot" => {
                            horizon_workspace::snapshot::workspace_snapshot(&shell.workspace)
                        }
                        other => serde_json::json!({
                            "error": format!("unknown host tool `{other}`")
                        }),
                    })
                    .unwrap_or_else(
                        |_| serde_json::json!({ "error": "the workspace shell is gone" }),
                    );
                handle.respond_host_tool(horizon_agent::wire::HostToolResponse {
                    request_id: request.request_id,
                    output,
                });
            }
        })
        .detach();
    }

    /// Agentd resume: connect to agentd on a background thread (never
    /// blocking the caller — the Floem shell's
    /// `connect_agentd_at_startup_async`/`reload_agent_runtime` shape),
    /// list the sessions it still hosts, and adopt each as a detached
    /// session: registered in the model (so the session manager shows it)
    /// and attached over the wire (so its replayed transcript is ready
    /// when a pane picks it up). Shared by two callers: startup
    /// ([`Self::new`], against a freshly opened window with no agent
    /// panes yet) and `Reload Agent Runtime`
    /// ([`Self::reload_agent_runtime`], after the old connection has
    /// drained — see that function's doc comment for why its
    /// `agent_sessions`/agent-pane views are already cleared by the time
    /// this runs). Either way, the post-adopt `reconcile`/`focus_active`
    /// pass rebuilds any agent pane view whose session id this resume
    /// just reattached — a no-op at startup (no agent panes exist yet)
    /// and the reload's actual pane-rebuild step.
    fn spawn_startup_resume(&self, cx: &mut Context<Self>) {
        let agentd_socket = horizon_agent::socket::default_socket_path();
        let control_socket = self.socket_path.clone();
        let window_handle = self.window;
        let (startup_tx, mut startup_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(
            move || match AgentdHandle::connect(&agentd_socket, &control_socket) {
                Ok((handle, host_tool_rx)) => {
                    let summaries = handle.session_list();
                    let _ = startup_tx.unbounded_send((handle, host_tool_rx, summaries));
                }
                Err(error) => eprintln!("agentd connect failed: {error}"),
            },
        );
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let Some((handle, host_tool_rx, summaries)) = startup_rx.next().await else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    if shell.agentd.is_none() {
                        shell.adopt_agentd(handle.clone(), host_tool_rx, cx);
                    }
                    for summary in summaries {
                        let session_id = SessionId::from_uuid(summary.session_id.as_uuid());
                        if shell.agent_sessions.contains_key(&session_id) {
                            continue;
                        }
                        shell
                            .workspace
                            .register_detached_session(PaneKind::Agent, session_id);
                        let session_handle = handle.attach_session(summary.session_id);
                        shell.agent_sessions.insert(
                            session_id,
                            cx.new(|cx| AgentSession::new(session_handle, cx)),
                        );
                    }
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                });
            });
        })
        .detach();
    }

    /// `Reload Agent Runtime`: drain the current agentd connection (if
    /// any), wait for the old process to actually exit
    /// ([`wait_for_drain`]), then run the same
    /// connect/`session_list`/adopt/reconcile flow as startup resume
    /// ([`Self::spawn_startup_resume`]) against the (possibly
    /// just-rebuilt) binary — "drain -> agentd flushes and exits ->
    /// Horizon spawns the rebuilt binary -> reconnect -> session_load",
    /// mirroring the Floem shell's `agentd_runtime::reload_agent_runtime`.
    ///
    /// Called with `self.agentd` already taken and every stale agent
    /// session entity/pane view already dropped (`execute`'s
    /// `CommandId::ReloadAgentRuntime` arm) — nothing should try to route
    /// a command through the dying connection while this is in flight,
    /// and `spawn_startup_resume`'s "already known" checks (`shell.agentd
    /// .is_none()`, `shell.agent_sessions.contains_key`) must see the
    /// pre-reload state as gone, not stale, so it reattaches every
    /// session rather than skipping them.
    fn reload_agent_runtime(&self, old: Option<AgentdHandle>, cx: &mut Context<Self>) {
        let socket_path = horizon_agent::socket::default_socket_path();
        let (drained_tx, mut drained_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            if let Some(handle) = old {
                handle.drain();
            }
            if let Err(error) = wait_for_drain(&socket_path) {
                eprintln!("horizon-agentd did not drain cleanly: {error}");
            }
            let _ = drained_tx.unbounded_send(());
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            if drained_rx.next().await.is_none() {
                return;
            }
            let _ = this.update(cx, |shell, cx| shell.spawn_startup_resume(cx));
        })
        .detach();
    }

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self
            .workspace
            .cursor_pane_id()
            .and_then(|id| self.panes.get(&id))
        {
            window.focus(&view.focus_handle(cx), cx);
        }
        self.sync_terminal_focus(window, cx);
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
    /// pane ([`Self::focus_active`], [`Self::activate_pane`]) and from
    /// the window-activation observer registered in [`Self::new`].
    fn sync_terminal_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
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
            let _ = session
                .read(cx)
                .sender()
                .send(TerminalCommand::Focus(focused));
        }
    }

    fn toggle_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.is_workspace_mode_active() {
            self.workspace.cancel_workspace_mode();
            self.focus_active(window, cx);
        } else {
            self.workspace.enter_workspace_mode();
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn mode_move(&mut self, direction: Direction, cx: &mut Context<Self>) {
        self.workspace.move_cursor(direction);
        cx.notify();
    }

    fn mode_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.commit_workspace_mode();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn mode_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.cancel_workspace_mode();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The cwd a newly spawned terminal should start in --
    /// `docs/session-relationship-design.md` decision 3's "workspace_root
    /// source" ("start where I'm looking"): the currently active
    /// terminal session's live cwd, sampled via its retained pid
    /// (`crate::terminal::sample_cwd`), falling back to `$HOME` (then
    /// `.`) when there is no active terminal session or its cwd can't be
    /// determined.
    ///
    /// Must be called *before* the workspace mutation that creates the
    /// new session: once that mutation activates it (`new_tab`/
    /// `split_pane` always activate; `external_new_session` sometimes
    /// does), `Workspace::active_terminal_session_id` would resolve to
    /// the new, not-yet-spawned session itself instead of the true
    /// spawn source.
    fn resolve_new_terminal_cwd(&self, cx: &mut Context<Self>) -> std::path::PathBuf {
        self.workspace
            .active_terminal_session_id()
            .and_then(|id| self.sessions.get(&id))
            .and_then(|session| session.read(cx).pid())
            .and_then(sample_cwd)
            .unwrap_or_else(Self::default_terminal_cwd)
    }

    fn default_terminal_cwd() -> std::path::PathBuf {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    }

    /// The one interactive session-creation path: what the view chooser
    /// confirms with. Terminal cwd and agent role ride the same staging
    /// maps `reconcile` consumes.
    fn create_session(
        &mut self,
        kind: PaneKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace.exit_workspace_mode();
        let cwd = matches!(kind, PaneKind::Terminal).then(|| self.resolve_new_terminal_cwd(cx));
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
            if let Some(cwd) = cwd {
                self.pending_terminal_cwds.insert(session_id, cwd);
            }
            if let Some(role_id) = role_id {
                self.pending_roles.insert(session_id, role_id);
            }
        }
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    fn open_view_chooser(
        &mut self,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace.exit_workspace_mode();
        self.pending_placement = Some(placement);
        let list =
            cx.new(|cx| ListState::new(ViewChooserDelegate::new(), window, cx).searchable(true));
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let choice = list.read(cx).delegate().choice_at(*index).cloned();
                    let placement = shell.pending_placement.take();
                    shell.close_view_chooser(window, cx);
                    if let (Some(choice), Some(placement)) = (choice, placement) {
                        shell.create_session(choice.kind, choice.role_id, placement, window, cx);
                    }
                }
                ListEvent::Cancel => {
                    shell.pending_placement = None;
                    shell.close_view_chooser(window, cx);
                }
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.view_chooser = Some(list);
        self._view_chooser_subscription = Some(subscription);
        cx.notify();
    }

    fn close_view_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_chooser = None;
        self._view_chooser_subscription = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The active pane's agent session, when it is an agent pane.
    fn active_agent_session(&self) -> Option<Entity<AgentSession>> {
        let pane_id = self.workspace.cursor_pane_id()?;
        let session_id = self.workspace.agent_session_id(pane_id)?;
        self.agent_sessions.get(&session_id).cloned()
    }

    /// The M3 dispatch point: every surface (palette, keybindings, and
    /// later the control plane) funnels through here — the GPUI
    /// counterpart of the Floem shell's `execute_command`.
    fn execute(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            CommandId::SplitRight => self.open_view_chooser(Placement::SplitRight, window, cx),
            CommandId::SplitDown => self.open_view_chooser(Placement::SplitDown, window, cx),
            CommandId::NewTab => self.open_view_chooser(Placement::NewTab, window, cx),
            CommandId::FocusNextPane => {
                self.workspace.focus_next();
                self.focus_active(window, cx);
                cx.notify();
            }
            CommandId::CloseActivePane => self.close_pane(window, cx),
            CommandId::CloseActiveTab => {
                self.workspace.exit_workspace_mode();
                self.workspace.close_active_tab();
                self.reconcile(window, cx);
                self.focus_active(window, cx);
            }
            CommandId::TerminateActiveSession => {
                self.workspace.exit_workspace_mode();
                self.workspace.terminate_active_session();
                self.reconcile(window, cx);
                self.focus_active(window, cx);
            }
            CommandId::TerminateAllDetachedSessions => {
                for summary in self.workspace.detached_session_summaries() {
                    self.workspace.terminate_session(summary.id);
                }
                self.reconcile(window, cx);
            }
            CommandId::OpenSessionManager => self.open_session_manager(window, cx),
            CommandId::ApproveToolCall => {
                if let Some(session) = self.active_agent_session() {
                    let pending = horizon_agent::frame::pending_approval_call_ids_in(
                        &session.read(cx).frame.items,
                    );
                    if let Some(call_id) = pending.first() {
                        session.read(cx).approve(call_id.clone());
                    }
                }
            }
            CommandId::DenyToolCall => {
                if let Some(session) = self.active_agent_session() {
                    let pending = horizon_agent::frame::pending_approval_call_ids_in(
                        &session.read(cx).frame.items,
                    );
                    if let Some(call_id) = pending.first() {
                        session.read(cx).deny(call_id.clone());
                    }
                }
            }
            CommandId::CancelAgentTurn => {
                if let Some(session) = self.active_agent_session() {
                    session.read(cx).cancel();
                }
            }
            CommandId::ReloadConfig => match horizon_config::reload() {
                Ok(raw) => {
                    theme::reload_from(&raw);
                    window.refresh();
                }
                Err(error) => eprintln!("reload-config failed: {error}"),
            },
            CommandId::ReloadAgentRuntime => {
                let old = self.agentd.take();
                // The model keeps the sessions/panes; only the stale
                // views/handles are dropped — `reload_agent_runtime`'s
                // resume repopulates `agent_sessions` (via `session_load`)
                // and `reconcile` rebuilds a fresh `AgentView` for every
                // pane whose session id comes back.
                self.agent_sessions.clear();
                self.panes
                    .retain(|_, view| !matches!(view, PaneView::Agent(_)));
                cx.notify();
                self.reload_agent_runtime(old, cx);
            }
        }
    }

    /// `execute` for control-plane callers — public without exposing the
    /// whole command surface.
    pub(crate) fn execute_external(
        &mut self,
        id: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.execute(id, window, cx);
    }

    fn open_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        let summaries = self.workspace.session_summaries();
        let list = cx.new(|cx| {
            ListState::new(SessionManagerDelegate::new(summaries), window, cx).searchable(true)
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let (summary, secondary) = {
                        let delegate = list.read(cx).delegate();
                        (
                            delegate.summary_at(*index).cloned(),
                            delegate.last_confirm_secondary(),
                        )
                    };
                    let Some(summary) = summary else {
                        return;
                    };
                    if secondary {
                        // Secondary confirm (cmd-enter / right click)
                        // terminates the session; the modal stays open
                        // on refreshed data.
                        shell.workspace.terminate_session(summary.id);
                        shell.reconcile(window, cx);
                        let sessions = shell.workspace.session_summaries();
                        list.update(cx, |list, cx| {
                            list.delegate_mut().reset(sessions);
                            cx.notify();
                        });
                        return;
                    }
                    shell.close_session_manager(window, cx);
                    if summary.attached {
                        if let Some((tab, pane)) =
                            shell.workspace.pane_location_for_session(summary.id)
                        {
                            shell.workspace.activate_pane_index(tab, pane);
                        }
                    } else {
                        shell
                            .workspace
                            .attach_existing_session_to_split_activated(summary.id, true);
                    }
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                }
                ListEvent::Cancel => shell.close_session_manager(window, cx),
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.session_manager = Some(list);
        self._session_manager_subscription = Some(subscription);
        cx.notify();
    }

    fn close_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_manager = None;
        self._session_manager_subscription = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(crate) fn session_summaries(&self) -> Vec<horizon_workspace::types::SessionSummary> {
        self.workspace.session_summaries()
    }

    /// External (control-plane) operations — the CLI's verbs, mirroring
    /// the Floem shell's `external_commands` semantics: `activate:
    /// false` never steals focus. `prompt` (agent sessions only) sends
    /// the first user message right after the session starts — the
    /// create-with-prompt composite from the CLI design. `role_id` is
    /// fixed by the caller (e.g. `new-config-agent`), never client-supplied
    /// — see `pending_roles`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn external_new_session(
        &mut self,
        kind: PaneKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        split: Option<(SessionId, SplitAxis)>,
        activate: bool,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let cwd = matches!(kind, PaneKind::Terminal).then(|| self.resolve_new_terminal_cwd(cx));
        let session_id = match split {
            Some((target, axis)) => self
                .workspace
                .split_session_with_new_session(target, kind, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_new_session_activated(kind, activate),
        };
        if let Some(cwd) = cwd {
            self.pending_terminal_cwds.insert(session_id, cwd);
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
        Ok(())
    }

    pub(crate) fn external_attach(
        &mut self,
        session_id: SessionId,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.workspace
            .attach_existing_session_to_split_activated(session_id, activate)
            .ok_or_else(|| "unknown session".to_string())?;
        self.reconcile(window, cx);
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }

    pub(crate) fn external_terminate(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if !self.workspace.terminate_session(session_id) {
            return Err("unknown session".to_string());
        }
        self.reconcile(window, cx);
        Ok(())
    }

    /// Session-targeted approve/deny/cancel, for a control-plane caller
    /// that names an explicit `session_id` rather than "whichever pane is
    /// active" (unlike `CommandId::ApproveToolCall`/`DenyToolCall`/
    /// `CancelAgentTurn`, which resolve against `active_agent_session`).
    pub(crate) fn external_approve(
        &mut self,
        session_id: SessionId,
        call_id: horizon_agent::contract::ToolCallId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).approve(call_id);
        Ok(())
    }

    pub(crate) fn external_deny(
        &mut self,
        session_id: SessionId,
        call_id: horizon_agent::contract::ToolCallId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).deny(call_id);
        Ok(())
    }

    pub(crate) fn external_cancel(
        &mut self,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).cancel();
        Ok(())
    }

    pub(crate) fn external_terminate_all_detached(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for summary in self.workspace.detached_session_summaries() {
            self.workspace.terminate_session(summary.id);
        }
        self.reconcile(window, cx);
    }

    pub(crate) fn command_state_with(&self, cx: &App) -> CommandState {
        let (has_pending_approval, has_turn_in_flight) = self
            .active_agent_session()
            .map(|session| {
                let session = session.read(cx);
                let pending =
                    !horizon_agent::frame::pending_approval_call_ids_in(&session.frame.items)
                        .is_empty();
                let in_flight = matches!(
                    session.frame.state,
                    Some(horizon_agent::contract::SessionState::Running)
                        | Some(horizon_agent::contract::SessionState::ToolRunning)
                );
                (pending, in_flight)
            })
            .unwrap_or((false, false));
        CommandState {
            tab_count: self.workspace.tab_count(),
            visible_pane_count: self.workspace.visible_panes().len(),
            has_active_session: self.workspace.active_session_id().is_some(),
            detached_session_count: self.workspace.detached_session_count(),
            has_pending_approval,
            has_turn_in_flight,
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        let entries = command_entries(self.command_state_with(cx));
        let list =
            cx.new(|cx| ListState::new(PaletteDelegate::new(entries), window, cx).searchable(true));
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let entry = list.read(cx).delegate().entry_at(*index).cloned();
                    shell.close_palette(window, cx);
                    if let Some(entry) = entry.filter(|entry| entry.enabled) {
                        shell.execute(entry.spec.id, window, cx);
                    }
                }
                ListEvent::Cancel => shell.close_palette(window, cx),
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.palette = Some(list);
        self._palette_subscription = Some(subscription);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self._palette_subscription = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        // The model detaches the session; in M2 the view (and its PTY)
        // simply drops with it — close-vs-terminate parity needs the M3
        // Registry.
        self.workspace.close_active_pane();
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.workspace.tab_count();
        if count > 1 {
            let next = (self.workspace.active_tab_index() + 1) % count;
            self.workspace.activate_tab_index(next);
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        self.workspace.activate_tab_index(index);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn activate_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.activate_pane(pane_id);
        self.sync_terminal_focus(window, cx);
        cx.notify();
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.workspace.tab_summaries();
        div()
            .flex()
            .flex_row()
            .gap_1()
            .px_2()
            .py_1()
            .bg(rgb(0x101216))
            .children(tabs.into_iter().map(|tab| {
                let index = tab.index;
                div()
                    .id(("tab", index))
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .text_size(px(12.0))
                    .text_color(rgb(if tab.active { 0xe9ecf2 } else { 0x8a90a0 }))
                    .when(tab.active, |this| this.bg(rgb(0x2a2e3a)))
                    .child(format!("{} {}", index + 1, tab.title))
                    .on_click(cx.listener(move |shell, _, window, cx| {
                        shell.activate_tab(index, window, cx);
                    }))
            }))
    }

    fn render_node(&self, node: &LayoutNode, path: String, cx: &mut Context<Self>) -> AnyElement {
        match node {
            LayoutNode::Pane(pane_id) => {
                let pane_id = *pane_id;
                let is_cursor = self.workspace.is_workspace_mode_active()
                    && self.workspace.cursor_pane_id() == Some(pane_id);
                let is_active = self.workspace.is_active_pane(pane_id);
                let border = if is_cursor {
                    rgb(0x84dcc6) // accent: the mode cursor
                } else if is_active {
                    rgb(0x3a3f4e)
                } else {
                    rgb(theme::background())
                };
                let view = self.panes.get(&pane_id).cloned();
                div()
                    .size_full()
                    .border_1()
                    .border_color(border)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |shell, _, window, cx| {
                            shell.activate_pane(pane_id, window, cx)
                        }),
                    )
                    .children(view.map(|view| view.element()))
                    .into_any_element()
            }
            LayoutNode::Split { axis, children } => {
                let mut group: ResizablePanelGroup = match axis {
                    SplitAxis::Horizontal => h_resizable(SharedString::from(path.clone())),
                    SplitAxis::Vertical => v_resizable(SharedString::from(path.clone())),
                };
                for (index, child) in children.iter().enumerate() {
                    let child_element =
                        self.render_node(&child.node, format!("{path}-{index}"), cx);
                    group = group.child(resizable_panel().child(child_element));
                }
                group.into_any_element()
            }
        }
    }
}

impl Render for WorkspaceShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode_active = self.workspace.is_workspace_mode_active();
        let content = self
            .workspace
            .active_tab()
            .map(|tab| tab.root.clone())
            .map(|root| self.render_node(&root, "root".to_string(), cx));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::background()))
            .key_context(if mode_active {
                MODE_CONTEXT
            } else {
                "Workspace"
            })
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|shell, _: &ToggleWorkspaceMode, window, cx| {
                shell.toggle_mode(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveLeft, _, cx| {
                shell.mode_move(Direction::Left, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveDown, _, cx| {
                shell.mode_move(Direction::Down, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveUp, _, cx| {
                shell.mode_move(Direction::Up, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveRight, _, cx| {
                shell.mode_move(Direction::Right, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeCommit, window, cx| {
                shell.mode_commit(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeCancel, window, cx| {
                shell.mode_cancel(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &NewTab, window, cx| {
                shell.execute(CommandId::NewTab, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &NewAgentTab, window, cx| {
                shell.create_session(PaneKind::Agent, None, Placement::NewTab, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &SplitPane, window, cx| {
                shell.execute(CommandId::SplitRight, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ClosePane, window, cx| {
                shell.execute(CommandId::CloseActivePane, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &NextTab, window, cx| {
                shell.next_tab(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &OpenPalette, window, cx| {
                shell.open_palette(window, cx);
            }))
            .on_action(cx.listener(|shell, action: &RunCommand, window, cx| {
                shell.execute(action.id, window, cx);
            }))
            .child(self.render_tab_strip(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .children(content)
                    .when_some(self.palette.clone(), |this, palette| {
                        this.child(
                            div()
                                .id("palette-backdrop")
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .justify_center()
                                .items_start()
                                .pt(px(64.0))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.close_palette(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(560.0))
                                        .h(px(400.0))
                                        .bg(rgb(0x1b1e26))
                                        .border_1()
                                        .border_color(rgb(0x2a2e3a))
                                        .rounded_md()
                                        .overflow_hidden()
                                        .child(List::new(&palette)),
                                ),
                        )
                    })
                    .when_some(self.view_chooser.clone(), |this, chooser| {
                        this.child(
                            div()
                                .id("view-chooser-backdrop")
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .justify_center()
                                .items_start()
                                .pt(px(64.0))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.pending_placement = None;
                                        shell.close_view_chooser(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(420.0))
                                        .h(px(220.0))
                                        .bg(rgb(0x1b1e26))
                                        .border_1()
                                        .border_color(rgb(0x2a2e3a))
                                        .rounded_md()
                                        .overflow_hidden()
                                        .child(List::new(&chooser)),
                                ),
                        )
                    })
                    .when_some(self.session_manager.clone(), |this, manager| {
                        this.child(
                            div()
                                .id("session-manager-backdrop")
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .justify_center()
                                .items_start()
                                .pt(px(64.0))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.close_session_manager(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(560.0))
                                        .h(px(400.0))
                                        .bg(rgb(0x1b1e26))
                                        .border_1()
                                        .border_color(rgb(0x2a2e3a))
                                        .rounded_md()
                                        .overflow_hidden()
                                        .child(List::new(&manager)),
                                ),
                        )
                    }),
            )
    }
}
