//! The wiki view: one page of the project wiki drawn as running text, with
//! each comment thread marked where it points and read after the block
//! its passage starts in.
//!
//! A thread starts closed, as one line (its quote, how many comments,
//! whether an agent is replying), and opens on a click; an open thread
//! holds its field for a reply. Text selected in the page can be
//! commented on, which starts a thread. Threads whose passage left the
//! page are listed after it.
//!
//! Comments are written straight to the page's thread log; the daemon's
//! wiki flow notices them and answers through the thread's session, and
//! lists the comments it is answering beside the log. The page file, the
//! log and that list are watched, so an answer or an edit is drawn.
//!
//! The page as a view reads it is `horizon-wiki`'s `view_model`, as the
//! GPUI shell's view uses it. That view also sets threads in a margin
//! beside the text on a wide pane, and has a second layout with a dialog;
//! here they are in the page at any width.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::{LocalStorage, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, With, WithValue};
use horizon_ui::{
    El, Page, dispatched, each_layout, el, prevent_default, relayout, stop_propagation,
};
use horizon_wiki::Store;
use horizon_wiki::anchor::Anchor;
use horizon_wiki::annotation::{Agent, AgentType, Annotation, markdown_body};
use horizon_wiki::view_model::{PlacedThread, WikiPage};
use horizon_workspace::markdown::{LinkTarget, resolve_link};
use keyboard_types::{Key, NamedKey};

use crate::background::Background;
use crate::markdown::watch::{self, Changed};
use crate::wake::Wake;

mod render;

use render::Piece;

const NO_PAGE: &str = "No wiki page";
const LOADING: &str = "Loading";
const ORPHANS: &str = "No longer in the page";
const CHANGED_SINCE: &str = "changed since";
const NO_SELECTION: &str = "Select text in the page to comment on it";
const COMMENT_ON: &str = "Comment on";
const TO_SEND: &str = "to send, Esc to cancel";
const REPLYING: &str = "Replying";
const UNSUPPORTED_LINK: &str = "Not a wiki page";
const BACK: &str = "Back";
const COMMENT: &str = "Comment";

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it. The arrangement follows `src/wiki/view.rs`.
pub const CSS: &str = "
    .wiki { display: flex; flex-direction: column; height: 100%; min-width: 240px;
            outline: none; background: var(--background); color: var(--text);
            font-family: var(--font-family); font-size: var(--body-size);
            line-height: var(--body-line);
        .header { flex: none; display: flex; flex-direction: column; gap: var(--u8);
                  padding: var(--u24) var(--u16) 0 var(--u16); }
        .back { align-self: flex-start; padding: 0 var(--u8); border-radius: 4px;
                font-size: var(--meta-size); line-height: 20px; color: var(--text-muted);
                cursor: pointer; }
        .back:hover { background: var(--surface-raised); }
        .heading { font-size: var(--t1-size); line-height: var(--t1-line); font-weight: 600; }
        .fields { display: flex; flex-wrap: wrap; gap: var(--u4); }
        .chip { padding: 0 var(--u4); border-radius: 2px; font-size: var(--micro-size);
                line-height: var(--micro-line); background: var(--chip-muted-surface);
                color: var(--chip-muted-text); }
        .chip.accent { background: var(--chip-accent-surface);
                       color: var(--chip-accent-text); }
        .state, .notice { flex: none; padding: var(--u24) var(--u16) 0 var(--u16);
                          font-size: var(--meta-size); line-height: var(--meta-line);
                          color: var(--text-muted); }
        .state.error { color: var(--danger); }
        .notice { padding-top: var(--u8); }
        .scroll { flex: 1; min-height: 0; overflow-y: scroll; display: flex;
                  flex-direction: column; gap: var(--u8);
                  padding: var(--u24) var(--u16) var(--u32) var(--u16); }
        .block { white-space: pre-wrap; }
        .block > :first-child { margin-top: 0; }
        .block > :last-child { margin-bottom: 0; }
        .block h1 { font-size: var(--t2-size); line-height: var(--t2-line); font-weight: 600;
                    margin: 8px 0 2px 0; }
        .block h2, .block h3 { font-size: var(--body-size); line-height: var(--body-line);
                               font-weight: 600; margin: 8px 0 2px 0; }
        .block p, .block ul, .block ol, .block pre, .block blockquote, .block table {
            margin: 0 0 8px 0; }
        .block ul, .block ol { padding-left: 18px; }
        .block pre { padding: 12px; border: 1px solid var(--border); border-radius: 6px;
                     background: var(--surface-panel); overflow-x: scroll;
                     font-family: var(--font-family); }
        .block code { font-family: var(--font-family); padding: 0 var(--u4);
                      border-radius: 2px; font-size: var(--meta-size);
                      background: var(--surface-panel); }
        .block blockquote { padding-left: 12px; border-left: 2px solid var(--border);
                            color: var(--text-muted); }
        .block td { padding: 2px 8px; border: 1px solid var(--border); }
        .link { color: var(--accent); cursor: pointer; }
        .mark { background: var(--selection); }
        .mark.focused { background: var(--surface-selected); }
        .thread, .new { display: flex; flex-direction: column; gap: var(--u4);
                        padding: var(--u4); border: 1px solid var(--border);
                        border-radius: 4px; }
        .thread.focused, .new { border-color: var(--accent); }
        .head, .quoted { display: flex; align-items: center; gap: 2px;
                         font-size: var(--meta-size); line-height: var(--meta-line);
                         color: var(--text-muted); }
        .head { cursor: pointer; }
        .quote { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap;
                 text-overflow: ellipsis; }
        .count { flex: none; }
        .post { display: flex; flex-direction: column; gap: 2px;
                padding: var(--u4) var(--u16); border: 1px solid var(--border);
                border-radius: 4px; }
        .post.person { margin-left: var(--u24); border: none; border-radius: 0;
                       border-left: 2px solid var(--text-muted); }
        .post .body > :first-child { margin-top: 0; }
        .post .body > :last-child { margin-bottom: 0; }
        .post .body p, .post .body ul, .post .body ol, .post .body pre { margin: 0 0 8px 0; }
        .when, .hint { font-size: var(--micro-size); line-height: var(--micro-line);
                       color: var(--text-muted); }
        .replying { position: relative; padding: var(--u4) var(--u16);
                    border: 1px solid var(--border); border-radius: 4px;
                    font-size: var(--meta-size); line-height: var(--meta-line);
                    color: var(--text-muted); }
        .replying .bar { position: absolute; left: 0; top: 0; bottom: 0; width: 2px;
                         background: var(--accent); }
        .composer { display: flex; flex-direction: column; gap: 2px; padding-top: var(--u4); }
        .composer .field { padding: 2px var(--u16); border: 1px solid var(--border);
                           border-radius: 4px; }
        .composer textarea { width: 100%; box-sizing: border-box; padding: 0; border: none;
                             outline: none; resize: none; background: transparent;
                             color: var(--text); font-family: var(--font-family);
                             font-size: var(--body-size); line-height: var(--body-line);
                             height: calc(var(--body-line) * 2); }
        .send { display: flex; align-items: center; justify-content: flex-end; gap: 2px; }
        .key { padding: 0 var(--u4); border: 1px solid var(--border); border-radius: 3px;
               font-size: var(--micro-size); line-height: var(--micro-line);
               color: var(--text-muted); }
        .offer { display: flex; align-items: center; gap: 2px; }
        .button { padding: 0 var(--u12); border-radius: 4px; line-height: var(--u24);
                  font-size: var(--meta-size); border: 1px solid var(--border);
                  cursor: pointer; }
        .orphans { padding-top: var(--u24); font-size: var(--meta-size);
                   line-height: var(--meta-line); font-weight: 600;
                   color: var(--text-muted); }
    }
";

/// What is outside the view: a URL handed to the desktop.
pub type OpenUrl = Rc<dyn Fn(&str)>;

/// What the field is for: a new thread on a passage (its logical
/// characters), or a reply in a thread (by its root's id).
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Selection(Range<usize>),
    Thread(String),
}

/// What the view keeps that is not a signal.
struct Held {
    background: Background,
    wake: Wake,
    open_url: OpenUrl,
    /// A change of the files drawn from, noted by the watches on them.
    changed: Changed,
    watches: RefCell<Vec<notify::RecommendedWatcher>>,
    /// Each block's runs of text, and its element.
    pieces: RefCell<HashMap<usize, Vec<Piece>>>,
    blocks: RefCell<HashMap<usize, El>>,
    /// The block to bring into view at the next layout.
    reveal: Cell<Option<usize>>,
    /// Who comments here: the installation's owner.
    owner: Cell<Option<uuid::Uuid>>,
}

/// The view's state, for whoever placed it to read or drive.
#[derive(Clone, Copy)]
pub struct Wiki {
    pub path: RwSignal<Option<PathBuf>>,
    page: RwSignal<Option<Rc<WikiPage>>, LocalStorage>,
    error: RwSignal<Option<String>>,
    /// The thread in focus, and the threads open.
    pub focused: RwSignal<Option<String>>,
    pub expanded: RwSignal<HashSet<String>>,
    pub target: RwSignal<Option<Target>>,
    /// The logical characters selected in the page.
    pub selection: RwSignal<Option<Range<usize>>>,
    pub notice: RwSignal<Option<String>>,
    /// The pages left by following links, most recent last.
    pub history: RwSignal<Vec<PathBuf>>,
    typed: RwSignal<String>,
    held: StoredValue<Rc<Held>, LocalStorage>,
    pub element: El,
    scroll: El,
}

/// One line of the page: a block of its text, or something set after one.
#[derive(Clone)]
enum Line {
    Block {
        index: usize,
        key: String,
    },
    /// The offer to comment on what is selected.
    Offer,
    New(Range<usize>),
    Thread {
        placed: Box<PlacedThread>,
        key: String,
    },
    Orphans,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Wiki {
    fn held(&self) -> Rc<Held> {
        self.held.with_value(Rc::clone)
    }

    fn store(&self) -> Option<Store> {
        (self.path.get_untracked().as_deref())
            .and_then(Path::parent)
            .map(Store::at)
    }

    fn page_name(path: &Path) -> Option<String> {
        (path.file_stem()).map(|stem| stem.to_string_lossy().into_owned())
    }

    /// Points the view at `path` from outside: the history is left as it
    /// is, since the jump was not a link followed here.
    pub fn open(&self, path: PathBuf) {
        if self.path.get_untracked().as_ref() == Some(&path) {
            self.reload();
        } else {
            self.show(path);
        }
    }

    fn show(&self, path: PathBuf) {
        self.page.set(None);
        self.error.set(None);
        self.focused.set(None);
        self.expanded.set(HashSet::new());
        self.target.set(None);
        self.selection.set(None);
        self.notice.set(None);
        self.scroll.scroll_to(0., 0.);
        let owner = (path.parent().and_then(Path::parent))
            .and_then(|root| horizon_wiki::thread::owner_id(root).ok());
        self.held().owner.set(owner);
        self.path.set(Some(path));
        self.watch();
        self.reload();
    }

    /// Watches the page's file and, once the page has an id, its thread
    /// log and the list of comments being answered.
    fn watch(&self) {
        let held = self.held();
        let Some(path) = self.path.get_untracked() else {
            return;
        };
        let mut files = vec![path];
        let id = self.page.get_untracked().and_then(|page| page.id);
        if let (Some(store), Some(id)) = (self.store(), id) {
            let _ = std::fs::create_dir_all(store.threads_dir());
            files.push(store.thread_log_path(id));
            files.push(store.replying_path());
        }
        *held.watches.borrow_mut() = (files.into_iter())
            .filter_map(|file| watch::on(file, held.changed.clone(), held.wake.clone()))
            .collect();
    }

    /// Reads the page and its threads again, off the UI thread.
    pub fn reload(&self) {
        let (Some(path), Some(store)) = (self.path.get_untracked(), self.store()) else {
            return;
        };
        let Some(name) = Self::page_name(&path) else {
            return;
        };
        let this = *self;
        self.held().background.run(
            move || WikiPage::load(&store, &name),
            move |loaded| {
                // (A page shown since the read began is not this one.)
                if this.path.get_untracked().as_ref() != Some(&path) {
                    return;
                }
                match loaded {
                    Ok(page) => this.loaded(page),
                    Err(error) => this.error.set(Some(error)),
                }
            },
        );
    }

    fn loaded(&self, page: WikiPage) {
        let had_id = self.page.get_untracked().and_then(|page| page.id);
        let roots: HashSet<String> = (page.threads.iter())
            .map(|placed| placed.thread.root.id.clone())
            .collect();
        // What was open or in focus stays so only while its thread does.
        self.expanded
            .update(|expanded| expanded.retain(|root| roots.contains(root)));
        if (self.focused.get_untracked()).is_some_and(|root| !roots.contains(&root)) {
            self.focused.set(None);
        }
        if let Some(Target::Thread(root)) = self.target.get_untracked()
            && !roots.contains(&root)
        {
            self.target.set(None);
        }
        let id = page.id;
        self.error.set(None);
        self.page.set(Some(Rc::new(page)));
        // The log to watch is known once the page has an id.
        if id != had_id {
            self.watch();
        }
    }

    fn placed(&self, root: &str) -> Option<PlacedThread> {
        let page = self.page.get_untracked()?;
        (page.threads.iter())
            .find(|placed| placed.thread.root.id == root)
            .cloned()
    }

    /// Opens a thread, with the field for a reply in it and its passage
    /// in view.
    pub fn focus_thread(&self, root: String) {
        self.expanded.update(|expanded| {
            expanded.insert(root.clone());
        });
        self.focused.set(Some(root.clone()));
        self.held()
            .reveal
            .set(self.placed(&root).and_then(|placed| placed.block));
        self.target.set(Some(Target::Thread(root)));
    }

    fn toggle_thread(&self, root: String) {
        if !self.expanded.with(|expanded| expanded.contains(&root)) {
            self.focus_thread(root);
            return;
        }
        self.expanded.update(|expanded| {
            expanded.remove(&root);
        });
        if self.focused.get_untracked().as_ref() == Some(&root) {
            self.focused.set(None);
        }
        if self.target.get_untracked() == Some(Target::Thread(root)) {
            self.target.set(None);
            self.element.focus();
        }
    }

    /// Starts a comment on what is selected in the page.
    pub fn comment_on_selection(&self) {
        match self.selection.get_untracked() {
            Some(chars) => {
                self.target.set(Some(Target::Selection(chars)));
                self.notice.set(None);
            }
            None => self.notice.set(Some(NO_SELECTION.to_string())),
        }
    }

    fn cancel(&self) {
        self.target.set(None);
        self.typed.set(String::new());
        self.element.focus();
    }

    /// Writes what is typed: a new thread on the passage, or a reply in
    /// the thread.
    fn send(&self) {
        let text = self.typed.get_untracked().trim().to_string();
        let (Some(target), Some(store), Some(path), Some(owner)) = (
            self.target.get_untracked(),
            self.store(),
            self.path.get_untracked(),
            self.held().owner.get(),
        ) else {
            return;
        };
        let Some(name) = Self::page_name(&path).filter(|_| !text.is_empty()) else {
            return;
        };
        let creator = Agent::person(owner, "owner");
        let written = match &target {
            Target::Selection(chars) => store
                .ask(&name, chars.clone(), creator, markdown_body(text))
                .map(|(_, root)| Some(root.id)),
            Target::Thread(root) => {
                let id = self.page.get_untracked().and_then(|page| page.id);
                match (id, self.placed(root)) {
                    (Some(id), Some(placed)) => store
                        .reply(id, &placed.thread.last().id, creator, markdown_body(text))
                        .map(|_| None),
                    _ => return,
                }
            }
        };
        match written {
            Ok(new_root) => {
                self.notice.set(None);
                self.typed.set(String::new());
                // A new thread is open and in focus; the keys go back to
                // the page. A reply leaves the field where it was.
                if let Some(root) = new_root {
                    self.expanded.update(|expanded| {
                        expanded.insert(root.clone());
                    });
                    self.focused.set(Some(root));
                    self.target.set(None);
                    self.element.focus();
                } else {
                    // (The field is built again, empty, with the thread.)
                    self.target.update(|_| {});
                }
                self.reload();
            }
            Err(error) => self.notice.set(Some(error.to_string())),
        }
    }

    fn follow_link(&self, target: &str) {
        let Some(current) = self.path.get_untracked() else {
            return;
        };
        match resolve_link(&current, target) {
            LinkTarget::Document(path) if path.parent() == current.parent() => {
                if path != current {
                    self.history.update(|history| history.push(current));
                    self.show(path);
                }
            }
            LinkTarget::External(url) => (self.held().open_url)(&url),
            _ => self
                .notice
                .set(Some(format!("{UNSUPPORTED_LINK}: {target}"))),
        }
    }

    fn back(&self) {
        let mut previous = None;
        self.history.update(|history| previous = history.pop());
        if let Some(previous) = previous {
            self.show(previous);
        }
    }

    /// The logical characters the page's selection covers, read through
    /// the runs of text the blocks drew.
    fn selected(&self, page: &Page) -> Option<Range<usize>> {
        let held = self.held();
        let pieces = held.pieces.borrow();
        let mut covered: Option<Range<usize>> = None;
        for (root, start, end) in page.selection() {
            let Some(text) = page.inline_text(root) else {
                continue;
            };
            // Each run is found where it is in the element's text, in
            // order.
            let mut from = 0;
            for piece in pieces.values().flatten().filter(|piece| piece.root == root) {
                let Some(at) = text[from..].find(&piece.value).map(|at| at + from) else {
                    continue;
                };
                from = at + piece.value.len();
                let (low, high) = (start.max(at), end.min(from));
                let Some(logical) = piece.logical.as_ref().filter(|_| low < high) else {
                    continue;
                };
                let chars = |byte: usize| piece.value[..byte - at].chars().count();
                let (low, high) = (logical.start + chars(low), logical.start + chars(high));
                covered = Some(match covered {
                    Some(earlier) => earlier.start.min(low)..earlier.end.max(high),
                    None => low..high,
                });
            }
        }
        covered
    }
}

fn chip(class: &str, text: &str) -> El {
    el("span").class(class).text(text)
}

/// The field for a comment, with what was typed so far in it.
fn composer(state: Wiki) -> El {
    let field = el("textarea").attr("rows", "2");
    let typed = state.typed.get_untracked();
    if !typed.is_empty() {
        field.set_value(&typed);
    }
    field
        .on("input", move |event| {
            if let DomEventData::Input(input) = &event.data {
                state.typed.set(input.value.clone());
            }
        })
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            match &key.key {
                Key::Named(NamedKey::Enter) if !key.modifiers.shift() && !key.is_composing => {
                    state.send();
                    prevent_default();
                }
                Key::Named(NamedKey::Escape) => state.cancel(),
                _ => {}
            }
            // The field's keys are its own.
            stop_propagation();
        })
        .focus();
    el("div")
        .class("composer")
        .child(el("div").class("field").child(field))
        .child(
            el("div")
                .class("send")
                .child(el("span").class("key").text("Enter"))
                .child(el("span").class("hint").text(TO_SEND)),
        )
}

fn post(annotation: &Annotation) -> El {
    let class = match annotation.creator.kind {
        AgentType::Person => "post person",
        AgentType::Software => "post",
    };
    let when = annotation.created.replace('T', " ").replace('Z', "");
    el("div")
        .class(class)
        .child(crate::markdown::render(
            &annotation.body.value,
            Rc::new(|_| {}),
        ))
        .child(el("div").class("when").text(&when))
}

/// What a thread's card shows, as text: the card is built again when any
/// of it changes.
fn thread_key(
    placed: &PlacedThread,
    open: bool,
    focused: bool,
    editing: bool,
    replying: bool,
) -> String {
    let changed = matches!(placed.anchor, Anchor::Moved { exact: false, .. });
    format!(
        "{} {open} {focused} {editing} {replying} {changed} {} {} {}",
        placed.thread.root.id,
        placed.thread.replies.len(),
        placed.thread.last().id,
        placed.quote()
    )
}

fn thread(state: Wiki, placed: &PlacedThread, replying: bool) -> El {
    let root = placed.thread.root.id.clone();
    let open = state.expanded.with(|expanded| expanded.contains(&root));
    let focused = state.focused.get_untracked().as_ref() == Some(&root);
    let editing = state.target.get_untracked() == Some(Target::Thread(root.clone()));
    let posts: Vec<&Annotation> = std::iter::once(&placed.thread.root)
        .chain(&placed.thread.replies)
        .collect();
    let count = match posts.len() {
        1 => "1 comment".to_string(),
        count => format!("{count} comments"),
    };
    let toggled = root.clone();
    let head = el("div")
        .class("head")
        .child(el("span").text(if open { "▾" } else { "▸" }))
        .child(
            el("span")
                .class("quote")
                .text(&format!("“{}”", one_line(&placed.quote()))),
        )
        .child(el("span").class("count").text(&count))
        .on("click", move |_| state.toggle_thread(toggled.clone()));
    if matches!(placed.anchor, Anchor::Moved { exact: false, .. }) {
        head.child(chip("chip", CHANGED_SINCE));
    }
    if replying && !open {
        head.child(chip("chip accent", REPLYING));
    }
    let card = el("div")
        .class(if focused { "thread focused" } else { "thread" })
        .child(head);
    if open {
        for annotation in posts {
            card.child(post(annotation));
        }
        if replying {
            card.child(
                el("div")
                    .class("replying")
                    .child(el("div").class("bar"))
                    .child(el("span").text(REPLYING)),
            );
        }
        if editing {
            card.child(composer(state));
        }
    }
    card
}

/// Builds the view, opened on `path`: a page's file in a project's wiki.
pub fn view(path: Option<PathBuf>, open_url: OpenUrl, wake: Wake) -> (El, Wiki) {
    let state = Wiki {
        path: RwSignal::new(None),
        page: RwSignal::new_local(None),
        error: RwSignal::new(None),
        focused: RwSignal::new(None),
        expanded: RwSignal::new(HashSet::new()),
        target: RwSignal::new(None),
        selection: RwSignal::new(None),
        notice: RwSignal::new(None),
        history: RwSignal::new(Vec::new()),
        typed: RwSignal::new(String::new()),
        held: StoredValue::new_local(Rc::new(Held {
            background: Background::new(wake.clone()),
            wake,
            open_url,
            changed: Changed::default(),
            watches: RefCell::default(),
            pieces: RefCell::default(),
            blocks: RefCell::default(),
            reveal: Cell::new(None),
            owner: Cell::new(None),
        })),
        element: el("div").class("wiki").attr("tabindex", "0"),
        scroll: el("div").class("scroll"),
    };
    let (root, scroll) = (state.element, state.scroll);
    let held = state.held();
    if let Some(path) = path {
        state.show(path);
    }

    let header = el("div")
        .class("header")
        .when(
            move || !state.history.with(Vec::is_empty),
            move || {
                el("div")
                    .class("back")
                    .text(BACK)
                    .on("click", move |_| state.back())
            },
        )
        .each(
            move || {
                let page = state.page.get();
                let title = page.as_ref().and_then(|page| page.document.title.clone());
                title.into_iter().collect::<Vec<_>>()
            },
            |title| title.clone(),
            |title| el("div").class("heading").text(title),
        )
        .each(
            move || {
                let page = state.page.get();
                let fields: Vec<(String, String)> = page
                    .iter()
                    .flat_map(|page| page.document.fields.clone())
                    .filter(|(key, _)| key != "id")
                    .collect();
                (!fields.is_empty())
                    .then_some(fields)
                    .into_iter()
                    .collect::<Vec<_>>()
            },
            |fields| fields.clone(),
            |fields| {
                let row = el("div").class("fields");
                for (key, value) in fields {
                    row.child(chip("chip", &format!("{key} {value}")));
                }
                row
            },
        );

    // The page: its blocks in order, each followed by what is set after
    // it, then the threads whose passage has gone.
    let drawing = held.clone();
    scroll.each(
        move || {
            let Some(page) = state.page.get() else {
                return Vec::new();
            };
            let focused = state.focused.get();
            let expanded = state.expanded.get();
            let target = state.target.get();
            let selection = state.selection.get();
            let card = |placed: &PlacedThread| {
                let root = &placed.thread.root.id;
                let replying = placed.replying(&page.replying);
                let key = thread_key(
                    placed,
                    expanded.contains(root),
                    focused.as_ref() == Some(root),
                    target == Some(Target::Thread(root.clone())),
                    replying,
                );
                Line::Thread {
                    placed: Box::new(placed.clone()),
                    key,
                }
            };
            let starting = match &target {
                Some(Target::Selection(chars)) => {
                    (page.block_of(chars.clone())).map(|block| (block, Line::New(chars.clone())))
                }
                Some(Target::Thread(_)) => None,
                None => (selection.clone())
                    .and_then(|chars| page.block_of(chars))
                    .map(|block| (block, Line::Offer)),
            };
            let mut lines = Vec::new();
            for (index, block) in page.blocks.iter().enumerate() {
                // The block is drawn again when its text is another, or
                // the passages marked in it are.
                let within = page.logical_range(block.clone());
                let marks: Vec<String> = (page.threads.iter())
                    .filter_map(|placed| {
                        let chars = placed.anchor.range()?;
                        let within = within.as_ref()?;
                        (chars.start < within.end && within.start < chars.end).then(|| {
                            let lit = focused.as_ref() == Some(&placed.thread.root.id);
                            format!("{chars:?}{lit}")
                        })
                    })
                    .collect();
                let key = format!("{index} {} {marks:?}", &page.document.body[block.clone()]);
                lines.push(Line::Block { index, key });
                if let Some((_, line)) = starting.as_ref().filter(|(at, _)| *at == index) {
                    lines.push(line.clone());
                }
                for placed in &page.threads {
                    if placed.block == Some(index) {
                        lines.push(card(placed));
                    }
                }
            }
            let orphans: Vec<Line> = page.orphans().map(|(_, placed)| card(placed)).collect();
            if !orphans.is_empty() {
                lines.push(Line::Orphans);
                lines.extend(orphans);
            }
            lines
        },
        |line| match line {
            Line::Block { key, .. } => format!("block {key}"),
            Line::Offer => "offer".to_string(),
            Line::New(chars) => format!("new {chars:?}"),
            Line::Thread { key, .. } => format!("thread {key}"),
            Line::Orphans => "orphans".to_string(),
        },
        move |line| {
            let Some(page) = state.page.get_untracked() else {
                return el("div");
            };
            match line {
                Line::Block { index, .. } => {
                    let focused = state.focused.get_untracked();
                    let marks: Vec<render::Mark> = (page.threads.iter())
                        .filter_map(|placed| {
                            let lit = focused.as_ref() == Some(&placed.thread.root.id);
                            Some((placed.anchor.range()?, lit))
                        })
                        .collect();
                    let Some(range) = page.blocks.get(*index).cloned() else {
                        return el("div");
                    };
                    let on_link: Rc<dyn Fn(&str)> =
                        Rc::new(move |target| state.follow_link(target));
                    let (element, pieces) = render::block(&page, range, &marks, on_link);
                    drawing.pieces.borrow_mut().insert(*index, pieces);
                    drawing.blocks.borrow_mut().insert(*index, element);
                    element
                }
                // (Pressed, not clicked: the press itself would take the
                // selection it is an offer for.)
                Line::Offer => {
                    el("div")
                        .class("offer")
                        .child(el("div").class("button").text(COMMENT).on(
                            "pointerdown",
                            move |_| {
                                state.comment_on_selection();
                                prevent_default();
                                stop_propagation();
                            },
                        ))
                        .child(el("span").class("key").text("c"))
                }
                Line::New(chars) => {
                    let quote = one_line(&page.quote_of(chars.clone()));
                    el("div")
                        .class("new")
                        .child(
                            el("div").class("quoted").child(
                                el("span")
                                    .class("quote")
                                    .text(&format!("{COMMENT_ON} “{quote}”")),
                            ),
                        )
                        .child(composer(state))
                }
                Line::Thread { placed, .. } => {
                    thread(state, placed, placed.replying(&page.replying))
                }
                Line::Orphans => el("div").class("orphans").text(ORPHANS),
            }
        },
    );

    // After each layout: what was read off the UI thread, a change of the
    // files, the selection as it is now, and the passage to bring into
    // view.
    let laid = held.clone();
    each_layout(move |page| {
        if laid.background.take_in() {
            relayout();
        }
        if laid.changed.settled() {
            state.reload();
        }
        if state.page.get_untracked().is_none() {
            return;
        }
        let selected = state.selected(page);
        if selected != state.selection.get_untracked() {
            state.selection.set(selected);
            relayout();
        }
        if let Some(index) = laid.reveal.take() {
            let block = laid.blocks.borrow().get(&index).copied();
            if let (Some(block), Some(view)) = (
                block.and_then(|block| page.bounds(block)),
                page.bounds(scroll),
            ) {
                let above = block.y - view.y;
                let below = (block.y + block.height) - (view.y + view.height);
                if above < 0. {
                    scroll.scroll_by(0., above);
                } else if below > 0. {
                    scroll.scroll_by(0., below);
                }
            }
        }
    });
    // The pieces and elements of blocks that are gone are let go of.
    Effect::new(move |_| {
        let count = state.page.get().map_or(0, |page| page.blocks.len());
        held.pieces.borrow_mut().retain(|index, _| *index < count);
        held.blocks.borrow_mut().retain(|index, _| *index < count);
    });

    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        if key.key == Key::Named(NamedKey::Escape) && state.target.get_untracked().is_some() {
            state.cancel();
            stop_propagation();
            return;
        }
        if key.modifiers.ctrl() || key.modifiers.alt() || key.modifiers.meta() {
            return;
        }
        // A line is three of the body's lines; a page most of the box.
        let scroll_by = move |lines: f64, pages: f64| {
            dispatched(move |page| {
                let height = page.size(scroll).map_or(0., |(_, height)| height);
                scroll.scroll_by(0., lines * 26. * 3. + pages * height * 0.8);
            });
        };
        let character = match &key.key {
            Key::Character(character) => character.as_str(),
            _ => "",
        };
        match (&key.key, character) {
            (_, "j") | (Key::Named(NamedKey::ArrowDown), _) => scroll_by(1., 0.),
            (_, "k") | (Key::Named(NamedKey::ArrowUp), _) => scroll_by(-1., 0.),
            (_, " ") | (Key::Named(NamedKey::PageDown), _) => scroll_by(0., 1.),
            (Key::Named(NamedKey::PageUp), _) => scroll_by(0., -1.),
            (_, "h") | (Key::Named(NamedKey::Backspace | NamedKey::ArrowLeft), _) => state.back(),
            (_, "c") => state.comment_on_selection(),
            // The next thread, or the one before, around the page's.
            (_, "n" | "p") => {
                let roots: Vec<String> = state.page.get_untracked().map_or_else(Vec::new, |page| {
                    (page.threads.iter())
                        .map(|placed| placed.thread.root.id.clone())
                        .collect()
                });
                if roots.is_empty() {
                    return;
                }
                let at = (state.focused.get_untracked())
                    .and_then(|root| roots.iter().position(|known| *known == root));
                let next = match (at, character) {
                    (None, "n") => 0,
                    (None, _) => roots.len() - 1,
                    (Some(index), "n") => (index + 1) % roots.len(),
                    (Some(index), _) => (index + roots.len() - 1) % roots.len(),
                };
                state.focus_thread(roots[next].clone());
            }
            _ => return,
        }
        prevent_default();
    })
    .child(header)
    .when(
        move || state.notice.with(Option::is_some),
        move || {
            el("div")
                .class("notice")
                .text_dyn(move || state.notice.get().unwrap_or_default())
        },
    )
    .each(
        move || {
            let stated = match (state.path.get(), state.error.get(), state.page.get()) {
                (None, ..) => Some((NO_PAGE.to_string(), false)),
                (_, Some(error), _) => Some((error, true)),
                (_, _, None) => Some((LOADING.to_string(), false)),
                _ => None,
            };
            stated.into_iter().collect::<Vec<_>>()
        },
        |stated| stated.clone(),
        |(text, error)| {
            el("div")
                .class(if *error { "state error" } else { "state" })
                .text(text)
        },
    )
    .child(scroll);
    (root, state)
}
