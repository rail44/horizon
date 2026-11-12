//! The wiki view: one page of the project wiki drawn as running text, with
//! each comment thread marked where it points. On a pane wide enough for
//! a margin, threads sit in a column beside the text, each at the top of
//! the block its passage starts in and below the thread before it; on a
//! narrower one they are in the text, after that block.
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
//! GPUI shell's view uses it.
//!
//! There is a second layout, switched to from the header: each thread is
//! only its head line in the page, and the thread or new comment being
//! worked on is in a dialog over the view.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::{LocalStorage, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{
    Get, GetUntracked, Set, Update, With, WithUntracked, WithValue,
};
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
const TO_DIALOG: &str = "Comments in a dialog";
const TO_MARGIN: &str = "Comments in the margin";
const CLOSE: &str = "Close";

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it. The arrangement follows `src/wiki/view.rs`.
pub const CSS: &str = "
    .wiki { position: relative; display: flex; flex-direction: column; height: 100%;
            min-width: 240px;
            outline: none; background: var(--background); color: var(--text);
            font-family: var(--font-family); font-size: var(--body-size);
            line-height: var(--body-line);
        .header { flex: none; display: flex; flex-direction: column; gap: var(--u8);
                  padding: var(--u24) var(--u16) 0 var(--u16); }
        .bar { display: flex; align-items: center; gap: var(--u8); }
        .bar .space { flex: 1; }
        .back, .layout, .close { flex: none; padding: 0 var(--u8); border-radius: 4px;
                                 font-size: var(--meta-size); line-height: 20px;
                                 color: var(--text-muted); cursor: pointer; }
        .back:hover, .layout:hover, .close:hover { background: var(--surface-raised); }
        .scrim { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 10;
                 display: flex; justify-content: center; align-items: flex-start;
                 padding-top: calc(var(--u24) * 2); }
        .dialog { display: flex; flex-direction: column; gap: var(--u8);
                  width: calc(var(--u24) * 30); max-width: 92%; max-height: 85%;
                  box-sizing: border-box; padding: var(--u16);
                  background: var(--background); border: 1px solid var(--border);
                  border-radius: 6px;
                  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18), 0 2px 8px rgba(0, 0, 0, 0.1); }
        .dialog .top { flex: none; display: flex; align-items: flex-start; gap: var(--u8); }
        .dialog .passage { flex: 1; min-width: 0; font-size: var(--meta-size);
                           line-height: var(--meta-line); color: var(--text-muted); }
        .dialog .posts { flex: 1; min-height: 0; overflow-y: scroll; display: flex;
                         flex-direction: column; gap: var(--u8); }
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
        .unit { position: absolute; width: var(--u16); height: 0; visibility: hidden; }
        .row { flex: none; display: flex; align-items: flex-start; gap: var(--u24); }
        .row.narrow { gap: 0; }
        .text { flex: 1; min-width: 0; display: flex; flex-direction: column;
                gap: var(--u8); }
        .margin { flex: none; position: relative; }
        .aside { position: absolute; left: 0; right: 0; }
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

/// The share of the width the margin takes.
const MARGIN_SHARE: f64 = 0.4;

/// The margin's width for `available` (the width the page's text and its
/// margin share), or `None` when the text beside it would get too narrow.
/// `unit` is what one pixel of the reference type size is drawn as.
fn margin_for(available: f64, unit: f64) -> Option<f64> {
    let margin = (available * MARGIN_SHARE)
        .clamp(240. * unit, 560. * unit)
        .round();
    (available - margin - 24. * unit >= 280. * unit).then_some(margin)
}

/// What is outside the view: a URL handed to the desktop.
pub type OpenUrl = Rc<dyn Fn(&str)>;

/// What the field is for: a new thread on a passage (its logical
/// characters), or a reply in a thread (by its root's id).
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Selection(Range<usize>),
    Thread(String),
}

/// Where an open thread is read: in a margin beside the text (in the page
/// on a narrow pane), or in a dialog over the page, with only each
/// thread's head line in the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentLayout {
    Margin,
    Dialog,
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
    /// What is in the margin, in order: the block each item is beside and
    /// the item's slot. And each slot's element.
    beside: RefCell<Vec<(usize, String)>>,
    asides: RefCell<HashMap<String, El>>,
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
    pub layout: RwSignal<CommentLayout>,
    /// The margin's width, on a pane wide enough for one.
    pub margin: RwSignal<Option<f64>>,
    /// Where each item of the margin is from the margin's top, by slot,
    /// and how far down the items reach.
    tops: RwSignal<HashMap<String, f64>>,
    reach: RwSignal<f64>,
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

impl Line {
    /// What names the line's place in the margin, whatever it shows there
    /// just now.
    fn slot(&self) -> String {
        match self {
            Line::Thread { placed, .. } => placed.thread.root.id.clone(),
            Line::New(_) | Line::Offer => "starting".to_string(),
            Line::Block { .. } | Line::Orphans => String::new(),
        }
    }
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

    /// Goes to the other layout. A thread or form being worked on carries
    /// over: the dialog shows it, or it opens in the margin.
    pub fn switch_layout(&self) {
        self.layout.update(|layout| {
            *layout = match layout {
                CommentLayout::Margin => CommentLayout::Dialog,
                CommentLayout::Dialog => CommentLayout::Margin,
            }
        });
        match self.target.get_untracked() {
            Some(Target::Thread(root)) => self.expanded.update(|expanded| {
                expanded.insert(root);
            }),
            Some(Target::Selection(_)) => {}
            None => {
                self.element.focus();
            }
        }
    }

    fn in_dialog(&self) -> bool {
        self.layout.get_untracked() == CommentLayout::Dialog
    }

    fn toggle_thread(&self, root: String) {
        if self.in_dialog() {
            self.focus_thread(root);
            return;
        }
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
                    self.focused.set(Some(root.clone()));
                    if self.in_dialog() {
                        // The dialog stays open on the thread just made.
                        self.target.set(Some(Target::Thread(root)));
                    } else {
                        self.target.set(None);
                        self.element.focus();
                    }
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

fn replying_line() -> El {
    el("div")
        .class("replying")
        .child(el("div").class("bar"))
        .child(el("span").text(REPLYING))
}

/// The thread or new comment being worked on, over the view: the whole
/// quote, the posts and the field. Escape, `Close` or a press outside
/// closes it.
fn dialog(state: Wiki) -> El {
    let scrim = el("div").class("scrim").on("pointerdown", move |_| {
        state.cancel();
    });
    let Some(page) = state.page.get_untracked() else {
        return scrim;
    };
    let (quote, posts, replying) = match state.target.get_untracked() {
        Some(Target::Selection(chars)) => (
            format!("{COMMENT_ON} “{}”", page.quote_of(chars)),
            Vec::new(),
            false,
        ),
        Some(Target::Thread(root)) => match state.placed(&root) {
            Some(placed) => {
                let posts: Vec<Annotation> = std::iter::once(&placed.thread.root)
                    .chain(&placed.thread.replies)
                    .cloned()
                    .collect();
                (
                    format!("“{}”", placed.quote()),
                    posts,
                    placed.replying(&page.replying),
                )
            }
            None => return scrim,
        },
        None => return scrim,
    };
    let list = el("div").class("posts");
    for annotation in &posts {
        list.child(post(annotation));
    }
    if replying {
        list.child(replying_line());
    }
    scrim.child(
        el("div")
            .class("dialog")
            // A press in the dialog is not a press outside it.
            .on("pointerdown", |_| stop_propagation())
            .child(
                el("div")
                    .class("top")
                    .child(el("div").class("passage").text(&quote))
                    .child(el("span").class("key").text("Esc"))
                    .child(
                        el("div")
                            .class("close")
                            .text(CLOSE)
                            .on("click", move |_| state.cancel()),
                    ),
            )
            .child(list)
            .child(composer(state)),
    )
}

fn thread(state: Wiki, placed: &PlacedThread, replying: bool) -> El {
    let root = placed.thread.root.id.clone();
    let open = !state.in_dialog() && state.expanded.with(|expanded| expanded.contains(&root));
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
            card.child(replying_line());
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
        layout: RwSignal::new(CommentLayout::Margin),
        margin: RwSignal::new(None),
        tops: RwSignal::new(HashMap::new()),
        reach: RwSignal::new(0.),
        held: StoredValue::new_local(Rc::new(Held {
            background: Background::new(wake.clone()),
            wake,
            open_url,
            changed: Changed::default(),
            watches: RefCell::default(),
            pieces: RefCell::default(),
            blocks: RefCell::default(),
            beside: RefCell::default(),
            asides: RefCell::default(),
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

    let bar = el("div")
        .class("bar")
        .when(
            move || !state.history.with(Vec::is_empty),
            move || {
                el("div")
                    .class("back")
                    .text(BACK)
                    .on("click", move |_| state.back())
            },
        )
        .child(el("div").class("space"))
        .child(
            el("div")
                .class("layout")
                .text_dyn(move || {
                    match state.layout.get() {
                        CommentLayout::Margin => TO_DIALOG,
                        CommentLayout::Dialog => TO_MARGIN,
                    }
                    .to_string()
                })
                .on("click", move |_| state.switch_layout()),
        );
    let header = el("div")
        .class("header")
        .child(bar)
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

    // A thread's card as a line, and what starts a comment: both tracked.
    let card = move |page: &WikiPage, placed: &PlacedThread| {
        let root = &placed.thread.root.id;
        let in_page = state.layout.get() == CommentLayout::Margin;
        let key = thread_key(
            placed,
            in_page && state.expanded.with(|expanded| expanded.contains(root)),
            state.focused.with(|focused| focused.as_ref() == Some(root)),
            state
                .target
                .with(|target| *target == Some(Target::Thread(root.clone()))),
            placed.replying(&page.replying),
        );
        Line::Thread {
            placed: Box::new(placed.clone()),
            key,
        }
    };
    let starting = move |page: &WikiPage| match state.target.get() {
        // (In the dialog, when that is where comments are written.)
        Some(Target::Selection(_)) if state.layout.get() == CommentLayout::Dialog => None,
        Some(Target::Selection(chars)) => {
            (page.block_of(chars.clone())).map(|block| (block, Line::New(chars)))
        }
        Some(Target::Thread(_)) => None,
        None => (state.selection.get())
            .and_then(|chars| page.block_of(chars))
            .map(|block| (block, Line::Offer)),
    };
    let key_of = |line: &Line| match line {
        Line::Block { key, .. } => format!("block {key}"),
        Line::Offer => "offer".to_string(),
        Line::New(chars) => format!("new {chars:?}"),
        Line::Thread { key, .. } => format!("thread {key}"),
        Line::Orphans => "orphans".to_string(),
    };
    let wide = Memo::new(move |_| {
        state.layout.get() == CommentLayout::Margin && state.margin.with(Option::is_some)
    });

    let drawing = held.clone();
    let draw: Rc<dyn Fn(&Line) -> El> = Rc::new(move |line| {
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
                let on_link: Rc<dyn Fn(&str)> = Rc::new(move |target| state.follow_link(target));
                let (element, pieces) = render::block(&page, range, &marks, on_link);
                drawing.pieces.borrow_mut().insert(*index, pieces);
                drawing.blocks.borrow_mut().insert(*index, element);
                element
            }
            // (Pressed, not clicked: the press itself would take the
            // selection it is an offer for.)
            Line::Offer => el("div")
                .class("offer")
                .child(
                    el("div")
                        .class("button")
                        .text(COMMENT)
                        .on("pointerdown", move |_| {
                            state.comment_on_selection();
                            prevent_default();
                            stop_propagation();
                        }),
                )
                .child(el("span").class("key").text("c")),
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
            Line::Thread { placed, .. } => thread(state, placed, placed.replying(&page.replying)),
            Line::Orphans => el("div").class("orphans").text(ORPHANS),
        }
    });

    // The page's text: its blocks in order, each followed, when there is
    // no margin, by what is set after it.
    let text = el("div").class("text").each(
        move || {
            let Some(page) = state.page.get() else {
                return Vec::new();
            };
            let focused = state.focused.get();
            let starting = (!wide.get()).then(|| starting(&page)).flatten();
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
                if wide.get() {
                    continue;
                }
                if let Some((_, line)) = starting.as_ref().filter(|(at, _)| *at == index) {
                    lines.push(line.clone());
                }
                for placed in &page.threads {
                    if placed.block == Some(index) {
                        lines.push(card(&page, placed));
                    }
                }
            }
            lines
        },
        key_of,
        {
            let draw = draw.clone();
            move |line| draw(line)
        },
    );

    // The margin: what starts a comment and the threads, in the order of
    // their blocks. Each is placed after layout, by its slot.
    let (listing, placing) = (held.clone(), held.clone());
    // (Without a margin the column is of no width, and the row has no gap
    // before it. It is not taken off the page with `display: none`: its
    // items change while it is away, and blitz-dom lays out a box whose
    // children changed while it was not displayed from the children it
    // had.)
    let margin = el("div")
        .class("margin")
        .style_dyn("width", move || {
            let width = state.margin.get().filter(|_| wide.get());
            format!("{}px", width.unwrap_or(0.))
        })
        .style_dyn("min-height", move || {
            let reach = if wide.get() { state.reach.get() } else { 0. };
            format!("{reach}px")
        })
        .each(
            move || {
                let page = state.page.get().filter(|_| wide.get());
                let mut lines: Vec<(usize, Line)> = (page.iter())
                    .flat_map(|page| {
                        (page.threads.iter())
                            .filter_map(|placed| Some((placed.block?, card(page, placed))))
                    })
                    .collect();
                if let Some((block, line)) = page.as_ref().and_then(|page| starting(page)) {
                    let at = (lines.iter())
                        .position(|(other, _)| *other >= block)
                        .unwrap_or(lines.len());
                    lines.insert(at, (block, line));
                }
                let beside: Vec<(usize, String)> = (lines.iter())
                    .map(|(block, line)| (*block, line.slot()))
                    .collect();
                // The elements of what left the margin are let go of.
                (listing.asides.borrow_mut())
                    .retain(|slot, _| beside.iter().any(|(_, kept)| kept == slot));
                *listing.beside.borrow_mut() = beside;
                lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>()
            },
            key_of,
            {
                let draw = draw.clone();
                move |line| {
                    let slot = line.slot();
                    let at = slot.clone();
                    let item = el("div")
                        .class("aside")
                        .style_dyn("top", move || {
                            let top = state.tops.with(|tops| tops.get(&at).copied());
                            format!("{}px", top.unwrap_or(0.))
                        })
                        .child(draw(line));
                    placing.asides.borrow_mut().insert(slot, item);
                    item
                }
            },
        );

    let unit = el("div").class("unit");
    let row = el("div")
        .class_dyn(move || if wide.get() { "row" } else { "row narrow" }.to_string())
        .child(text)
        .child(margin);
    // After them, the threads whose passage has gone.
    scroll.child(unit).child(row).each(
        move || {
            let Some(page) = state.page.get() else {
                return Vec::new();
            };
            let orphans: Vec<Line> = (page.orphans())
                .map(|(_, placed)| card(&page, placed))
                .collect();
            let heading = (!orphans.is_empty()).then_some(Line::Orphans);
            heading.into_iter().chain(orphans).collect::<Vec<_>>()
        },
        key_of,
        move |line| draw(line),
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
        // Whether there is a margin, from the width the text and it share.
        let scale = page.size(unit).map_or(1., |(width, _)| width / 16.);
        let fits = (page.size(row)).and_then(|(width, _)| margin_for(width, scale.max(0.1)));
        if fits != state.margin.get_untracked() {
            state.margin.set(fits);
            relayout();
            return;
        }
        // Each item of the margin at the top of its block, or below the
        // item before it.
        if let Some(column) = fits.and_then(|_| page.bounds(margin)) {
            let (blocks, asides) = (laid.blocks.borrow(), laid.asides.borrow());
            let mut tops = HashMap::new();
            let mut cursor = 0.;
            for (block, slot) in laid.beside.borrow().iter() {
                let Some(item) = asides.get(slot).and_then(|item| page.bounds(*item)) else {
                    continue;
                };
                let beside = (blocks.get(block))
                    .and_then(|block| page.bounds(*block))
                    .map_or(0., |block| (block.y - column.y).max(0.));
                let top = beside.max(cursor);
                cursor = top + item.height + 8. * scale;
                tops.insert(slot.clone(), top);
            }
            let moved = state.tops.with_untracked(|placed| {
                placed.len() != tops.len()
                    || (tops.iter()).any(|(slot, top)| {
                        placed.get(slot).is_none_or(|was| (was - top).abs() > 0.5)
                    })
            });
            if moved || (state.reach.get_untracked() - cursor).abs() > 0.5 {
                state.tops.set(tops);
                state.reach.set(cursor);
                relayout();
            }
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
    // The dialog, built again when what it shows is other.
    .each(
        move || {
            let shown = (state.layout.get() == CommentLayout::Dialog)
                .then(|| state.target.get())
                .flatten()
                .zip(state.page.get());
            let key = shown.map(|(target, page)| match target {
                Target::Selection(chars) => format!("new {chars:?}"),
                Target::Thread(root) => {
                    let placed = (page.threads.iter()).find(|placed| placed.thread.root.id == root);
                    let shows = placed.map(|placed| {
                        (
                            placed.thread.replies.len(),
                            placed.thread.last().id.clone(),
                            placed.replying(&page.replying),
                        )
                    });
                    format!("thread {root} {shows:?}")
                }
            });
            key.into_iter().collect::<Vec<_>>()
        },
        |key| key.clone(),
        move |_| dialog(state),
    )
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
