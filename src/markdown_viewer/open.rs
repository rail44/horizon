//! The "Open Markdown File…" path-input modal: a searchable `List` whose
//! query is treated as a file path. Confirming opens the path as a Markdown
//! view pane. This lives in the markdown viewer module because it is
//! specific to this view kind; the lifecycle wiring is in
//! `src/workspace/modals.rs`.

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, Icon, IconName, IndexPath};

use crate::theme;

pub(crate) struct MarkdownOpenDelegate {
    query: String,
    selected: Option<IndexPath>,
}

impl MarkdownOpenDelegate {
    pub(crate) fn new() -> Self {
        Self {
            query: String::new(),
            selected: None,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }
}

impl ListDelegate for MarkdownOpenDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        if self.query.trim().is_empty() {
            0
        } else {
            1
        }
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.query = query.to_string();
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let selected = self.selected == Some(index);
        let text = if self.query.trim().is_empty() {
            return None;
        } else {
            format!("Open Markdown file: {}", self.query.trim())
        };
        let mut color = theme::text_primary();
        if selected {
            color = theme::readable_on(color, theme::surface_selected());
        }
        Some(
            ListItem::new(index).child(
                div()
                    .py_0p5()
                    .text_size(px(13.0))
                    .text_color(color)
                    .child(text),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = index;
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_color(theme::readable_on(
                theme::text_muted(),
                rgb(theme::background()).into(),
            ))
            .child(Icon::new(IconName::Inbox).size_12())
    }
}
