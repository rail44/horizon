//! The Markdown viewer pane: Horizon's second session-less first-party
//! view (`docs/markdown-viewer-design.md`). It reads a local Markdown file
//! once at construction time and renders it with gpui-component's existing
//! `TextView::markdown` renderer (reused from the agent transcript).
//!
//! Out of scope for this slice: editing, external URLs, image rendering,
//! file watching, and CLI-driven opening.

pub(crate) mod open;

use std::path::PathBuf;

use gpui::*;
use gpui_component::text::TextView;

use crate::theme;

pub(crate) struct MarkdownViewer {
    focus_handle: FocusHandle,
    scroll: ScrollHandle,
    path: PathBuf,
    content: String,
    error: Option<String>,
}

impl MarkdownViewer {
    pub(crate) fn new(path: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (content, error) = match std::fs::read_to_string(&path) {
            Ok(text) => (text, None),
            Err(err) => (
                String::new(),
                Some(format!("Could not read {}: {err}", path.display())),
            ),
        };
        Self {
            focus_handle: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            path,
            content,
            error,
        }
    }
}

impl Focusable for MarkdownViewer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MarkdownViewer {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = if let Some(error) = &self.error {
            div()
                .size_full()
                .p_4()
                .text_color(theme::danger())
                .child(error.clone())
                .into_any_element()
        } else {
            div()
                .id("markdown-viewer-body")
                .track_scroll(&self.scroll)
                .size_full()
                .p_4()
                .overflow_y_scroll()
                .child(
                    TextView::markdown(("markdown-viewer", 0usize), self.content.clone())
                        .text_size(px(13.0))
                        .text_color(theme::text_primary()),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(px(11.0))
                    .text_color(theme::text_muted())
                    .child(self.path.display().to_string()),
            )
            .child(body)
    }
}
