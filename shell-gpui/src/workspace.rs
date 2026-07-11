//! The GPUI projection of the shared workspace model
//! (`crates/horizon-workspace`): tab strip, recursive split rendering on
//! gpui-component's resizable primitives, pane focus, and workspace mode
//! with spatial navigation. The model owns all layout truth; this module
//! only renders it and translates GPUI actions into model operations.
//!
//! The key bindings registered in [`init`] are M2 stand-ins wired
//! straight to model calls — M3 replaces them with the command model
//! (`CommandId` + keymap config), at which point every handler here
//! becomes a binding to a command instead.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::{List, ListEvent, ListState};
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizablePanelGroup};
use horizon_workspace::commands::{command_entries, CommandId, CommandState};
use horizon_workspace::types::LayoutNode;
use horizon_workspace::{Direction, PaneId, PaneKind, SplitAxis, Workspace};

use crate::agent::{AgentSession, AgentView, AgentdHandle};
use crate::palette::PaletteDelegate;
use crate::session_manager::SessionManagerDelegate;
use crate::terminal::{TerminalSession, TerminalView};
use crate::theme;
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

const MODE_CONTEXT: &str = "WorkspaceMode";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-'", ToggleWorkspaceMode, None),
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
    ]);
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
    // Lazily connected on the first agent session (the Floem shell
    // connects async at startup; lazy-blocking is the v1 tradeoff here).
    agentd: Option<AgentdHandle>,
    panes: HashMap<PaneId, PaneView>,
    // Focused while workspace mode is active, so mode keys dispatch here
    // instead of reaching the terminal.
    focus_handle: FocusHandle,
    palette: Option<Entity<ListState<PaletteDelegate>>>,
    _palette_subscription: Option<Subscription>,
    session_manager: Option<Entity<ListState<SessionManagerDelegate>>>,
    _session_manager_subscription: Option<Subscription>,
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
            agentd: None,
            panes: HashMap::new(),
            focus_handle: cx.focus_handle(),
            palette: None,
            _palette_subscription: None,
            session_manager: None,
            _session_manager_subscription: None,
        };
        shell.reconcile(window, cx);
        shell.focus_active(window, cx);
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
                    let socket_path = self.socket_path.clone();
                    let id = summary.id;
                    self.sessions.entry(id).or_insert_with(|| {
                        cx.new(|cx| TerminalSession::spawn(id, &socket_path, cx))
                    });
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
                    let session_handle =
                        handle.start_session(agent_session_id(summary.id), provider_id, None);
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

    /// Lazily connects to `horizon-agentd` (spawning it if needed) and
    /// wires the host-tool responder: `workspace.snapshot` requests are
    /// answered on the UI thread from the live model, mirroring the
    /// Floem shell's `wire_host_tool_responder`.
    fn agentd(&mut self, cx: &mut Context<Self>) -> Result<AgentdHandle, String> {
        if let Some(handle) = &self.agentd {
            return Ok(handle.clone());
        }
        let (handle, host_tool_rx) = AgentdHandle::connect(
            &horizon_agent::socket::default_socket_path(),
            &self.socket_path,
        )?;
        self.agentd = Some(handle.clone());

        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(request) = host_tool_rx.recv() {
                if async_tx.unbounded_send(request).is_err() {
                    return;
                }
            }
        });
        let responder = handle.clone();
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
                responder.respond_host_tool(horizon_agent::wire::HostToolResponse {
                    request_id: request.request_id,
                    output,
                });
            }
        })
        .detach();

        Ok(handle)
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self
            .workspace
            .cursor_pane_id()
            .and_then(|id| self.panes.get(&id))
        {
            window.focus(&view.focus_handle(cx), cx);
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

    fn new_tab(&mut self, kind: PaneKind, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        self.workspace
            .open_tab_with_new_session_activated(kind, true);
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// The active pane's agent session, when it is an agent pane.
    fn active_agent_session(&self) -> Option<Entity<AgentSession>> {
        let pane_id = self.workspace.cursor_pane_id()?;
        let session_id = self.workspace.agent_session_id(pane_id)?;
        self.agent_sessions.get(&session_id).cloned()
    }

    fn split_pane(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        if let Some(target) = self.workspace.active_session_id() {
            self.workspace
                .split_session_with_new_session(target, PaneKind::Terminal, axis, true);
        }
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// The M3 dispatch point: every surface (palette, keybindings, and
    /// later the control plane) funnels through here — the GPUI
    /// counterpart of the Floem shell's `execute_command`.
    fn execute(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            CommandId::SplitRight => self.split_pane(SplitAxis::Horizontal, window, cx),
            CommandId::SplitDown => self.split_pane(SplitAxis::Vertical, window, cx),
            CommandId::NewTab => self.new_tab(PaneKind::Terminal, window, cx),
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
            // Unwired until the drain/respawn sequence is ported (M5).
            CommandId::ReloadAgentRuntime => {}
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
                    let summary = list.read(cx).delegate().summary_at(*index).cloned();
                    shell.close_session_manager(window, cx);
                    if let Some(summary) = summary {
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
    /// create-with-prompt composite from the CLI design.
    pub(crate) fn external_new_session(
        &mut self,
        kind: PaneKind,
        split: Option<(SessionId, SplitAxis)>,
        activate: bool,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session_id = match split {
            Some((target, axis)) => self
                .workspace
                .split_session_with_new_session(target, kind, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_new_session_activated(kind, activate),
        };
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

    fn activate_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        self.workspace.activate_pane(pane_id);
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
                        cx.listener(move |shell, _, _, cx| shell.activate_pane(pane_id, cx)),
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
                shell.new_tab(PaneKind::Agent, window, cx);
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
