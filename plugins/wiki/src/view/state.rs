//! What the wiki view keeps, and what is done to it: the page shown and
//! the way back, the thread in focus and those open, what is being
//! written, and the comment sent.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use horizon_ui::reactive_graph::owner::{LocalStorage, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, Update, With, WithValue};
use horizon_ui::{Callable, Callback, El, Page, el};
use horizon_wiki::Store;

use horizon_markdown::{LinkTarget, resolve_link};
use horizon_wiki::annotation::{Agent, markdown_body};
use horizon_wiki::view_model::{PlacedThread, WikiPage};

use super::render::Piece;
use super::{Wiki, styles};

const NO_SELECTION: &str = "Select text in the page to comment on it";
const UNSUPPORTED_LINK: &str = "Not a wiki page";

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
pub(super) struct Held {
    pub(super) open_url: OpenUrl,
    pub(super) on_navigate: Option<Callback<PathBuf>>,
    /// The watches on the files drawn from: a change of one reads the
    /// page again.
    pub(super) watches: RefCell<Vec<horizon_ui::guest::Watch>>,
    /// Each block's runs of text, and its element.
    pub(super) pieces: RefCell<HashMap<usize, Vec<Piece>>>,
    pub(super) blocks: RefCell<HashMap<usize, El>>,
    /// What is in the margin, in order: the block each item is beside and
    /// the item's slot. And each slot's element.
    pub(super) beside: RefCell<Vec<(usize, String)>>,
    pub(super) asides: RefCell<HashMap<String, El>>,
    /// The block to bring into view at the next layout.
    pub(super) reveal: Cell<Option<usize>>,
    /// Who comments here: the installation's owner.
    pub(super) owner: Cell<Option<uuid::Uuid>>,
}

/// What the view keeps.
#[derive(Clone, Copy)]
pub struct State {
    pub path: RwSignal<Option<PathBuf>>,
    pub(super) page: RwSignal<Option<Rc<WikiPage>>, LocalStorage>,
    pub(super) error: RwSignal<Option<String>>,
    /// The thread in focus, and the threads open.
    pub focused: RwSignal<Option<String>>,
    pub expanded: RwSignal<HashSet<String>>,
    pub target: RwSignal<Option<Target>>,
    /// The logical characters selected in the page.
    pub selection: RwSignal<Option<Range<usize>>>,
    pub notice: RwSignal<Option<String>>,
    /// The pages left by following links, most recent last.
    pub history: RwSignal<Vec<PathBuf>>,
    pub(super) typed: RwSignal<String>,
    pub layout: RwSignal<CommentLayout>,
    /// The margin's width, on a pane wide enough for one.
    pub margin: RwSignal<Option<f64>>,
    /// Where each item of the margin is from the margin's top, by slot,
    /// and how far down the items reach.
    pub(super) tops: RwSignal<HashMap<String, f64>>,
    pub(super) reach: RwSignal<f64>,
    pub(super) held: StoredValue<Rc<Held>, LocalStorage>,
    pub element: El,
    pub(super) scroll: El,
}

/// One line of the page: a block of its text, or something set after one.
impl State {
    /// A view of a wiki: its signals, its held handles, and the two
    /// elements the view is built around.
    pub(super) fn new(given: Wiki) -> Self {
        let Wiki {
            open_url,
            on_navigate,
            ..
        } = given;
        Self {
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
                on_navigate,
                open_url,
                watches: RefCell::default(),
                pieces: RefCell::default(),
                blocks: RefCell::default(),
                beside: RefCell::default(),
                asides: RefCell::default(),
                reveal: Cell::new(None),
                owner: Cell::new(None),
            })),
            element: el("div").class(styles::WIKI).attr("tabindex", "0"),
            scroll: el("div").class(styles::SCROLL),
        }
    }

    pub(super) fn held(&self) -> Rc<Held> {
        self.held.with_value(Rc::clone)
    }

    pub(super) fn store(&self) -> Option<Store> {
        (self.path.get_untracked().as_deref())
            .and_then(Path::parent)
            .map(Store::at)
    }

    pub(super) fn page_name(path: &Path) -> Option<String> {
        (path.file_stem()).map(|stem| stem.to_string_lossy().into_owned())
    }

    /// Goes to `path` from here: whoever placed the view is told.
    pub(super) fn go(&self, path: PathBuf) {
        self.show(path.clone());
        if let Some(on_navigate) = self.held().on_navigate {
            on_navigate.run(path);
        }
    }

    pub(super) fn show(&self, path: PathBuf) {
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
    pub(super) fn watch(&self) {
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
        let this = *self;
        *held.watches.borrow_mut() = (files.into_iter())
            .map(|file| horizon_ui::guest::watch(&file.to_string_lossy(), move || this.reload()))
            .collect();
    }

    /// Reads the page and its threads again.
    pub fn reload(&self) {
        let (Some(path), Some(store)) = (self.path.get_untracked(), self.store()) else {
            return;
        };
        let Some(name) = Self::page_name(&path) else {
            return;
        };
        match WikiPage::load(&store, &name) {
            Ok(page) => self.loaded(page),
            Err(error) => self.error.set(Some(error)),
        }
    }

    pub(super) fn loaded(&self, page: WikiPage) {
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

    pub(super) fn placed(&self, root: &str) -> Option<PlacedThread> {
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

    pub(super) fn in_dialog(&self) -> bool {
        self.layout.get_untracked() == CommentLayout::Dialog
    }

    pub(super) fn toggle_thread(&self, root: String) {
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

    pub(super) fn cancel(&self) {
        self.target.set(None);
        self.typed.set(String::new());
        self.element.focus();
    }

    /// Writes what is typed: a new thread on the passage, or a reply in
    /// the thread.
    pub(super) fn send(&self) {
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

    pub(super) fn follow_link(&self, target: &str) {
        let Some(current) = self.path.get_untracked() else {
            return;
        };
        match resolve_link(&current, target) {
            LinkTarget::Document(path) if path.parent() == current.parent() => {
                if path != current {
                    self.history.update(|history| history.push(current));
                    self.go(path);
                }
            }
            LinkTarget::External(url) => (self.held().open_url)(&url),
            _ => self
                .notice
                .set(Some(format!("{UNSUPPORTED_LINK}: {target}"))),
        }
    }

    pub(super) fn back(&self) {
        let mut previous = None;
        self.history.update(|history| previous = history.pop());
        if let Some(previous) = previous {
            self.go(previous);
        }
    }

    /// The logical characters the page's selection covers, read through
    /// the runs of text the blocks drew.
    pub(super) fn selected(&self, page: &Page) -> Option<Range<usize>> {
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
