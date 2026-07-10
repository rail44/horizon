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
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizablePanelGroup};
use horizon_workspace::types::LayoutNode;
use horizon_workspace::{Direction, PaneId, PaneKind, SplitAxis, Workspace};

use crate::terminal::TerminalView;
use crate::theme;

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
        NextTab
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
    ]);
}

pub struct WorkspaceShell {
    workspace: Workspace,
    panes: HashMap<PaneId, Entity<TerminalView>>,
    // Focused while workspace mode is active, so mode keys dispatch here
    // instead of reaching the terminal.
    focus_handle: FocusHandle,
}

impl WorkspaceShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            workspace: Workspace::mvp(),
            panes: HashMap::new(),
            focus_handle: cx.focus_handle(),
        };
        shell.reconcile(window, cx);
        shell.focus_active(window, cx);
        shell
    }

    /// Bring the PaneId → view map in line with the model: spawn views
    /// for new panes, drop views whose panes are gone. In M2 every pane
    /// hosts its own fresh terminal; the Registry (detach/reattach)
    /// arrives with M3.
    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.workspace.all_pane_ids();
        self.panes.retain(|id, _| ids.contains(id));
        for id in ids {
            self.panes
                .entry(id)
                .or_insert_with(|| cx.new(|cx| TerminalView::new(window, cx)));
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

    fn split_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.exit_workspace_mode();
        if let Some(target) = self.workspace.active_session_id() {
            self.workspace.split_session_with_new_session(
                target,
                PaneKind::Terminal,
                SplitAxis::Horizontal,
                true,
            );
        }
        self.reconcile(window, cx);
        self.focus_active(window, cx);
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
                shell.new_tab(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &SplitPane, window, cx| {
                shell.split_pane(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ClosePane, window, cx| {
                shell.close_pane(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &NextTab, window, cx| {
                shell.next_tab(window, cx);
            }))
            .child(self.render_tab_strip(cx))
            .child(div().flex_1().min_h_0().children(content))
    }
}
