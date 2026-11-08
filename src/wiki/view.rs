//! The wiki view: one page of the project wiki drawn as running text, its
//! comment threads highlighted in it and listed under it, and a composer
//! pinned at the bottom that opens a thread on the selected text or
//! continues the thread in focus.
//!
//! The layout follows the board's thread view (a body, cards under it, the
//! composer pinned under them); where threads sit relative to the page is
//! not settled (docs/project-wiki-design.md, "Still open before planning").
//!
//! Comments are written straight to the page's thread log; the daemon's
//! wiki flow notices them and answers through the thread's session. The
//! page file and the log are watched, so an answer or a page edit redraws.

use std::ops::Range;
use std::path::{Path, PathBuf};

use futures::channel::mpsc::{self, UnboundedSender};
use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::text::{RangeHighlight, TextView, TextViewState};
use gpui_component::{h_flex, v_flex};
use horizon_wiki::anchor::Anchor;
use horizon_wiki::annotation::{markdown_body, Agent, AgentType, Annotation};
use horizon_wiki::Store;

use super::model::{PlacedThread, WikiPage};
use crate::file_watch::{self, FileWatch};
use crate::markdown::document::{resolve_link, LinkTarget};
use crate::markdown::DocumentChanged;
use crate::theme;
use crate::ui::parts::{chip, key_chip, markdown_body as draw_markdown, running_text};
use crate::ui::spec::*;

const NO_PAGE: &str = "No wiki page";
const LOADING: &str = "Loading";
const THREADS: &str = "Comments";
const ORPHANS: &str = "No longer in the page";
const CHANGED: &str = "(the passage has changed since)";
const NO_SELECTION: &str = "Select text in the page, then press c to comment on it";
const COMMENT_ON: &str = "Comment on";
const REPLY_IN: &str = "Reply in the thread on";
const TO_SEND: &str = "to send, Esc to cancel";
const OWNER: &str = "Owner";
const AGENT: &str = "Agent";
const UNSUPPORTED_LINK: &str = "Not a wiki page";

/// The composer's row bounds, as the board thread's composer has them.
const COMPOSER_MIN_ROWS: usize = 2;
const COMPOSER_MAX_ROWS: usize = 8;

/// What the composer writes to when it is sent.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// A new thread on these logical characters.
    Selection(Range<usize>),
    /// The thread at this index of the page's threads.
    Thread(usize),
}

pub(crate) struct WikiView {
    path: Option<PathBuf>,
    page: Option<WikiPage>,
    error: Option<String>,
    /// Bumped on every load; the highlights are set once per load, after
    /// the text view's parse of that load's body has landed.
    generation: u64,
    highlighted: Option<u64>,
    text: Entity<TextViewState>,
    focused: Option<usize>,
    target: Option<Target>,
    composer: Entity<TextareaState>,
    notice: Option<SharedString>,
    owner: Option<uuid::Uuid>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    links: UnboundedSender<SharedString>,
    watch_files: bool,
    _watches: Vec<FileWatch>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DocumentChanged> for WikiView {}

impl WikiView {
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
        let text = cx.new(|cx| TextViewState::markdown("", cx).selectable(true));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let subscriptions = vec![
            cx.observe(&text, |view, _, cx| view.apply_highlights(cx)),
            cx.subscribe_in(
                &composer,
                window,
                |view, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        view.send(window, cx);
                    }
                },
            ),
        ];
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
            page: None,
            error: None,
            generation: 0,
            highlighted: None,
            text,
            focused: None,
            target: None,
            composer,
            notice: None,
            owner: None,
            scroll: ScrollHandle::new(),
            focus_handle,
            links,
            watch_files,
            _watches: Vec::new(),
            _tasks: vec![link_pump],
            _subscriptions: subscriptions,
        };
        if let Some(path) = path {
            view.show(path, cx);
        }
        view
    }

    /// Points the view at a page file from outside (the CLI).
    pub(crate) fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.path.as_deref() == Some(path.as_path()) {
            self.reload(cx);
        } else {
            self.show(path, cx);
        }
    }

    fn store(&self) -> Option<Store> {
        self.path.as_deref().and_then(Path::parent).map(Store::at)
    }

    fn page_name(path: &Path) -> Option<String> {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    }

    fn show(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.page = None;
        self.error = None;
        self.focused = None;
        self.target = None;
        self.notice = None;
        self.scroll.set_offset(Point::default());
        self.path = Some(path.clone());
        self.owner = path
            .parent()
            .and_then(Path::parent)
            .and_then(|root| horizon_wiki::thread::owner_id(root).ok());
        self.watch(cx);
        self.reload(cx);
        cx.emit(DocumentChanged(path));
    }

    /// Watches the page file and, once the page has an id, its thread log.
    fn watch(&mut self, cx: &mut Context<Self>) {
        if !self.watch_files {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let mut files = vec![path];
        if let (Some(store), Some(id)) = (self.store(), self.page.as_ref().and_then(|page| page.id))
        {
            let _ = std::fs::create_dir_all(store.threads_dir());
            files.push(store.thread_log_path(id));
        }
        let weak = cx.entity().downgrade();
        self._watches = files
            .into_iter()
            .filter_map(|file| {
                let weak = weak.clone();
                file_watch::watch(file, cx, move |cx| {
                    weak.update(cx, |view, cx| view.reload(cx)).ok();
                })
            })
            .collect();
    }

    /// Reads the page and its threads again, keeping what is drawn until
    /// the read lands.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(store)) = (self.path.clone(), self.store()) else {
            return;
        };
        let Some(name) = Self::page_name(&path) else {
            return;
        };
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { WikiPage::load(&store, &name) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.path.as_deref() != Some(path.as_path()) {
                    return;
                }
                match result {
                    Ok(page) => view.loaded(page, cx),
                    Err(error) => {
                        view.error = Some(error);
                        cx.notify();
                    }
                }
            });
        });
        self._tasks.push(task);
        self._tasks.truncate(8);
    }

    fn loaded(&mut self, page: WikiPage, cx: &mut Context<Self>) {
        let had_id = self.page.as_ref().and_then(|page| page.id);
        let body = page.document.body.clone();
        let threads = page.threads.len();
        self.error = None;
        self.page = Some(page);
        self.generation += 1;
        if self.focused.is_some_and(|index| index >= threads) {
            self.focused = None;
        }
        if had_id.is_none() && self.page.as_ref().is_some_and(|page| page.id.is_some()) {
            self.watch(cx);
        }
        self.text.update(cx, |text, cx| text.set_text(&body, cx));
        self.apply_highlights(cx);
        cx.notify();
    }

    /// Paints every anchored thread once the text view has parsed this
    /// load's body; the focused thread is painted stronger.
    fn apply_highlights(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.page.as_ref() else {
            return;
        };
        if self.highlighted == Some(self.generation) {
            return;
        }
        let rendered = self.text.read(cx).rendered_text();
        if rendered.source() != page.document.body {
            return;
        }
        let accent = theme::accent();
        let highlights: Vec<RangeHighlight> = page
            .threads
            .iter()
            .enumerate()
            .filter_map(|(index, placed)| {
                let range = rendered.range_for_source(placed.source.clone()?)?;
                let alpha = if Some(index) == self.focused {
                    0.35
                } else {
                    0.15
                };
                Some(RangeHighlight::new(range, accent.opacity(alpha)))
            })
            .collect();
        self.highlighted = Some(self.generation);
        let _ = self
            .text
            .update(cx, |text, cx| text.set_range_highlights(highlights, cx));
    }

    fn focus_thread(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.focused = Some(index);
        self.target = Some(Target::Thread(index));
        self.highlighted = None;
        self.apply_highlights(cx);
        if let Some(source) = self
            .page
            .as_ref()
            .and_then(|page| page.threads.get(index))
            .and_then(|placed| placed.source.clone())
        {
            let rendered = self.text.read(cx).rendered_text();
            if let Some(range) = rendered.range_for_source(source) {
                let _ = self
                    .text
                    .update(cx, |text, cx| text.reveal_range(range, cx));
            }
        }
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
        cx.notify();
    }

    /// Starts a comment on the page text currently selected.
    fn comment_on_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.text.read(cx).selected_source_range();
        let chars = source.and_then(|source| self.page.as_ref()?.selection(source));
        match chars {
            Some(chars) => {
                self.target = Some(Target::Selection(chars));
                self.notice = None;
                self.composer
                    .update(cx, |composer, cx| composer.focus(window, cx));
            }
            None => self.notice = Some(NO_SELECTION.into()),
        }
        cx.notify();
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.target = None;
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        let (Some(target), Some(store), Some(path), Some(owner)) = (
            self.target.clone(),
            self.store(),
            self.path.clone(),
            self.owner,
        ) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let Some(name) = Self::page_name(&path) else {
            return;
        };
        let creator = Agent::person(owner, "owner");
        let result = match &target {
            Target::Selection(chars) => store
                .ask(&name, chars.clone(), creator, markdown_body(text))
                .map(|_| ()),
            Target::Thread(index) => {
                let page = self.page.as_ref();
                match (
                    page.and_then(|page| page.id),
                    page.and_then(|page| page.threads.get(*index)),
                ) {
                    (Some(id), Some(placed)) => store
                        .reply(id, &placed.thread.last().id, creator, markdown_body(text))
                        .map(|_| ()),
                    _ => return,
                }
            }
        };
        match result {
            Ok(()) => {
                self.notice = None;
                self.target = match target {
                    Target::Selection(_) => None,
                    thread => Some(thread),
                };
                self.composer
                    .update(cx, |composer, cx| composer.set_value("", window, cx));
                self.reload(cx);
            }
            Err(error) => self.notice = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn follow_link(&mut self, target: &str, cx: &mut Context<Self>) {
        let Some(current) = self.path.clone() else {
            return;
        };
        match resolve_link(&current, target) {
            LinkTarget::Document(path) if path.parent() == current.parent() => {
                if path != current {
                    self.show(path, cx);
                }
            }
            LinkTarget::External(url) => cx.open_url(&url),
            _ => {
                self.notice = Some(format!("{UNSUPPORTED_LINK}: {target}").into());
                cx.notify();
            }
        }
    }

    fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let mut offset = self.scroll.offset();
        let max = self.scroll.max_offset().y;
        offset.y = (offset.y - delta).clamp(-max, px(0.));
        self.scroll.set_offset(offset);
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" && self.target.is_some() {
            self.cancel(window, cx);
            cx.stop_propagation();
            return;
        }
        if !self.focus_handle.is_focused(window)
            || keystroke.modifiers.control
            || keystroke.modifiers.alt
            || keystroke.modifiers.platform
        {
            return;
        }
        let line = body().line_height * 3.;
        let page = window.viewport_size().height * 0.8;
        let threads = self.page.as_ref().map_or(0, |page| page.threads.len());
        match keystroke.key.as_str() {
            "j" | "down" => self.scroll_by(line, cx),
            "k" | "up" => self.scroll_by(-line, cx),
            "space" | "pagedown" => self.scroll_by(page, cx),
            "pageup" => self.scroll_by(-page, cx),
            "c" => self.comment_on_selection(window, cx),
            "n" | "p" if threads > 0 => {
                let next = match (self.focused, keystroke.key.as_str()) {
                    (None, "n") => 0,
                    (None, _) => threads - 1,
                    (Some(index), "n") => (index + 1) % threads,
                    (Some(index), _) => (index + threads - 1) % threads,
                };
                self.focus_thread(next, window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_header(&self) -> Option<AnyElement> {
        let document = &self.page.as_ref()?.document;
        let heading = t1();
        let fields: Vec<_> = document
            .fields
            .iter()
            .filter(|(key, _)| key != "id")
            .collect();
        Some(
            v_flex()
                .flex_none()
                .w_full()
                .px(pad_x())
                .pt(gap_section())
                .gap(gap_tight())
                .when_some(document.title.clone(), |column, title| {
                    column.child(
                        div()
                            .text_size(heading.size)
                            .line_height(heading.line_height)
                            .font_weight(ANCHOR)
                            .child(title),
                    )
                })
                .when(!fields.is_empty(), |column| {
                    column.child(h_flex().flex_wrap().gap(gap_label()).children(
                        fields.into_iter().map(|(key, value)| {
                            chip(format!("{key} {value}"), theme::text_muted())
                        }),
                    ))
                })
                .into_any_element(),
        )
    }

    fn render_post(&self, annotation: &Annotation) -> impl IntoElement {
        let author = match annotation.creator.kind {
            AgentType::Person => OWNER,
            AgentType::Software => AGENT,
        };
        v_flex()
            .w_full()
            .gap(gap_label())
            .child(
                h_flex()
                    .gap(gap_label())
                    .text_size(meta().size)
                    .line_height(meta().line_height)
                    .child(div().font_weight(ANCHOR).child(author))
                    .child(
                        div()
                            .text_color(theme::text_muted())
                            .child(annotation.created.replace('T', " ").replace('Z', "")),
                    ),
            )
            .child(draw_markdown(
                ElementId::Name(format!("wiki-post:{}", annotation.id).into()),
                annotation.body.value.clone(),
                theme::text_primary(),
            ))
    }

    fn render_thread(
        &self,
        index: usize,
        placed: &PlacedThread,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.focused == Some(index);
        let posts: Vec<&Annotation> = std::iter::once(&placed.thread.root)
            .chain(&placed.thread.replies)
            .collect();
        v_flex()
            .id(("wiki-thread", index))
            .w_full()
            .p(pad_y())
            .gap(gap_tight())
            .rounded(RADIUS)
            .border_l(ACCENT_BAR)
            .border_color(if focused {
                theme::accent()
            } else {
                theme::border()
            })
            .when(focused, |card| {
                card.bg(theme::tint_over_background(theme::accent(), SELECTION_TINT))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |view, _, window, cx| view.focus_thread(index, window, cx)))
            .child(
                div()
                    .text_size(meta().size)
                    .line_height(meta().line_height)
                    .text_color(theme::text_muted())
                    .child(match placed.anchor {
                        Anchor::Moved { exact: false, .. } => {
                            format!("“{}” {CHANGED}", placed.quote())
                        }
                        _ => format!("“{}”", placed.quote()),
                    }),
            )
            .children(posts.into_iter().map(|post| self.render_post(post)))
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = |text: SharedString, color: Hsla| {
            div()
                .px(pad_x())
                .pt(gap_section())
                .text_size(meta().size)
                .line_height(meta().line_height)
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        if self.path.is_none() {
            return muted(NO_PAGE.into(), theme::text_muted());
        }
        if let Some(error) = self.error.clone() {
            return muted(error.into(), theme::danger());
        }
        let Some(page) = self.page.as_ref() else {
            return muted(LOADING.into(), theme::text_muted());
        };
        let links = self.links.clone();
        let anchored: Vec<AnyElement> = page
            .threads
            .iter()
            .enumerate()
            .filter(|(_, placed)| placed.source.is_some())
            .map(|(index, placed)| self.render_thread(index, placed, cx))
            .collect();
        let orphans: Vec<AnyElement> = page
            .orphans()
            .map(|(index, placed)| self.render_thread(index, placed, cx))
            .collect();
        let section = |title: &'static str| {
            div()
                .pt(gap_section())
                .text_size(meta().size)
                .line_height(meta().line_height)
                .font_weight(ANCHOR)
                .text_color(theme::text_muted())
                .child(title)
        };
        v_flex()
            .id("wiki-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(pad_x())
            .pt(gap_section())
            .pb(gap_composer())
            .gap(gap_unit())
            .child(running_text(
                TextView::new(&self.text).on_link_click(move |target, _, _, _| {
                    let _ = links.unbounded_send(target.clone());
                }),
                theme::text_primary(),
            ))
            .when(!anchored.is_empty(), |column| {
                column.child(section(THREADS)).children(anchored)
            })
            .when(!orphans.is_empty(), |column| {
                column.child(section(ORPHANS)).children(orphans)
            })
            .into_any_element()
    }

    fn render_composer(&self) -> Option<AnyElement> {
        let page = self.page.as_ref()?;
        let label = match self.target.as_ref()? {
            Target::Selection(chars) => format!("{COMMENT_ON} “{}”", page.quote_of(chars.clone())),
            Target::Thread(index) => format!(
                "{REPLY_IN} “{}”",
                page.threads
                    .get(*index)
                    .map(PlacedThread::quote)
                    .unwrap_or_default()
            ),
        };
        Some(
            v_flex()
                .w_full()
                .flex_none()
                .px(pad_x())
                .py(pad_y())
                .gap(gap_label())
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .text_size(meta().size)
                        .line_height(meta().line_height)
                        .text_color(theme::text_muted())
                        .child(label),
                )
                .child(
                    div()
                        .w_full()
                        .text_size(body().size)
                        .child(Textarea::new(&self.composer).appearance(false)),
                )
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_end()
                        .gap(gap_label())
                        .child(key_chip("Enter"))
                        .child(
                            div()
                                .text_size(micro().size)
                                .line_height(micro().line_height)
                                .text_color(theme::text_muted())
                                .child(TO_SEND),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Focusable for WikiView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WikiView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("wiki-view")
            .track_focus(&self.focus_handle)
            .child(crate::keystroke_input::keystroke_only(&self.focus_handle))
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && view.target.is_some() {
                    view.cancel(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .size_full()
            .min_w(VIEW_MIN_WIDTH)
            .bg(rgb(theme::background()))
            .text_color(theme::text_primary())
            .children(self.render_header())
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
                    .child(self.render_body(cx)),
            )
            .children(self.render_composer())
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in `gpui::*`, whose `test` shadows the
    // attribute `#[gpui::test]` expands to.
    use gpui::{TestAppContext, WindowHandle};
    use horizon_wiki::Store;

    use super::{Target, WikiView};

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open(cx: &mut TestAppContext) -> (Scratch, Store, WindowHandle<WikiView>) {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("horizon-wiki-view-{}", uuid::Uuid::new_v4())),
        );
        let store = Store::at(scratch.0.join("project"));
        store
            .write_page(
                "sandbox",
                "---\ntitle: Sandbox\n---\nThe helper runs nono first.\n",
            )
            .unwrap();
        let path = store.page_path("sandbox").unwrap();
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| WikiView::build(Some(path), false, window, cx));
        cx.run_until_parked();
        (scratch, store, window)
    }

    #[gpui::test]
    fn a_comment_on_a_selection_opens_a_thread_and_a_reply_continues_it(cx: &mut TestAppContext) {
        let (_scratch, store, window) = open(cx);
        window
            .read_with(cx, |view, _| {
                let page = view.page.as_ref().expect("loaded");
                assert_eq!(page.document.title.as_deref(), Some("Sandbox"));
                assert!(view.owner.is_some());
            })
            .unwrap();

        window
            .update(cx, |view, window, cx| {
                view.target = Some(Target::Selection(4..10));
                view.composer.update(cx, |composer, cx| {
                    composer.set_value("Which helper?", window, cx)
                });
                view.send(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        let page_id = window
            .read_with(cx, |view, _| {
                let page = view.page.as_ref().unwrap();
                assert_eq!(page.threads.len(), 1);
                assert_eq!(page.threads[0].quote(), "helper");
                assert!(view.target.is_none());
                page.id.unwrap()
            })
            .unwrap();

        window
            .update(cx, |view, window, cx| {
                view.focus_thread(0, window, cx);
                view.composer.update(cx, |composer, cx| {
                    composer.set_value("And why?", window, cx)
                });
                view.send(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        let threads = store.threads(page_id).unwrap();
        assert_eq!(threads[0].replies.len(), 1);
        assert_eq!(threads[0].replies[0].body.value, "And why?");
    }
}
