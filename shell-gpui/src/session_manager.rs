//! The session manager modal, on the same gpui-component searchable
//! `List` pattern as the palette: attach or jump to a session on
//! confirm, terminate on secondary confirm. The shell owns the events;
//! this delegate only filters and renders summaries.

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::IndexPath;
use horizon_workspace::types::SessionSummary;

pub struct SessionManagerDelegate {
    all: Vec<SessionSummary>,
    filtered: Vec<SessionSummary>,
}

impl SessionManagerDelegate {
    pub fn new(sessions: Vec<SessionSummary>) -> Self {
        Self {
            filtered: sessions.clone(),
            all: sessions,
        }
    }

    pub fn summary_at(&self, index: IndexPath) -> Option<&SessionSummary> {
        self.filtered.get(index.row)
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
            ("attached", rgb(0x98c379))
        } else {
            ("detached", rgb(0x8a90a0))
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
                            .text_color(rgb(0xe9ecf2))
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
}
