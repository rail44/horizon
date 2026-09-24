//! A from-scratch board prototype, reachable only as a named preview.
//!
//! It is master–detail in one view: the task list on the left, the selected
//! task's thread on the right, with no mode switch between reading the list
//! and reading a thread.
//!
//! One view renders several arrangements of that: [`Layout`] is chosen at
//! construction, the model, the state and the key map below are shared, and
//! only [`Render`] branches on it. [`Layout::Prototype`] is the first
//! arrangement ([`list`]/[`thread`]); the others are laid out in
//! [`directions`] out of the pieces in [`parts`], to the measurements in
//! [`spec`].
//!
//! It reads a [`Store`] through the same
//! [`BoardStoreSource`]/[`run_store_job`] pair the shipped board pane uses,
//! so a preview's in-memory store and a log-backed one look the same from
//! here; on the former every write answers [`StoreError::ReadOnly`], which
//! the view reports on its notice line.
//!
//! Nothing in this module reaches the shell: it emits no commands and holds
//! no session handles, so it compiles for the preview plugin target as it
//! stands.

use std::collections::{HashMap, HashSet};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_component::{h_flex, v_flex};
use horizon_board::{Item, Store, StoreError};
use horizon_workspace::SessionId;

use crate::board_pane::activity::BoardSessionActivity;
use crate::board_pane::execute::{run_store_job, BoardStoreSource};
use crate::theme;

mod directions;
mod list;
mod model;
mod parts;
pub(crate) mod previews;
mod spec;
mod thread;

use model::{Command, Row};
use spec::Layout;

/// The prototype's root view.
pub(crate) struct BoardNextView {
    /// Which arrangement this view renders. The model, the state, and the
    /// key map below are shared by all of them.
    layout: Layout,
    store: BoardStoreSource,
    /// The activity of every session the board binds, as the shell would
    /// report it.
    activity: HashMap<SessionId, BoardSessionActivity>,
    rows: Vec<Row>,
    positions: HashMap<u64, String>,
    selected: Option<u64>,
    finished_expanded: bool,
    /// Tasks whose body is folded away; a body starts open.
    body_collapsed: HashSet<u64>,
    /// Message ids shown in full despite being long enough to fold.
    expanded_messages: HashSet<String>,
    /// One line under the thread header: the last write refusal or error.
    notice: Option<String>,
    /// A task to select once the first read lands, for a preview that opens
    /// on a thread.
    open: Option<u64>,
    /// Whether the rail direction shows the full list. The other
    /// directions always do.
    rail_expanded: bool,
    composer: Entity<InputState>,
    /// The layout directions' composer: two lines by default, growing with
    /// what is typed.
    reply: Entity<TextareaState>,
    status_input: Entity<InputState>,
    list_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _composer_subscription: Subscription,
    _reply_subscription: Subscription,
    _status_subscription: Subscription,
}

impl BoardNextView {
    /// `open` names the task the view selects once the first read lands;
    /// without it the first row of the steering order is selected.
    /// `layout` picks the arrangement.
    pub(crate) fn new(
        store: Store,
        activity: HashMap<SessionId, BoardSessionActivity>,
        open: Option<u64>,
        layout: Layout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Reply…")
                .submit_on_enter(true)
        });
        let _composer_subscription = cx.subscribe_in(
            &composer,
            window,
            |view, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.execute(Command::PostMessage, window, cx);
                }
            },
        );
        let reply = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("返信を書く")
                .auto_grow(spec::COMPOSER_MIN_ROWS, spec::COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let _reply_subscription = cx.subscribe_in(
            &reply,
            window,
            |view, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.execute(Command::PostMessage, window, cx);
                }
            },
        );
        let status_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("status…")
                .submit_on_enter(true)
        });
        let _status_subscription = cx.subscribe_in(
            &status_input,
            window,
            |view, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.execute(Command::SetStatus, window, cx);
                }
            },
        );
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let view = Self {
            layout,
            store: BoardStoreSource::Ready(store),
            activity,
            rows: Vec::new(),
            positions: HashMap::new(),
            selected: None,
            finished_expanded: false,
            body_collapsed: HashSet::new(),
            expanded_messages: HashSet::new(),
            notice: None,
            open,
            rail_expanded: false,
            composer,
            reply,
            status_input,
            list_scroll: ScrollHandle::new(),
            focus_handle,
            _composer_subscription,
            _reply_subscription,
            _status_subscription,
        };
        view.load(cx);
        view
    }

    // -- store ------------------------------------------------------------

    fn load(&self, cx: &mut Context<Self>) {
        let source = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = run_store_job(cx, source, |store| {
                Box::pin(async move {
                    Ok((
                        store.list(None, true)?.items,
                        store.read_positions("owner")?,
                    ))
                })
            })
            .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok((items, positions)) => view.set_loaded(items, positions, cx),
                Err(error) => view.set_notice(error.to_string(), cx),
            });
        })
        .detach();
    }

    fn set_loaded(
        &mut self,
        items: Vec<Item>,
        positions: HashMap<u64, String>,
        cx: &mut Context<Self>,
    ) {
        self.positions = positions;
        self.rows = model::rows(&items, &self.positions, &self.activity);
        if let Some(open) = self.open.take() {
            if let Some(row) = self.rows.iter().find(|row| row.item.id == open) {
                // A task that opens into the finished band unfolds it,
                // rather than selecting a row nothing shows.
                self.finished_expanded |= row.group == model::Group::Finished;
                self.selected = Some(open);
            }
        }
        let visible = self.visible_ids();
        if !self.selected.is_some_and(|id| visible.contains(&id)) {
            self.selected = visible.first().copied();
        }
        cx.notify();
    }

    /// Runs one write and reloads on success. A store that takes no writes
    /// lands in the notice line instead.
    fn mutate(
        &self,
        cx: &mut Context<Self>,
        job: impl FnOnce(Store) -> crate::board_pane::execute::StoreJob<()> + Send + 'static,
    ) {
        let source = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = run_store_job(cx, source, job).await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(()) => {
                    view.notice = None;
                    view.load(cx);
                }
                Err(error) => {
                    let notice = if view.layout == Layout::Prototype {
                        write_refusal(&error)
                    } else {
                        write_refusal_ja(&error)
                    };
                    view.set_notice(notice, cx)
                }
            });
        })
        .detach();
    }

    fn set_notice(&mut self, notice: String, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }

    // -- selection --------------------------------------------------------

    fn visible_ids(&self) -> Vec<u64> {
        model::visible_rows(&self.rows, self.finished_expanded)
            .into_iter()
            .filter_map(|index| self.rows.get(index).map(|row| row.item.id))
            .collect()
    }

    fn selected_row(&self) -> Option<&Row> {
        let id = self.selected?;
        self.rows.iter().find(|row| row.item.id == id)
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let visible = self.visible_ids();
        let next = model::step_selection(&visible, self.selected, forward);
        if next != self.selected {
            self.selected = next;
            if let Some(index) = next.and_then(|id| self.scroll_index(&visible, id)) {
                self.list_scroll.scroll_to_item(index);
            }
            cx.notify();
        }
    }

    /// Where `id` sits among the scroll container's children. The finished
    /// band is preceded by its one header row, which is a child of the same
    /// container, so a row inside that band is one further down.
    fn scroll_index(&self, visible: &[u64], id: u64) -> Option<usize> {
        let position = visible.iter().position(|row| *row == id)?;
        let in_finished_band = self
            .rows
            .iter()
            .any(|row| row.item.id == id && row.group == model::Group::Finished);
        Some(position + usize::from(in_finished_band))
    }

    // -- commands ---------------------------------------------------------

    fn execute(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::SelectNext => self.step(true, cx),
            Command::SelectPrevious => self.step(false, cx),
            Command::SelectTask(id) => {
                self.selected = Some(id);
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            Command::FocusComposer => {
                let handle = if self.layout == Layout::Prototype {
                    self.composer.read(cx).focus_handle(cx)
                } else {
                    self.reply.read(cx).focus_handle(cx)
                };
                window.focus(&handle, cx);
                cx.notify();
            }
            Command::FocusList => {
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            Command::ToggleFinished => {
                self.finished_expanded = !self.finished_expanded;
                let visible = self.visible_ids();
                if !self.selected.is_some_and(|id| visible.contains(&id)) {
                    self.selected = visible.first().copied();
                }
                cx.notify();
            }
            Command::ToggleRail => {
                self.rail_expanded = !self.rail_expanded;
                cx.notify();
            }
            Command::ToggleLongMessages => self.toggle_long_messages(cx),
            Command::ToggleMessage(id) => {
                if !self.expanded_messages.remove(&id) {
                    self.expanded_messages.insert(id);
                }
                cx.notify();
            }
            Command::ToggleBody => {
                if let Some(id) = self.selected {
                    if !self.body_collapsed.remove(&id) {
                        self.body_collapsed.insert(id);
                    }
                }
                cx.notify();
            }
            Command::SetStatus => self.set_status(window, cx),
            Command::ToggleClosed => self.toggle_closed(cx),
            Command::OpenTaskSession => {
                self.set_notice("Opening a task session is a shell command; the prototype only shows the affordance.".into(), cx)
            }
            Command::PostMessage => self.post_message(window, cx),
        }
    }

    /// Whether a post is long enough to be folded in this direction. The
    /// prototype folds on its own line/character budget; the layout
    /// directions fold on rendered length at the post's own measure, and
    /// never fold an unread post.
    fn foldable(&self, comment: &horizon_board::Comment, unread: bool) -> bool {
        if self.layout == Layout::Prototype {
            return model::fold(&comment.text).is_some();
        }
        let cells = parts::post_cells(model::voice(&comment.author));
        !unread && model::fold_preview(&comment.text, cells as usize).is_some()
    }

    /// One key for the whole open thread: the first press opens every folded
    /// message in it, the next folds them all back.
    fn toggle_long_messages(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let read = model::read_through(&row.item, &self.positions);
        let foldable: Vec<String> = row
            .item
            .comments
            .iter()
            .enumerate()
            .filter(|(index, comment)| self.foldable(comment, *index >= read))
            .map(|(_, comment)| comment.id.clone())
            .collect();
        let any_folded = foldable
            .iter()
            .any(|id| !self.expanded_messages.contains(id));
        for id in foldable {
            if any_folded {
                self.expanded_messages.insert(id);
            } else {
                self.expanded_messages.remove(&id);
            }
        }
        cx.notify();
    }

    fn post_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let prototype = self.layout == Layout::Prototype;
        let text = if prototype {
            self.composer.read(cx).value().trim().to_string()
        } else {
            self.reply.read(cx).value().trim().to_string()
        };
        if text.is_empty() {
            return;
        }
        if prototype {
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
        } else {
            self.reply
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.mutate(cx, move |store| {
            Box::pin(async move { store.comment(id, "owner", &text).await })
        });
    }

    fn set_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let status = self.status_input.read(cx).value().trim().to_string();
        if status.is_empty() {
            return;
        }
        self.status_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.mutate(cx, move |store| {
            Box::pin(async move { store.set_status(id, &status).await })
        });
    }

    fn toggle_closed(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let id = row.item.id;
        let closed = !row.item.is_closed;
        self.mutate(cx, move |store| {
            Box::pin(async move { store.set_closed(id, closed, None).await })
        });
    }

    // -- input ------------------------------------------------------------

    /// Whether keystrokes belong to one of the two text fields rather than
    /// to the key map. The fields are inside the focus path, so their keys
    /// bubble through the root handler on their way to the input.
    fn editing(&self, window: &Window, cx: &App) -> bool {
        self.composer.read(cx).focus_handle(cx).is_focused(window)
            || self.reply.read(cx).focus_handle(cx).is_focused(window)
            || self
                .status_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        if self.editing(window, cx) {
            // Esc is the way back out of a field; everything else is the
            // field's own.
            if keystroke.key == "escape" {
                self.execute(Command::FocusList, window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let Some(command) = model::command_for_key(&keystroke.key, self.layout) else {
            return;
        };
        self.execute(command, window, cx);
        cx.stop_propagation();
    }
}

/// What the view shows for a write the store would not take. A read-only
/// store is the preview's normal state, not a fault, so it reads as one.
fn write_refusal(error: &StoreError) -> String {
    match error {
        StoreError::ReadOnly => "This board is read-only — the write was not recorded.".to_string(),
        other => other.to_string(),
    }
}

/// The same, in the chrome language the layout directions are written in.
fn write_refusal_ja(error: &StoreError) -> String {
    match error {
        StoreError::ReadOnly => {
            "このボードは読み取り専用です。書き込みは記録されませんでした。".to_string()
        }
        other => other.to_string(),
    }
}

impl Focusable for BoardNextView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BoardNextView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list_focused = self.focus_handle.is_focused(window);
        let body = match self.layout {
            Layout::Prototype => h_flex()
                .size_full()
                .child(self.render_list(cx))
                .child(self.render_thread(cx))
                .into_any_element(),
            layout => self.render_direction(layout, list_focused, cx),
        };
        h_flex()
            .id("board-next")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .size_full()
            .bg(rgb(theme::background()))
            .text_color(theme::text_primary())
            .child(body)
    }
}

/// Text sizes the prototype draws with, in one place so the two columns
/// stay in step.
mod size {
    use gpui::{px, Pixels};

    pub(super) const TITLE: Pixels = px(13.0);
    pub(super) const BODY: Pixels = px(13.0);
    pub(super) const META: Pixels = px(11.0);
    pub(super) const HEADING: Pixels = px(15.0);
}

#[cfg(test)]
mod tests {
    use super::{write_refusal, write_refusal_ja};
    use horizon_board::StoreError;

    #[test]
    fn a_read_only_store_reads_as_a_state_not_a_failure() {
        assert!(write_refusal(&StoreError::ReadOnly).contains("read-only"));
        assert!(write_refusal_ja(&StoreError::ReadOnly).contains("読み取り専用"));
        let other = StoreError::ItemNotFound(7);
        assert_eq!(write_refusal(&other), other.to_string());
        assert_eq!(write_refusal_ja(&other), other.to_string());
    }
}
