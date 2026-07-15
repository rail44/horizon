//! The session manager modal, on the same gpui-component searchable
//! `List` pattern as the palette: attach or jump to a session on
//! confirm, terminate on secondary confirm. The shell owns the events;
//! this delegate only filters and renders summaries.

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::IndexPath;
use horizon_workspace::types::SessionSummary;

use crate::theme;

pub struct SessionManagerDelegate {
    all: Vec<SessionSummary>,
    filtered: Vec<SessionSummary>,
    // Whether the most recent confirm was the secondary one (cmd-enter /
    // right click) — the List calls `confirm` before emitting
    // `ListEvent::Confirm`, so the shell's event handler reads this to
    // pick attach-or-jump (primary) vs terminate (secondary).
    last_confirm_secondary: bool,
}

impl SessionManagerDelegate {
    pub fn new(sessions: Vec<SessionSummary>) -> Self {
        Self {
            filtered: sessions.clone(),
            all: sessions,
            last_confirm_secondary: false,
        }
    }

    pub fn summary_at(&self, index: IndexPath) -> Option<&SessionSummary> {
        self.filtered.get(index.row)
    }

    pub fn last_confirm_secondary(&self) -> bool {
        self.last_confirm_secondary
    }

    /// Replaces the listed sessions (after a terminate, keeping the
    /// modal open on fresh data).
    pub fn reset(&mut self, sessions: Vec<SessionSummary>) {
        self.filtered = sessions.clone();
        self.all = sessions;
    }
}

impl ListDelegate for SessionManagerDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.filtered.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_ascii_lowercase();
        self.filtered = self
            .all
            .iter()
            .filter(|summary| {
                query.is_empty() || summary.title.to_ascii_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let summary = self.filtered.get(index.row)?;
        let (status, status_color) = if summary.attached {
            ("attached", theme::success())
        } else {
            ("detached", theme::text_muted())
        };
        Some(
            ListItem::new(index).child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py_0p5()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::text_primary())
                            .child(summary.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(status_color)
                            .child(status),
                    ),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        _index: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }

    fn confirm(
        &mut self,
        secondary: bool,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.last_confirm_secondary = secondary;
    }
}
