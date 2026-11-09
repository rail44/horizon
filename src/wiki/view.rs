//! The wiki view: one page of the project wiki drawn as running text, with
//! each comment thread highlighted where it points and read beside it.
//!
//! On a pane wide enough for a margin, threads sit in a column at the
//! right, each at the height of the top-level block (paragraph, list,
//! table…) its passage starts in, pushed down only by the threads above it;
//! the text never moves for them. On a narrower pane they sit in the page,
//! right after that block. Either way a thread starts closed, as one line
//! (its quote, how many comments, whether an agent is replying), and opens
//! on a click; an open thread holds its composer. Threads whose passage
//! left the page are listed after it (docs/project-wiki-design.md, "After
//! first use").
//!
//! The page is drawn one top-level block per text view so that a block's
//! height can be measured (a `canvas` laid over it) and threads placed
//! against it; positions convert through each block's offset in the body.
//! Posts are drawn as the agent pane draws a conversation: the owner's
//! indented on a grey tint, an agent's in a bordered card.
//!
//! Comments are written straight to the page's thread log; the daemon's
//! wiki flow notices them and answers through the thread's session, and
//! lists the comments it is answering in `threads/replying.json`. The page
//! file, the log and that list are watched, so an answer or an edit redraws.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

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
const CHANGED: &str = "changed since";
const NO_SELECTION: &str = "Select text in the page to comment on it";
const COMMENT_ON: &str = "Comment on";
const TO_SEND: &str = "to send, Esc to cancel";
const REPLYING: &str = "Replying";
const UNSUPPORTED_LINK: &str = "Not a wiki page";
const BACK: &str = "Back";
const COMMENT: &str = "Comment";
/// How much of the selection the comment bar quotes.
const SELECTION_QUOTE_CHARS: usize = 80;
/// The composer's row bounds, as the board thread's composer has them.
const COMPOSER_MIN_ROWS: usize = 2;
const COMPOSER_MAX_ROWS: usize = 8;
/// One cycle of the replying dots, as the agent pane's thinking dots.
const DOTS_CYCLE_MS: u64 = 1200;
/// The measure key of a comment being started.
const NEW_COMMENT: &str = "new";

/// The margin threads sit in.
fn margin_width() -> Pixels {
    scaled(320.0)
}

/// The narrowest the text may get beside the margin; below it, threads
/// move into the page.
fn text_min_width() -> Pixels {
    scaled(360.0)
}

/// How far the owner's posts are indented, as in the agent pane.
fn owner_indent() -> Pixels {
    scaled(24.0)
}

/// A thread's height before it has been measured.
fn unmeasured_height() -> Pixels {
    scaled(40.0)
}

/// What the composer writes to when it is sent.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// A new thread on these logical characters.
    Selection(Range<usize>),
    /// The thread whose root annotation has this id.
    Thread(String),
}

/// Where things were laid out in the last frame, measured by canvases laid
/// over them and read by the next render.
#[derive(Debug, Default)]
struct Measure {
    /// The row holding the text and the margin, in window coordinates.
    row: Option<Bounds<Pixels>>,
    /// Each block's top, in window coordinates.
    blocks: HashMap<usize, Pixels>,
    /// Each margin item's height, by thread root id (or [`NEW_COMMENT`]).
    items: HashMap<String, Pixels>,
}

#[derive(Debug, Clone)]
enum Probe {
    Row,
    Block(usize),
    Item(String),
}

impl Measure {
    /// Records `bounds` for `probe`; `true` when it moved enough to redraw.
    fn record(&mut self, probe: &Probe, bounds: Bounds<Pixels>) -> bool {
        let moved = |old: Pixels, new: Pixels| (old - new).abs() > px(0.5);
        match probe {
            Probe::Row => {
                let changed = self.row.is_none_or(|row| {
                    moved(row.size.width, bounds.size.width) || moved(row.top(), bounds.top())
                });
                self.row = Some(bounds);
                changed
            }
            Probe::Block(index) => self
                .blocks
                .insert(*index, bounds.top())
                .is_none_or(|old| moved(old, bounds.top())),
            Probe::Item(key) => self
                .items
                .insert(key.clone(), bounds.size.height)
                .is_none_or(|old| moved(old, bounds.size.height)),
        }
    }

    /// Whether the last frame's row had room for the margin.
    fn wide(&self) -> bool {
        self.row
            .is_some_and(|row| row.size.width >= text_min_width() + margin_width() + gap_section())
    }

    /// A block's top relative to the row.
    fn block_top(&self, index: usize) -> Pixels {
        match (self.row, self.blocks.get(&index)) {
            (Some(row), Some(top)) => (*top - row.top()).max(px(0.)),
            _ => px(0.),
        }
    }
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
    /// The thread in focus, by root id: open, and painted stronger.
    focused: Option<String>,
    /// Threads shown open, by root id.
    expanded: HashSet<String>,
    target: Option<Target>,
    /// The page text selected now, in logical characters, followed from
    /// the text views so the comment button can act on it.
    selection: Option<Range<usize>>,
    composer: Entity<TextareaState>,
    notice: Option<SharedString>,
    /// The pages left by following links, most recent last, as the
    /// Markdown view keeps them.
    history: Vec<PathBuf>,
    owner: Option<uuid::Uuid>,
    measure: Rc<RefCell<Measure>>,
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
            expanded: HashSet::new(),
            target: None,
            selection: None,
            composer,
            notice: None,
            history: Vec::new(),
            owner: None,
            measure: Rc::default(),
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
        self.expanded.clear();
        self.target = None;
        self.notice = None;
        *self.measure.borrow_mut() = Measure::default();
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

    /// Watches the page file and, once the page has an id, its thread log
    /// and the list of comments being answered.
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
        let roots: HashSet<&String> = page
            .threads
            .iter()
            .map(|placed| &placed.thread.root.id)
            .collect();
        if self
            .focused
            .as_ref()
            .is_some_and(|root| !roots.contains(root))
        {
            self.focused = None;
        }
        self.expanded.retain(|root| roots.contains(root));
        self.error = None;
        let has_id = page.id.is_some();
        self.page = Some(page);
        self.generation += 1;
        if had_id.is_none() && has_id {
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
                .filter_map(|placed| {
                    let source = placed.source.clone()?;
                    let start = source.start.max(range.start);
                    let end = source.end.min(range.end);
                    if start >= end {
                        return None;
                    }
                    let local =
                        rendered.range_for_source(start - range.start..end - range.start)?;
                    let focused = self.focused.as_ref() == Some(&placed.thread.root.id);
                    let alpha = if focused { 0.35 } else { 0.15 };
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

    fn placed(&self, root: &str) -> Option<&PlacedThread> {
        self.page
            .as_ref()?
            .threads
            .iter()
            .find(|placed| placed.thread.root.id == root)
    }

    /// Opens the thread `root`, puts it in focus, scrolls its passage into
    /// view and points the composer at it.
    fn focus_thread(&mut self, root: String, window: &mut Window, cx: &mut Context<Self>) {
        self.expanded.insert(root.clone());
        self.focused = Some(root.clone());
        self.target = Some(Target::Thread(root.clone()));
        self.highlighted = vec![None; self.blocks.len()];
        self.apply_highlights(cx);
        let target = self.page.as_ref().and_then(|page| {
            let placed = self.placed(&root)?;
            let block = placed.block?;
            let range = page.blocks.get(block)?;
            let source = placed.source.clone()?;
            let start = source.start.max(range.start) - range.start;
            let end = source.end.min(range.end).max(range.start) - range.start;
            Some((block, start..end))
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

    /// A click on a thread's head: opens it in focus, or closes it.
    fn toggle_thread(&mut self, root: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.expanded.contains(&root) {
            self.expanded.remove(&root);
            if self.focused.as_ref() == Some(&root) {
                self.focused = None;
                self.highlighted = vec![None; self.blocks.len()];
                self.apply_highlights(cx);
            }
            if self.target == Some(Target::Thread(root)) {
                self.target = None;
                window.focus(&self.focus_handle, cx);
            }
            cx.notify();
        } else {
            self.focus_thread(root, window, cx);
        }
    }

    /// Keeps [`Self::selection`] in step with the text views' selection.
    fn follow_selection(&mut self, cx: &mut Context<Self>) {
        let source = self.selected_source(cx);
        let chars = source.and_then(|source| self.page.as_ref()?.selection(source));
        if chars != self.selection {
            self.selection = chars;
            cx.notify();
        }
    }

    /// Starts a comment on the selected page text. Falls back to the stored
    /// selection: a click on the comment button may clear the text views'
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
                .map(|(_, root)| Some(root.id)),
            Target::Thread(root) => {
                let page_id = self.page.as_ref().and_then(|page| page.id);
                match (page_id, self.placed(root)) {
                    (Some(id), Some(placed)) => store
                        .reply(id, &placed.thread.last().id, creator, markdown_body(text))
                        .map(|_| None),
                    _ => return,
                }
            }
        };
        match result {
            Ok(new_root) => {
                self.notice = None;
                self.composer
                    .update(cx, |composer, cx| composer.set_value("", window, cx));
                // A new thread shows open; its composer closes, and keys go
                // back to the view rather than to an input no longer drawn.
                if let Some(root) = new_root {
                    self.expanded.insert(root.clone());
                    self.focused = Some(root);
                    self.target = None;
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
        // Selecting page text focuses a text view inside this view, so keys
        // count while focus is anywhere in it except the composer.
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
        let roots: Vec<String> = self.page.as_ref().map_or_else(Vec::new, |page| {
            page.threads
                .iter()
                .map(|placed| placed.thread.root.id.clone())
                .collect()
        });
        match keystroke.key.as_str() {
            "j" | "down" => self.scroll_by(line, cx),
            "k" | "up" => self.scroll_by(-line, cx),
            "space" | "pagedown" => self.scroll_by(page, cx),
            "pageup" => self.scroll_by(-page, cx),
            "backspace" | "h" | "left" => self.back(cx),
            "c" => self.comment_on_selection(window, cx),
            "n" | "p" if !roots.is_empty() => {
                let count = roots.len();
                let at = self
                    .focused
                    .as_ref()
                    .and_then(|root| roots.iter().position(|known| known == root));
                let next = match (at, keystroke.key.as_str()) {
                    (None, "n") => 0,
                    (None, _) => count - 1,
                    (Some(index), "n") => (index + 1) % count,
                    (Some(index), _) => (index + count - 1) % count,
                };
                self.focus_thread(roots[next].clone(), window, cx);
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

    /// One post, drawn as the agent pane draws a conversation: the owner's
    /// indented on a grey tint, an agent's in a bordered card.
    fn render_post(&self, annotation: &Annotation) -> impl IntoElement {
        let when = text_line(
            micro(),
            theme::text_muted(),
            annotation.created.replace('T', " ").replace('Z', ""),
        );
        let text = draw_markdown(
            ElementId::Name(format!("wiki-post:{}", annotation.id).into()),
            annotation.body.value.clone(),
            theme::text_primary(),
        );
        let post = v_flex()
            .px(pad_x())
            .py(gap_tight())
            .gap(gap_label())
            .rounded(RADIUS)
            .child(div().w_full().child(text))
            .child(when);
        match annotation.creator.kind {
            AgentType::Person => post.ml(owner_indent()).bg(theme::tint_over_background(
                theme::text_primary(),
                BAND_TINT,
            )),
            AgentType::Software => post.border_1().border_color(theme::border()),
        }
    }

    /// An agent writing a reply: a bordered card with the accent bar down
    /// its edge, as a running turn has it in the agent pane.
    fn render_replying(&self, id: SharedString) -> impl IntoElement {
        v_flex()
            .relative()
            .px(pad_x())
            .py(gap_tight())
            .rounded(RADIUS)
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(ACCENT_BAR)
                    .bg(theme::accent()),
            )
            .child(replying_dots(id))
    }

    /// The composer, inside the thread or new comment it writes to.
    fn render_composer(&self) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap(gap_label())
            .pt(gap_tight())
            .child(
                div()
                    .w_full()
                    .px(pad_x())
                    .py(gap_label())
                    .rounded(RADIUS)
                    .border_1()
                    .border_color(theme::border())
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
                    .child(text_line(micro(), theme::text_muted(), TO_SEND)),
            )
    }

    /// A thread: its head line (the quote, how many comments, whether an
    /// agent is replying), and when open its posts and composer.
    fn render_thread(&self, placed: &PlacedThread, cx: &mut Context<Self>) -> AnyElement {
        let root = placed.thread.root.id.clone();
        let open = self.expanded.contains(&root);
        let focused = self.focused.as_ref() == Some(&root);
        let replying = self
            .page
            .as_ref()
            .is_some_and(|page| placed.replying(&page.replying));
        let posts: Vec<&Annotation> = std::iter::once(&placed.thread.root)
            .chain(&placed.thread.replies)
            .collect();
        let count = posts.len();
        let changed = matches!(placed.anchor, Anchor::Moved { exact: false, .. });
        let quote = one_line(&placed.quote());
        let head = h_flex()
            .id(SharedString::from(format!("wiki-thread-head:{root}")))
            .w_full()
            .gap(gap_label())
            .items_center()
            .cursor_pointer()
            .text_size(meta().size)
            .line_height(meta().line_height)
            .text_color(theme::text_muted())
            .on_click(cx.listener({
                let root = root.clone();
                move |view, _, window, cx| view.toggle_thread(root.clone(), window, cx)
            }))
            .child(div().flex_none().child(if open { "▾" } else { "▸" }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("“{quote}”")),
            )
            .child(div().flex_none().child(if count == 1 {
                "1 comment".to_string()
            } else {
                format!("{count} comments")
            }))
            .when(changed, |head| {
                head.child(chip(CHANGED, theme::text_muted()))
            })
            .when(replying && !open, |head| {
                head.child(chip(REPLYING, theme::accent()))
            });
        let editing = self.target == Some(Target::Thread(root.clone()));
        v_flex()
            .w_full()
            .p(gap_tight())
            .gap(gap_tight())
            .rounded(RADIUS)
            .when(focused, |card| {
                card.bg(theme::tint_over_background(theme::accent(), SELECTION_TINT))
            })
            .child(head)
            .when(open, |card| {
                card.children(posts.into_iter().map(|post| self.render_post(post)))
                    .when(replying, |card| {
                        card.child(self.render_replying(format!("wiki-replying:{root}").into()))
                    })
                    .when(editing, |card| card.child(self.render_composer()))
            })
            .into_any_element()
    }

    /// A comment being started on `chars`: what it quotes and the composer.
    fn render_new_comment(&self, page: &WikiPage, chars: Range<usize>) -> AnyElement {
        v_flex()
            .w_full()
            .p(gap_tight())
            .gap(gap_tight())
            .rounded(RADIUS)
            .bg(theme::tint_over_background(theme::accent(), SELECTION_TINT))
            .child(
                div()
                    .w_full()
                    .truncate()
                    .text_size(meta().size)
                    .line_height(meta().line_height)
                    .text_color(theme::text_muted())
                    .child(format!(
                        "{COMMENT_ON} “{}”",
                        one_line(&page.quote_of(chars))
                    )),
            )
            .child(self.render_composer())
            .into_any_element()
    }

    /// The page's text and its threads: in a margin beside the text when
    /// the last frame had room for one, in the text after their blocks
    /// otherwise.
    fn render_page(&self, page: &WikiPage, cx: &mut Context<Self>) -> AnyElement {
        let measure = self.measure.borrow();
        let wide = measure.wide();
        let weak = cx.weak_entity();
        let new_comment = match &self.target {
            Some(Target::Selection(chars)) => Some((page.block_of(chars.clone()), chars.clone())),
            _ => None,
        };

        let mut text = v_flex().flex_1().min_w_0().gap(gap_tight());
        for (index, block) in self.blocks.iter().enumerate() {
            let links = self.links.clone();
            let scroll = self.scroll.clone();
            text = text.child(
                div()
                    .relative()
                    .w_full()
                    .child(running_text(
                        TextView::new(block)
                            .on_link_click(move |target, _, _, _| {
                                let _ = links.unbounded_send(target.clone());
                            })
                            // The body scrolls in this view's own container,
                            // which a text view's reveal does not move.
                            .on_reveal(move |line, window, _| {
                                reveal_in(&scroll, line);
                                window.refresh();
                            }),
                        theme::text_primary(),
                    ))
                    .child(probe(&self.measure, &weak, Probe::Block(index))),
            );
            if !wide {
                for placed in &page.threads {
                    if placed.block == Some(index) {
                        text = text.child(self.render_thread(placed, cx));
                    }
                }
                if let Some((Some(block), chars)) = &new_comment {
                    if *block == index {
                        text = text.child(self.render_new_comment(page, chars.clone()));
                    }
                }
            }
        }

        let mut row = h_flex()
            .relative()
            .w_full()
            .items_start()
            .gap(gap_section())
            .child(text);
        if wide {
            // Each item at its block's top, pushed below the one before.
            let mut items: Vec<(usize, String, AnyElement)> = page
                .threads
                .iter()
                .filter_map(|placed| {
                    Some((
                        placed.block?,
                        placed.thread.root.id.clone(),
                        self.render_thread(placed, cx),
                    ))
                })
                .collect();
            if let Some((Some(block), chars)) = &new_comment {
                let at = items
                    .iter()
                    .position(|(other, _, _)| other > block)
                    .unwrap_or(items.len());
                items.insert(
                    at,
                    (
                        *block,
                        NEW_COMMENT.to_string(),
                        self.render_new_comment(page, chars.clone()),
                    ),
                );
            }
            let mut margin = div().relative().flex_none().w(margin_width());
            let mut cursor = px(0.);
            for (block, key, element) in items {
                let top = measure.block_top(block).max(cursor);
                let height = measure
                    .items
                    .get(&key)
                    .copied()
                    .unwrap_or_else(unmeasured_height);
                cursor = top + height + gap_tight();
                margin = margin.child(
                    div()
                        .absolute()
                        .top(top)
                        .left_0()
                        .w_full()
                        .child(element)
                        .child(probe(&self.measure, &weak, Probe::Item(key))),
                );
            }
            row = row.child(margin.h(cursor));
        }
        drop(measure);

        let orphans: Vec<AnyElement> = page
            .orphans()
            .map(|(_, placed)| self.render_thread(placed, cx))
            .collect();
        v_flex()
            .w_full()
            .gap(gap_tight())
            .child(row.child(probe(&self.measure, &weak, Probe::Row)))
            .when(!orphans.is_empty(), |column| {
                column
                    .child(
                        div()
                            .pt(gap_section())
                            .text_size(meta().size)
                            .line_height(meta().line_height)
                            .font_weight(ANCHOR)
                            .text_color(theme::text_muted())
                            .child(ORPHANS),
                    )
                    .children(orphans)
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
        v_flex()
            .id("wiki-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(pad_x())
            .pt(gap_section())
            .pb(gap_composer())
            .child(self.render_page(page, cx))
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
                        .child(format!("“{}”", one_line(&quote))),
                )
                .child(key_chip("c"))
                .child(
                    Button::new("wiki-comment")
                        .small()
                        .label(COMMENT)
                        // On press, not release: the press may clear the
                        // text views' selection, and the stored one is
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
}

/// A canvas laid over its parent that records the parent's bounds for the
/// next render, and asks for one when they moved.
fn probe(
    measure: &Rc<RefCell<Measure>>,
    view: &WeakEntity<WikiView>,
    key: Probe,
) -> impl IntoElement {
    let measure = measure.clone();
    let view = view.clone();
    canvas(
        move |bounds, _, cx| {
            if measure.borrow_mut().record(&key, bounds) {
                cx.defer(move |cx| {
                    view.update(cx, |_, cx| cx.notify()).ok();
                });
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// `Replying` and three dots whose opacity cycles, as the agent pane's
/// thinking line.
fn replying_dots(id: SharedString) -> impl IntoElement {
    let step = micro();
    h_flex()
        .items_baseline()
        .text_size(step.size)
        .line_height(step.line_height)
        .text_color(theme::text_muted())
        .child(REPLYING)
        .children((0..3).map(move |dot| {
            div().child("・").with_animation(
                (id.clone(), dot),
                Animation::new(Duration::from_millis(DOTS_CYCLE_MS)).repeat(),
                move |dot_el, delta| {
                    let phase = (delta + 1.0 - dot as f32 / 3.0).fract();
                    dot_el.opacity(0.25 + 0.75 * (1.0 - phase))
                },
            )
        }))
}

fn text_line(step: Step, color: Hsla, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(step.size)
        .line_height(step.line_height)
        .text_color(color)
        .child(text.into())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in `gpui::*`, whose `test` shadows the
    // attribute `#[gpui::test]` expands to.
    use gpui::{px, size, Bounds, TestAppContext, WindowHandle};
    use horizon_wiki::Store;

    use super::{Measure, Probe, Target, WikiView};

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
    fn a_new_thread_opens_and_a_reply_continues_it_and_a_click_closes_it(cx: &mut TestAppContext) {
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
        let (page_id, root) = window
            .read_with(cx, |view, _| {
                let page = view.page.as_ref().unwrap();
                assert_eq!(page.threads.len(), 1);
                assert_eq!(page.threads[0].quote(), "helper");
                assert!(view.target.is_none());
                let root = page.threads[0].thread.root.id.clone();
                // The new thread shows open and in focus.
                assert!(view.expanded.contains(&root));
                assert_eq!(view.focused.as_ref(), Some(&root));
                (page.id.unwrap(), root)
            })
            .unwrap();

        window
            .update(cx, |view, window, cx| {
                view.focus_thread(root.clone(), window, cx);
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

        window
            .update(cx, |view, window, cx| {
                view.toggle_thread(root.clone(), window, cx);
                assert!(!view.expanded.contains(&root));
                assert!(view.focused.is_none());
                assert!(view.target.is_none());
            })
            .unwrap();
    }

    /// The margin is used only when the row has room for it beside the
    /// text, and a block's top is read relative to the row.
    #[test]
    fn the_margin_needs_room_and_tops_are_relative_to_the_row() {
        let mut measure = Measure::default();
        assert!(!measure.wide(), "unmeasured is narrow");
        let row = Bounds::new(gpui::point(px(0.), px(100.)), size(px(1200.), px(800.)));
        assert!(measure.record(&Probe::Row, row));
        assert!(!measure.record(&Probe::Row, row), "no change, no redraw");
        assert!(measure.wide());
        let block = Bounds::new(gpui::point(px(0.), px(160.)), size(px(600.), px(40.)));
        measure.record(&Probe::Block(2), block);
        assert_eq!(measure.block_top(2), px(60.));
        let narrow = Bounds::new(gpui::point(px(0.), px(100.)), size(px(400.), px(800.)));
        measure.record(&Probe::Row, narrow);
        assert!(!measure.wide());
    }
}
