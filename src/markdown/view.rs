//! The Markdown view: one file read from disk, drawn as running text, with
//! links to other Markdown files followed inside the same pane.
//!
//! The file is watched, so a document another process is writing redraws
//! as it changes. Following a link pushes the current file onto a history
//! the back key returns along; the shell persists only the file shown.

use std::path::PathBuf;

use futures::channel::mpsc::{self, UnboundedSender};
use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{h_flex, v_flex, Sizable as _};

use super::document::{self, Document, LinkTarget};
use crate::file_watch::{self, FileWatch};
use crate::theme;
use crate::ui::parts::{chip, linked_markdown_body};
use crate::ui::spec::*;

const BACK: &str = "Back";
const NO_DOCUMENT: &str = "No document";
const LOADING: &str = "Loading";
const UNSUPPORTED_LINK: &str = "Not a Markdown file";

/// The view moved to another file. The shell records it as the pane's
/// document, so the tab title and a restart follow the file shown.
pub(crate) struct DocumentChanged(pub(crate) PathBuf);

impl EventEmitter<DocumentChanged> for MarkdownView {}

enum Content {
    Loading,
    Loaded(Document),
    Failed(String),
}

pub(crate) struct MarkdownView {
    path: Option<PathBuf>,
    /// The files left by following links, most recent last.
    history: Vec<PathBuf>,
    content: Content,
    /// A line under the header: a link the view did not follow.
    notice: Option<SharedString>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    /// Where the body's link handler sends a clicked target. The handler
    /// must be `Send + Sync`, so it cannot hold this entity; the pump task
    /// below brings the target back onto it.
    links: UnboundedSender<SharedString>,
    _link_pump: Task<()>,
    /// Off only in tests: a real watch runs on a thread of its own, which
    /// gpui's deterministic test scheduler rejects.
    watch_files: bool,
    _watch: Option<FileWatch>,
}

impl MarkdownView {
    pub(crate) fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(path, true, window, cx)
    }

    fn build(
        path: Option<PathBuf>,
        watch_files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let (links, mut clicked) = mpsc::unbounded::<SharedString>();
        let link_pump = cx.spawn(async move |this, cx| {
            while let Some(target) = clicked.next().await {
                if this
                    .update(cx, |view, cx| view.follow_link(&target, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        let mut view = Self {
            path: None,
            history: Vec::new(),
            content: Content::Loading,
            notice: None,
            scroll: ScrollHandle::new(),
            focus_handle,
            links,
            _link_pump: link_pump,
            watch_files,
            _watch: None,
        };
        if let Some(path) = path {
            view.show(path, cx);
        }
        view
    }

    /// Points the view at `path` from outside (the CLI): the history is
    /// left as it is, since the jump was not a link followed here.
    pub(crate) fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.path.as_deref() == Some(path.as_path()) {
            self.reload(cx);
            return;
        }
        self.show(path, cx);
    }

    fn show(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.notice = None;
        self.content = Content::Loading;
        self.scroll.set_offset(Point::default());
        if self.watch_files {
            let weak = cx.entity().downgrade();
            self._watch = file_watch::watch(path.clone(), cx, move |cx| {
                weak.update(cx, |view, cx| view.reload(cx)).ok();
            });
        }
        self.path = Some(path.clone());
        self.reload(cx);
        cx.emit(DocumentChanged(path));
    }

    /// Reads the file again, keeping what is drawn until the read lands so
    /// a live edit does not flash the loading state.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let read_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::read_to_string(&read_path) })
                .await;
            let _ = this.update(cx, |view, cx| {
                // A read that lands after the view moved on is stale.
                if view.path.as_deref() != Some(path.as_path()) {
                    return;
                }
                view.content = match result {
                    Ok(source) => Content::Loaded(document::parse(&source)),
                    Err(error) => Content::Failed(format!("{}: {error}", path.display())),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn follow_link(&mut self, target: &str, cx: &mut Context<Self>) {
        let Some(current) = self.path.clone() else {
            return;
        };
        match document::resolve_link(&current, target) {
            LinkTarget::Document(path) => {
                if path != current {
                    self.history.push(current);
                    self.show(path, cx);
                }
            }
            LinkTarget::External(url) => cx.open_url(&url),
            LinkTarget::Unsupported => {
                self.notice = Some(format!("{UNSUPPORTED_LINK}: {target}").into());
                cx.notify();
            }
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.history.pop() {
            self.show(previous, cx);
        }
    }

    fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let mut offset = self.scroll.offset();
        let max = self.scroll.max_offset().y;
        // Offsets grow negative as the content moves up.
        offset.y = (offset.y - delta).clamp(-max, px(0.));
        self.scroll.set_offset(offset);
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        let line = body().line_height * 3.;
        let page = window.viewport_size().height * 0.8;
        match keystroke.key.as_str() {
            "j" | "down" => self.scroll_by(line, cx),
            "k" | "up" => self.scroll_by(-line, cx),
            "space" | "pagedown" => self.scroll_by(page, cx),
            "pageup" => self.scroll_by(-page, cx),
            "backspace" | "h" | "left" => self.back(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let doc = match &self.content {
            Content::Loaded(doc) => Some(doc),
            _ => None,
        };
        let title = doc.and_then(|doc| doc.title.clone());
        let fields = doc.map(|doc| doc.fields.as_slice()).unwrap_or_default();
        let can_go_back = !self.history.is_empty();
        if title.is_none() && fields.is_empty() && !can_go_back {
            return None;
        }
        let heading = t1();
        Some(
            v_flex()
                .flex_none()
                .w_full()
                .px(pad_x())
                .pt(gap_section())
                .gap(gap_tight())
                .when(can_go_back, |column| {
                    column.child(
                        h_flex().child(
                            Button::new("markdown-back")
                                .ghost()
                                .xsmall()
                                .label(BACK)
                                .on_click(cx.listener(|view, _, _, cx| view.back(cx))),
                        ),
                    )
                })
                .when_some(title, |column, title| {
                    column.child(
                        div()
                            .text_size(heading.size)
                            .line_height(heading.line_height)
                            .font_weight(ANCHOR)
                            .text_color(theme::text_primary())
                            .child(title),
                    )
                })
                .when(!fields.is_empty(), |column| {
                    column.child(h_flex().flex_wrap().gap(gap_label()).children(
                        fields.iter().map(|(key, value)| {
                            chip(format!("{key} {value}"), theme::text_muted())
                        }),
                    ))
                })
                .into_any_element(),
        )
    }

    fn render_body(&self) -> AnyElement {
        let muted = |text: SharedString| {
            div()
                .px(pad_x())
                .pt(gap_section())
                .text_size(meta().size)
                .line_height(meta().line_height)
                .text_color(theme::text_muted())
                .child(text)
                .into_any_element()
        };
        let Some(path) = self.path.as_ref() else {
            return muted(NO_DOCUMENT.into());
        };
        match &self.content {
            Content::Loading => muted(LOADING.into()),
            Content::Failed(error) => div()
                .px(pad_x())
                .pt(gap_section())
                .text_size(meta().size)
                .line_height(meta().line_height)
                .text_color(theme::danger())
                .child(error.clone())
                .into_any_element(),
            Content::Loaded(doc) => {
                let links = self.links.clone();
                v_flex()
                    .id("markdown-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px(pad_x())
                    .pt(gap_section())
                    .pb(gap_composer())
                    .child(linked_markdown_body(
                        ElementId::Name(format!("markdown:{}", path.display()).into()),
                        doc.body.clone(),
                        theme::text_primary(),
                        move |target, _, _| {
                            let _ = links.unbounded_send(target.clone());
                        },
                    ))
                    .into_any_element()
            }
        }
    }
}

impl Focusable for MarkdownView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MarkdownView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("markdown-view")
            .track_focus(&self.focus_handle)
            .child(crate::keystroke_input::keystroke_only(&self.focus_handle))
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .size_full()
            .min_w(VIEW_MIN_WIDTH)
            .bg(rgb(theme::background()))
            .text_color(theme::text_primary())
            .children(self.render_header(cx))
            .when_some(self.notice.clone(), |column, notice| {
                column.child(
                    div()
                        .flex_none()
                        .px(pad_x())
                        .pt(gap_tight())
                        .text_size(meta().size)
                        .line_height(meta().line_height)
                        .text_color(theme::text_muted())
                        .child(notice),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .text_size(body().size)
                    .line_height(body().line_height)
                    .child(self.render_body()),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in `gpui::*`, whose `test` shadows the
    // built-in attribute `#[gpui::test]` expands to.
    use std::path::PathBuf;

    use gpui::{TestAppContext, WindowHandle};

    use super::{Content, MarkdownView};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("horizon-markdown-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open(cx: &mut TestAppContext, path: &std::path::Path) -> WindowHandle<MarkdownView> {
        cx.update(gpui_component::init);
        let path = path.to_path_buf();
        cx.add_window(|window, cx| MarkdownView::build(Some(path), false, window, cx))
    }

    fn loaded_title(view: &MarkdownView) -> Option<String> {
        match &view.content {
            Content::Loaded(doc) => doc.title.clone(),
            _ => None,
        }
    }

    #[gpui::test]
    fn a_link_opens_in_place_and_back_returns(cx: &mut TestAppContext) {
        let scratch = Scratch::new();
        let index = scratch.write(
            "index.md",
            "---\ntitle: Index\nwritten: 2026-11-08\n---\n\nSee [a](a.md).\n",
        );
        let page = scratch.write("a.md", "---\ntitle: Page A\n---\nbody\n");
        let window = open(cx, &index);
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                assert_eq!(loaded_title(view).as_deref(), Some("Index"));
                let Content::Loaded(doc) = &view.content else {
                    unreachable!()
                };
                assert_eq!(
                    doc.fields,
                    vec![("written".to_string(), "2026-11-08".to_string())]
                );
            })
            .unwrap();

        window
            .update(cx, |view, _, cx| view.follow_link("a.md", cx))
            .unwrap();
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                assert_eq!(view.path.as_deref(), Some(page.as_path()));
                assert_eq!(view.history, vec![index.clone()]);
                assert_eq!(loaded_title(view).as_deref(), Some("Page A"));
            })
            .unwrap();

        // The back key, through the view's own key handling.
        cx.simulate_keystrokes(window.into(), "backspace");
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                assert_eq!(view.path.as_deref(), Some(index.as_path()));
                assert!(view.history.is_empty());
                assert_eq!(loaded_title(view).as_deref(), Some("Index"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_link_that_is_not_markdown_stays_put_with_a_notice(cx: &mut TestAppContext) {
        let scratch = Scratch::new();
        let index = scratch.write("index.md", "# Index\n");
        let window = open(cx, &index);
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| view.follow_link("src/lib.rs", cx))
            .unwrap();
        window
            .read_with(cx, |view, _| {
                assert_eq!(view.path.as_deref(), Some(index.as_path()));
                assert!(view.history.is_empty());
                assert!(view
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.contains("src/lib.rs")));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_missing_file_is_a_pane_state_and_a_rewrite_is_picked_up(cx: &mut TestAppContext) {
        let scratch = Scratch::new();
        let path = scratch.0.join("later.md");
        let window = open(cx, &path);
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                assert!(
                    matches!(&view.content, Content::Failed(error) if error.contains("later.md"))
                );
            })
            .unwrap();

        scratch.write("later.md", "---\ntitle: Now\n---\n");
        window.update(cx, |view, _, cx| view.reload(cx)).unwrap();
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                assert_eq!(loaded_title(view).as_deref(), Some("Now"))
            })
            .unwrap();
    }
}
