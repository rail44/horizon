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

use crate::palette::PaletteDelegate;
use crate::session_manager::SessionManagerDelegate;
use crate::terminal::{TerminalSession, TerminalView};
use crate::theme;
use horizon_workspace::SessionId;

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
        KeyBinding::new("s", SplitPane, Some(MODE_CONTEXT)),
        KeyBinding::new("x", ClosePane, Some(MODE_CONTEXT)),
        KeyBinding::new("tab", NextTab, Some(MODE_CONTEXT)),
        KeyBinding::new(":", OpenPalette, Some(MODE_CONTEXT)),
    ]);
}

pub struct WorkspaceShell {
    workspace: Workspace,
    // The session store — the GPUI shell's Registry counterpart: PTY
    // sessions live here keyed by SessionId, independent of pane views,
    // so closing a pane detaches (session survives, scrollback intact)
    // and terminating is the explicit destructive path.
    sessions: HashMap<SessionId, Entity<TerminalSession>>,
    panes: HashMap<PaneId, Entity<TerminalView>>,
    // Focused while workspace mode is active, so mode keys dispatch here
    // instead of reaching the terminal.
    focus_handle: FocusHandle,
    palette: Option<Entity<ListState<PaletteDelegate>>>,
    _palette_subscription: Option<Subscription>,
    session_manager: Option<Entity<ListState<SessionManagerDelegate>>>,
    _session_manager_subscription: Option<Subscription>,
}

impl WorkspaceShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            workspace: Workspace::mvp(),
            sessions: HashMap::new(),
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
        let known: std::collections::HashSet<SessionId> = self
            .workspace
            .session_summaries()
            .iter()
            .map(|summary| summary.id)
            .collect();
        self.sessions.retain(|id, session| {
            let keep = known.contains(id);
            if !keep {
                session.read(cx).shutdown();
            }
            keep
        });
        for id in known {
            self.sessions
                .entry(id)
                .or_insert_with(|| cx.new(TerminalSession::spawn));
        }

        let pane_ids = self.workspace.all_pane_ids();
        self.panes.retain(|id, _| pane_ids.contains(id));
        for pane_id in pane_ids {
            if self.panes.contains_key(&pane_id) {
                continue;
            }
            let Some(session) = self
                .workspace
                .terminal_session_id(pane_id)
                .and_then(|id| self.sessions.get(&id).cloned())
            else {
                continue;
            };
            self.panes.insert(
                pane_id,
                cx.new(|cx| TerminalView::new(session.clone(), window, cx)),
            );
        }
        cx.notify();
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

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        self.workspace
            .open_tab_with_new_session_activated(PaneKind::Terminal, true);
        self.reconcile(window, cx);
        self.focus_active(window, cx);
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
            CommandId::NewTab => self.new_tab(window, cx),
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
            // Unwired until their subsystems arrive: agent commands (M4),
            // config reload (M3 config port).
            CommandId::ApproveToolCall
            | CommandId::DenyToolCall
            | CommandId::CancelAgentTurn
            | CommandId::ReloadAgentRuntime
            | CommandId::ReloadConfig => {}
        }
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

    fn command_state(&self) -> CommandState {
        CommandState {
            tab_count: self.workspace.tab_count(),
            visible_pane_count: self.workspace.visible_panes().len(),
            has_active_session: self.workspace.active_session_id().is_some(),
            detached_session_count: self.workspace.detached_session_count(),
            has_pending_approval: false,
            has_turn_in_flight: false,
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        let entries = command_entries(self.command_state());
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
                    rgb(theme::BACKGROUND)
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
                    .children(view)
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
            .bg(rgb(theme::BACKGROUND))
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
