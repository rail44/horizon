//! The wiki view: one page of the project wiki drawn as running text,
//! with each comment thread highlighted where it points and read in the
//! page, right after the block (paragraph, list, table…) it points into, and
//! a composer pinned at the bottom that opens a thread on the selected text
//! or continues the thread in focus. Threads whose passage left the page are
//! listed after it.
//!
//! The page is drawn one top-level block per text view, so threads can sit
//! between blocks (docs/project-wiki-design.md, "Comments and the wiki
//! view"); positions convert through each block's offset in the body.
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
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::text::{RangeHighlight, TextView, TextViewState};
use gpui_component::{h_flex, v_flex, Sizable as _};
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
const ORPHANS: &str = "No longer in the page";
const CHANGED: &str = "(the passage has changed since)";
const NO_SELECTION: &str = "Select text in the page to comment on it";
const COMMENT_ON: &str = "Comment on";
const REPLY_IN: &str = "Reply in the thread on";
const TO_SEND: &str = "to send, Esc to cancel";
const OWNER: &str = "Owner";
const AGENT: &str = "Agent";
const REPLYING: &str = "Agent is replying…";
const UNSUPPORTED_LINK: &str = "Not a wiki page";
const BACK: &str = "Back";
const COMMENT: &str = "Comment";
/// How much of the selection the comment bar quotes.
const SELECTION_QUOTE_CHARS: usize = 80;

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
    /// Bumped on every load; each block's highlights are set once per
    /// load, after that block's text view has parsed the load's text.
    generation: u64,
    /// Per block, the generation its highlights were last set for.
    highlighted: Vec<Option<u64>>,
    /// One text view per top-level block of the page body.
    blocks: Vec<Entity<TextViewState>>,
    _block_subscriptions: Vec<Subscription>,
    focused: Option<usize>,
    target: Option<Target>,
    /// The page text selected now, in logical characters, followed from
    /// the text view so the comment button can act on it.
    selection: Option<Range<usize>>,
    composer: Entity<TextareaState>,
    notice: Option<SharedString>,
    /// The pages left by following links, most recent last, as the
    /// Markdown view keeps them.
    history: Vec<PathBuf>,
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
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let subscriptions = vec![cx.subscribe_in(
            &composer,
            window,
            |view, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.send(window, cx);
                }
            },
        )];
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
            highlighted: Vec::new(),
            blocks: Vec::new(),
            _block_subscriptions: Vec::new(),
            focused: None,
            target: None,
            selection: None,
            composer,
            notice: None,
            history: Vec::new(),
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
            files.push(store.replying_path());
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
        let texts: Vec<String> = page
            .blocks
            .iter()
            .map(|block| page.document.body[block.clone()].to_string())
            .collect();
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
        self.sync_blocks(texts, cx);
        self.apply_highlights(cx);
        cx.notify();
    }

    /// One text view per block: existing views take the new text (a view
    /// whose text is unchanged keeps its parse), missing ones are made and
    /// extra ones dropped.
    fn sync_blocks(&mut self, texts: Vec<String>, cx: &mut Context<Self>) {
        self.blocks.truncate(texts.len());
        self._block_subscriptions.truncate(texts.len());
        for (index, text) in texts.iter().enumerate() {
            match self.blocks.get(index) {
                Some(block) => block.update(cx, |block, cx| block.set_text(text, cx)),
                None => {
                    let block = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
                    self._block_subscriptions
                        .push(cx.observe(&block, |view, _, cx| {
                            view.apply_highlights(cx);
                            view.follow_selection(cx);
                        }));
                    self.blocks.push(block);
                }
            }
        }
        self.highlighted = vec![None; self.blocks.len()];
    }

    /// Paints every anchored thread, block by block, once each block's
    /// text view has parsed this load's text; the focused thread is painted
    /// stronger. A thread spanning blocks paints its part in each.
    fn apply_highlights(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.page.as_ref() else {
            return;
        };
        let accent = theme::accent();
        for (index, block) in self.blocks.clone().into_iter().enumerate() {
            if self.highlighted.get(index).copied().flatten() == Some(self.generation) {
                continue;
            }
            let Some(range) = page.blocks.get(index).cloned() else {
                continue;
            };
            let rendered = block.read(cx).rendered_text();
            if rendered.source() != &page.document.body[range.clone()] {
                continue;
            }
            let highlights: Vec<RangeHighlight> = page
                .threads
                .iter()
                .enumerate()
                .filter_map(|(thread, placed)| {
                    let source = placed.source.clone()?;
                    let start = source.start.max(range.start);
                    let end = source.end.min(range.end);
                    if start >= end {
                        return None;
                    }
                    let local =
                        rendered.range_for_source(start - range.start..end - range.start)?;
                    let alpha = if Some(thread) == self.focused {
                        0.35
                    } else {
                        0.15
                    };
                    Some(RangeHighlight::new(local, accent.opacity(alpha)))
                })
                .collect();
            if let Some(slot) = self.highlighted.get_mut(index) {
                *slot = Some(self.generation);
            }
            let _ = block.update(cx, |block, cx| block.set_range_highlights(highlights, cx));
        }
    }

    /// The body bytes selected now: from the first selecting block's start
    /// to the last one's end.
    fn selected_source(&self, cx: &App) -> Option<Range<usize>> {
        let page = self.page.as_ref()?;
        let mut selected: Option<Range<usize>> = None;
        for (block, range) in self.blocks.iter().zip(&page.blocks) {
            let Some(local) = block.read(cx).selected_source_range() else {
                continue;
            };
            let start = range.start + local.start;
            let end = range.start + local.end;
            selected = Some(match selected {
                Some(earlier) => earlier.start.min(start)..earlier.end.max(end),
                None => start..end,
            });
        }
        selected
    }

    fn focus_thread(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.focused = Some(index);
        self.target = Some(Target::Thread(index));
        self.highlighted = vec![None; self.blocks.len()];
        self.apply_highlights(cx);
        let target = self.page.as_ref().and_then(|page| {
            let placed = page.threads.get(index)?;
            let block = placed.block?;
            let range = page.blocks.get(block)?;
            let source = placed.source.clone()?;
            let start = source.start.max(range.start) - range.start;
            Some((
                block,
                start..source.end.min(range.end).max(range.start) - range.start,
            ))
        });
        if let Some((block, local)) = target {
            if let Some(state) = self.blocks.get(block).cloned() {
                let rendered = state.read(cx).rendered_text();
                if let Some(range) = rendered.range_for_source(local) {
                    let _ = state.update(cx, |state, cx| state.reveal_range(range, cx));
                }
            }
        }
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
        cx.notify();
    }

    /// Starts a comment on the page text currently selected.
    /// Keeps [`Self::selection`] in step with the text view's selection.
    fn follow_selection(&mut self, cx: &mut Context<Self>) {
        let source = self.selected_source(cx);
        let chars = source.and_then(|source| self.page.as_ref()?.selection(source));
        if chars != self.selection {
            self.selection = chars;
            cx.notify();
        }
    }

    /// Starts a comment on the selected page text. Reads the stored
    /// selection: a click on the comment button may clear the text view's
    /// own before this runs.
    fn comment_on_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live = self
            .selected_source(cx)
            .and_then(|source| self.page.as_ref()?.selection(source));
        let chars = live.or_else(|| self.selection.clone());
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
                // The composer of a new thread closes; keys go back to the
                // view rather than to an input no longer drawn.
                if self.target.is_none() {
                    window.focus(&self.focus_handle, cx);
                }
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
                    self.history.push(current);
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

    fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.history.pop() {
            self.show(previous, cx);
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
        // Selecting page text focuses the text view inside this view, so
        // keys count while focus is anywhere in it except the composer.
        if !self.focus_handle.contains_focused(window, cx)
            || self
                .composer
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
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
            "backspace" | "h" | "left" => self.back(cx),
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

    fn render_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let document = &self.page.as_ref()?.document;
        let can_go_back = !self.history.is_empty();
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
                .when(can_go_back, |column| {
                    column.child(
                        h_flex().child(
                            Button::new("wiki-back")
                                .ghost()
                                .xsmall()
                                .label(BACK)
                                .on_click(cx.listener(|view, _, _, cx| view.back(cx))),
                        ),
                    )
                })
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
        let replying = self
            .page
            .as_ref()
            .is_some_and(|page| placed.replying(&page.replying));
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
            .when(replying, |card| {
                card.child(
                    div()
                        .text_size(meta().size)
                        .line_height(meta().line_height)
                        .text_color(theme::text_muted())
                        .child(REPLYING),
                )
            })
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
        let mut flow: Vec<AnyElement> = Vec::new();
        for (block_index, block) in self.blocks.iter().enumerate() {
            let links = self.links.clone();
            let scroll = self.scroll.clone();
            flow.push(
                running_text(
                    TextView::new(block)
                        .on_link_click(move |target, _, _, _| {
                            let _ = links.unbounded_send(target.clone());
                        })
                        // The body scrolls in this view's own container, which
                        // a text view's reveal does not move by itself.
                        .on_reveal(move |line, window, _| {
                            reveal_in(&scroll, line);
                            window.refresh();
                        }),
                    theme::text_primary(),
                )
                .into_any_element(),
            );
            for (index, placed) in page.threads.iter().enumerate() {
                if placed.block == Some(block_index) {
                    flow.push(self.render_thread(index, placed, cx));
                }
            }
        }
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
            .gap(gap_tight())
            .children(flow)
            .when(!orphans.is_empty(), |column| {
                column.child(section(ORPHANS)).children(orphans)
            })
            .into_any_element()
    }

    /// While page text is selected and nothing is being written: the
    /// selection's quote and a button that starts a comment on it.
    fn render_selection_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.target.is_some() {
            return None;
        }
        let page = self.page.as_ref()?;
        let quote = page.quote_of(self.selection.clone()?);
        let quote: String = if quote.chars().count() > SELECTION_QUOTE_CHARS {
            let head: String = quote.chars().take(SELECTION_QUOTE_CHARS).collect();
            format!("{head}…")
        } else {
            quote
        };
        Some(
            h_flex()
                .w_full()
                .flex_none()
                .px(pad_x())
                .py(pad_y())
                .gap(gap_label())
                .items_center()
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(meta().size)
                        .line_height(meta().line_height)
                        .text_color(theme::text_muted())
                        .child(format!("“{}”", quote.replace('\n', " "))),
                )
                .child(key_chip("c"))
                .child(
                    Button::new("wiki-comment")
                        .small()
                        .label(COMMENT)
                        // On press, not release: the press may clear the
                        // text view's selection, and the stored one is
                        // what this acts on.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, window, cx| {
                                cx.stop_propagation();
                                view.comment_on_selection(window, cx);
                            }),
                        ),
                )
                .into_any_element(),
        )
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

/// Scrolls `scroll` so `line` (window coordinates) is inside its
/// viewport, with some room around it.
fn reveal_in(scroll: &ScrollHandle, line: Bounds<Pixels>) {
    let viewport = scroll.bounds();
    let room = line.size.height * 2.;
    let mut offset = scroll.offset();
    if line.top() < viewport.top() {
        offset.y += viewport.top() - line.top() + room;
    } else if line.bottom() > viewport.bottom() {
        offset.y -= line.bottom() - viewport.bottom() + room;
    } else {
        return;
    }
    let max = scroll.max_offset().y;
    offset.y = offset.y.clamp(-max, px(0.));
    scroll.set_offset(offset);
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
                    .child(self.render_body(cx)),
            )
            .children(self.render_selection_bar(cx))
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

    /// After a new thread is sent, its composer closes and keys reach the
    /// view again, so the next `c` is handled.
    #[gpui::test]
    fn keys_reach_the_view_again_after_a_comment_is_sent(cx: &mut TestAppContext) {
        let (_scratch, _store, window) = open(cx);
        window
            .update(cx, |view, window, cx| {
                view.selection = Some(4..10);
                view.comment_on_selection(window, cx);
                assert_eq!(view.target, Some(Target::Selection(4..10)));
                view.composer.update(cx, |composer, cx| {
                    composer.set_value("Which helper?", window, cx)
                });
                view.send(window, cx);
                assert!(view.target.is_none());
                assert!(view.focus_handle.is_focused(window));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, _| {
                view.selection = Some(15..19);
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "c");
        window
            .read_with(cx, |view, _| {
                assert_eq!(view.target, Some(Target::Selection(15..19)));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_followed_link_can_be_gone_back_from(cx: &mut TestAppContext) {
        let (_scratch, store, window) = open(cx);
        store
            .write_page("other", "---\ntitle: Other\n---\nSee [back](sandbox.md).\n")
            .unwrap();
        let sandbox = store.page_path("sandbox").unwrap();
        window
            .update(cx, |view, _, cx| view.follow_link("other.md", cx))
            .unwrap();
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                assert_eq!(view.history, vec![sandbox.clone()]);
                let title = view
                    .page
                    .as_ref()
                    .and_then(|page| page.document.title.clone());
                assert_eq!(title.as_deref(), Some("Other"));
            })
            .unwrap();
        // The back key, through the view's own key handling.
        cx.simulate_keystrokes(window.into(), "backspace");
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                assert_eq!(view.path.as_deref(), Some(sandbox.as_path()));
                assert!(view.history.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_comment_on_a_selection_opens_a_thread_and_a_reply_continues_it(cx: &mut TestAppContext) {
        let (_scratch, store, window) = open(cx);
        window
            .read_with(cx, |view, _| {
                let page = view.page.as_ref().expect("loaded");
                assert_eq!(page.document.title.as_deref(), Some("Sandbox"));
                assert!(view.owner.is_some());
                // One text view per top-level block.
                assert_eq!(view.blocks.len(), page.blocks.len());
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
