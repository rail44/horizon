//! One board task's thread: its title, state and actions in a band, its
//! body and posts in a column that marks what has been shown as read,
//! and a field to reply in.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use horizon_board::{Comment, Item, read_position_advances};
use horizon_board_view::chrome;
use horizon_board_view::model::{
    self, ThreadCommand, Voice, command_for_key, fold_preview, read_through, status_text,
    step_post, task_session_id, unread_count, voice,
};
use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::owner::{LocalStorage, StoredValue, on_cleanup};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{
    Get, GetUntracked, Set, Update, With, WithUntracked, WithValue,
};
use horizon_ui::{El, dispatched, each_layout, el, prevent_default, relayout, stop_propagation};
use horizon_workspace::SessionId;
use keyboard_types::{Key, NamedKey};

use super::list::key_name;
use super::{Job, Source, follow, run, tone, write_refusal};
use crate::background::Background;
use crate::markdown::render;

/// How many cells wide a post is taken to be when deciding whether to
/// fold it: an owner's is narrower by its indent (`src/board/spec.rs`).
const MEASURE_CELLS: usize = 72;
const OWNER_MEASURE_CELLS: usize = 60;
/// The reply field's rows.
const COMPOSER_MIN_ROWS: usize = 2;
const COMPOSER_MAX_ROWS: usize = 8;

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it. The measurements are `src/board/spec.rs`'s and
// `src/board/thread.rs`'s, in the theme's scaled lengths.
pub const CSS: &str = "
    .board-thread { display: flex; flex-direction: column; height: 100%; min-width: 240px;
                    outline: none; background: var(--background); color: var(--text);
                    font-family: var(--font-family); font-size: var(--body-size);
                    line-height: var(--body-line);
        &.none > .band, &.none > .column, &.none > .composer { display: none; }
        .missing { padding: var(--u16); font-size: var(--meta-size);
                   line-height: var(--meta-line); color: var(--text-muted); }
        .band { flex: none; display: flex; flex-direction: column; gap: var(--u4);
                padding: var(--u16); background: var(--band);
                border-bottom: 1px solid var(--border); }
        .top { display: flex; flex-wrap: wrap; align-items: flex-start; gap: var(--u8); }
        .title { flex: 1; min-width: calc(var(--t1-size) * 12); overflow: hidden;
                 font-size: var(--t1-size); line-height: var(--t1-line); font-weight: 600; }
        .actions { flex: none; display: flex; align-items: center; gap: var(--u4); }
        .button { padding: 0 var(--u12); border-radius: 4px; line-height: var(--u28);
                  font-size: var(--meta-size); cursor: pointer; }
        .button.primary { background: var(--accent); color: var(--on-accent); }
        .button:hover { background: var(--surface-raised); }
        .button.primary:hover { background: var(--accent); opacity: 0.9; }
        .state { display: flex; flex-wrap: wrap; align-items: center; gap: var(--u4); }
        .updated, .time, .hint, .more, .on-hover { font-size: var(--meta-size);
                                                   line-height: var(--meta-line);
                                                   color: var(--text-muted); }
        .spacer { flex: 1; min-width: 0; }
        .status { position: relative; flex: none; width: calc(var(--u28) * 4);
                  font-size: var(--meta-size); line-height: var(--meta-line); }
        .status input, .composer textarea {
            width: 100%; box-sizing: border-box; font-family: var(--font-family);
            font-size: inherit; line-height: inherit; padding: 0; border: none;
            outline: none; resize: none; background: transparent; color: var(--text); }
        .placeholder { position: absolute; left: 0; top: 0; color: var(--text-muted);
                       pointer-events: none; }
        .chip { padding: 0 var(--u4); border-radius: 2px; font-size: var(--micro-size);
                line-height: var(--micro-line); }
        .chip.muted { background: var(--chip-muted-surface); color: var(--chip-muted-text); }
        .chip.accent { background: var(--chip-accent-surface); color: var(--chip-accent-text); }
        .chip.success { background: var(--chip-success-surface);
                        color: var(--chip-success-text); }
        .chip.info { background: var(--chip-info-surface); color: var(--chip-info-text); }
        .chip.warning { background: var(--chip-warning-surface);
                        color: var(--chip-warning-text); }
        .notice { flex: none; padding: var(--u4) var(--u24) 0 var(--u24);
                  font-size: var(--meta-size); line-height: var(--meta-line);
                  color: var(--danger); }
        .column { flex: 1; min-height: 0; overflow-y: scroll; display: flex;
                  flex-direction: column; gap: var(--u8);
                  padding: var(--u24) var(--u24) var(--u32) var(--u24); }
        .post { position: relative; flex: none; padding: var(--u8) var(--u16);
                border: 1px solid var(--border); border-radius: 4px; }
        .post.owner { margin-left: var(--u24); border-color: transparent;
                      background: var(--band); }
        .post .bar { position: absolute; left: 0; top: 0; bottom: 0; width: 2px; }
        .post.current .bar { background: var(--accent); }
        .post .head { display: flex; align-items: baseline; gap: var(--u4); }
        .author { font-weight: 600; }
        .on-hover { display: flex; gap: var(--u4); opacity: 0; cursor: pointer; }
        .post:hover .on-hover { opacity: 1; }
        .text { margin-top: var(--u4); }
        .text.plain { white-space: pre-wrap; }
        .post.system .text { color: var(--text-muted); }
        .more { margin-top: var(--u4); color: var(--accent); cursor: pointer; }
        .body > :first-child { margin-top: 0; }
        .body > :last-child { margin-bottom: 0; }
        .body p, .body ul, .body ol, .body pre, .body blockquote { margin: 0 0 8px 0; }
        .body ul, .body ol { padding-left: 18px; }
        .body h1, .body h2, .body h3 { font-size: var(--body-size);
                                       line-height: var(--body-line); font-weight: 600;
                                       margin: 12px 0 2px 0; }
        .body pre { padding: 12px; border: 1px solid var(--border); border-radius: 6px;
                    background: var(--surface-panel); overflow-x: scroll; }
        .body code { font-family: var(--font-family); }
        .link { color: var(--accent); }
        .composer { flex: none; display: flex; flex-direction: column; gap: 2px;
                    padding: var(--u8) var(--u24); border-top: 1px solid var(--border); }
        .composer .field { position: relative; }
        .send { display: flex; align-items: center; justify-content: flex-end; gap: 2px; }
        .key { padding: 0 var(--u4); border: 1px solid var(--border); border-radius: 3px;
               font-size: var(--micro-size); line-height: var(--micro-line);
               color: var(--text-muted); }
        .hint { font-size: var(--micro-size); line-height: var(--micro-line); }
    }
";

/// What the thread asks of whatever holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum Asked {
    /// The agent session bound to the task, in a pane.
    OpenSession(SessionId),
}

/// What the view keeps that is not a signal.
struct Held {
    source: Source,
    background: Background,
    ask: Box<dyn Fn(Asked)>,
    /// Each post's element on the page, by the post's id.
    posts: RefCell<HashMap<String, El>>,
    /// The post to bring into view at the next layout.
    reveal: Cell<Option<usize>>,
}

/// The view's state, for whoever placed it to read or drive.
#[derive(Clone, Copy)]
pub struct BoardThread {
    pub task: RwSignal<u64>,
    item: RwSignal<Option<Item>>,
    positions: RwSignal<HashMap<u64, String>>,
    /// The post the keyboard is on.
    pub cursor: RwSignal<Option<usize>>,
    /// The long posts opened in full.
    expanded: RwSignal<HashSet<String>>,
    pub notice: RwSignal<Option<String>>,
    typed: RwSignal<String>,
    status_typed: RwSignal<String>,
    held: StoredValue<Rc<Held>, LocalStorage>,
    pub element: El,
    reply: El,
    status: El,
    column: El,
}

type Read = (Option<Item>, HashMap<u64, String>);

impl BoardThread {
    fn held(&self) -> Rc<Held> {
        self.held.with_value(Rc::clone)
    }

    /// The directory of the project whose board this is.
    pub fn root(&self) -> Option<std::path::PathBuf> {
        self.held().source.root()
    }

    /// Reads the task again.
    pub fn load(&self) {
        let (this, held) = (*self, self.held());
        let id = self.task.get_untracked();
        let read = move |store: horizon_board::Store| -> Job<Read> {
            Box::pin(async move { Ok((store.show(id)?, store.read_positions("owner")?)) })
        };
        run(&held.background, held.source.clone(), read, move |read| {
            match read {
                Ok((item, positions)) => {
                    // (A task shown since the read began is not this one.)
                    if this.task.get_untracked() != id {
                        return;
                    }
                    let posts = item.as_ref().map_or(0, |item| item.comments.len());
                    this.item.set(item);
                    this.positions.set(positions);
                    let cursor = match (this.cursor.get_untracked(), posts) {
                        (_, 0) => None,
                        (Some(index), posts) => Some(index.min(posts - 1)),
                        (None, _) => Some(0),
                    };
                    this.cursor.set(cursor);
                }
                Err(error) => this.notice.set(Some(error.to_string())),
            }
        });
    }

    fn mutate(&self, job: impl FnOnce(horizon_board::Store) -> Job<()> + Send + 'static) {
        let (this, held) = (*self, self.held());
        run(
            &held.background,
            held.source.clone(),
            job,
            move |wrote| match wrote {
                Ok(()) => {
                    this.notice.set(None);
                    this.load();
                }
                Err(error) => this.notice.set(Some(write_refusal(&error))),
            },
        );
    }

    /// Shows another task in the same view.
    pub fn show_task(&self, task: u64) {
        if self.task.get_untracked() == task {
            return;
        }
        self.task.set(task);
        self.item.set(None);
        self.cursor.set(None);
        self.expanded.set(HashSet::new());
        self.notice.set(None);
        self.load();
    }

    /// The agent session bound to the task.
    pub fn task_session(&self) -> Option<SessionId> {
        self.item
            .with_untracked(|item| item.as_ref().and_then(task_session_id))
    }

    fn posts(&self) -> usize {
        (self.item).with_untracked(|item| item.as_ref().map_or(0, |item| item.comments.len()))
    }

    /// A post shown is a post read: the owner's read position moves to it
    /// if it is further on.
    fn mark_displayed(&self, message: String) {
        let advanced = self.item.with_untracked(|item| {
            let item = item.as_ref()?;
            let current = self
                .positions
                .with_untracked(|positions| positions.get(&item.id).cloned());
            read_position_advances(item, current.as_deref(), &message).then_some(item.id)
        });
        let Some(id) = advanced else {
            return;
        };
        self.positions.update(|positions| {
            positions.insert(id, message.clone());
        });
        let held = self.held();
        let mark = move |store: horizon_board::Store| -> Job<()> {
            Box::pin(async move { store.mark_read(id, "owner", &message).await })
        };
        run(&held.background, held.source.clone(), mark, |_| {});
    }

    pub fn execute(&self, command: ThreadCommand) {
        match command {
            ThreadCommand::NextPost | ThreadCommand::PreviousPost => {
                let forward = command == ThreadCommand::NextPost;
                let cursor = self.cursor.get_untracked();
                let next = step_post(self.posts(), cursor, forward);
                if next != cursor {
                    self.cursor.set(next);
                    self.held().reveal.set(next);
                }
            }
            ThreadCommand::ToggleCurrentPost => {
                let id = self.item.with_untracked(|item| {
                    let index = self.cursor.get_untracked()?;
                    Some(item.as_ref()?.comments.get(index)?.id.clone())
                });
                if let Some(id) = id {
                    self.execute(ThreadCommand::TogglePost(id));
                }
            }
            ThreadCommand::TogglePost(id) => self.expanded.update(|expanded| {
                if !expanded.remove(&id) {
                    expanded.insert(id);
                }
            }),
            ThreadCommand::SelectPost(index) => {
                if index < self.posts() {
                    self.cursor.set(Some(index));
                    self.element.focus();
                }
            }
            ThreadCommand::FocusComposer => {
                self.reply.focus();
            }
            ThreadCommand::LeaveComposer => {
                self.element.focus();
            }
            ThreadCommand::ToggleClosed => {
                let state = (self.item)
                    .with_untracked(|item| item.as_ref().map(|item| (item.id, !item.is_closed)));
                if let Some((id, closed)) = state {
                    self.mutate(move |store| {
                        Box::pin(async move { store.set_closed(id, closed, None).await })
                    });
                }
            }
            ThreadCommand::SaveStatus => {
                let id = self.task.get_untracked();
                let status = self.status_typed.get_untracked().trim().to_string();
                if status.is_empty() {
                    return;
                }
                self.status_typed.set(String::new());
                self.status.set_value("");
                self.mutate(move |store| {
                    Box::pin(async move { store.set_status(id, &status).await })
                });
            }
            ThreadCommand::OpenTaskSession => {
                if let Some(session) = self.task_session() {
                    (self.held().ask)(Asked::OpenSession(session));
                }
            }
            ThreadCommand::PostMessage => {
                let id = self.task.get_untracked();
                let text = self.typed.get_untracked().trim().to_string();
                if text.is_empty() {
                    return;
                }
                self.typed.set(String::new());
                self.reply.set_value("");
                self.mutate(move |store| {
                    Box::pin(async move { store.comment(id, "owner", &text).await })
                });
            }
        }
    }
}

fn author_label(author: &str) -> String {
    match voice(author) {
        Voice::Owner => chrome::OWNER.to_string(),
        Voice::System => chrome::SYSTEM.to_string(),
        Voice::Agent if author == "agent" => chrome::AGENT.to_string(),
        Voice::Agent => author.to_string(),
    }
}

fn chip(tone: &str, text: &str) -> El {
    el("span").class(&format!("chip {tone}")).text(text)
}

fn button(class: &str, label: &str, state: BoardThread, command: ThreadCommand) -> El {
    el("div")
        .class(class)
        .text(label)
        .on("click", move |_| state.execute(command.clone()))
}

/// The band's parts that follow the task: built again when it changes.
fn band(state: BoardThread, item: &Item, unread: usize) -> El {
    let actions = el("div")
        .class("actions")
        .child(button(
            "button primary",
            chrome::REPLY,
            state,
            ThreadCommand::FocusComposer,
        ))
        .child(button(
            "button",
            if item.is_closed {
                chrome::REOPEN
            } else {
                chrome::CLOSE
            },
            state,
            ThreadCommand::ToggleClosed,
        ));
    if item.session_id.is_some() {
        actions.child(button(
            "button",
            chrome::SESSION,
            state,
            ThreadCommand::OpenTaskSession,
        ));
    }
    let line = el("div").class("state");
    let status = status_text(item);
    if !status.is_empty() {
        line.child(chip(tone(item), &status));
    }
    if unread > 0 {
        line.child(chip("accent", &chrome::unread_count(unread)));
    }
    if let Some(at) = item.comments.last().and_then(|comment| comment.at) {
        let updated = chrome::updated(&chrome::relative_time(at, model::now_ms()));
        line.child(el("span").class("updated").text(&updated));
    }
    el("div")
        .class("task")
        // The actions go under the title when the band is too narrow for
        // both.
        .child(
            el("div")
                .class("top")
                .child(el("div").class("title").text(&item.title))
                .child(actions),
        )
        .child(line)
}

/// One post. What it shows of its own state (unread, folded) is fixed
/// when it is built: a post is built again when that changes.
fn post(state: BoardThread, index: usize, comment: &Comment, unread: bool, expanded: bool) -> El {
    let spoken = voice(&comment.author);
    let cells = match spoken {
        Voice::Owner => OWNER_MEASURE_CELLS,
        _ => MEASURE_CELLS,
    };
    // A long post that has been read is folded to its opening lines.
    let preview = (!unread)
        .then(|| fold_preview(&comment.text, cells))
        .flatten();
    let foldable = preview.is_some();
    let fold = preview.filter(|_| !expanded);
    let shown = fold
        .as_ref()
        .map_or(comment.text.as_str(), |fold| fold.head.as_str());
    // An agent's post is Markdown; a person's, and the system's, is the
    // text as it was written.
    let text = match spoken {
        Voice::Agent => el("div")
            .class("text")
            .child(render(shown, Rc::new(|_| {}))),
        _ => el("div").class("text plain").text(shown),
    };
    let fixed = match spoken {
        Voice::Owner => "post owner",
        Voice::System => "post system",
        Voice::Agent => "post",
    };
    let head = el("div").class("head").child(
        el("span")
            .class("author")
            .text(&author_label(&comment.author)),
    );
    if let Some(at) = comment.at {
        let age = chrome::relative_time(at, model::now_ms());
        head.child(el("span").class("time").text(&age));
    }
    head.child(el("div").class("spacer"));
    let on_hover =
        el("div")
            .class("on-hover")
            .child(el("span").text(chrome::REPLY).on("click", move |_| {
                state.execute(ThreadCommand::FocusComposer)
            }));
    if foldable && expanded {
        let id = comment.id.clone();
        on_hover.child(el("span").text(chrome::COLLAPSE).on("click", move |_| {
            state.execute(ThreadCommand::TogglePost(id.clone()));
        }));
    }
    head.child(on_hover);
    if unread {
        head.child(chip("accent", chrome::UNREAD));
    }
    let element = el("div")
        .class_dyn(move || match state.cursor.get() == Some(index) {
            true => format!("{fixed} current"),
            false => fixed.to_string(),
        })
        .child(el("div").class("bar"))
        .child(head)
        .child(text)
        .on("click", move |_| {
            state.execute(ThreadCommand::SelectPost(index))
        });
    if let Some(fold) = fold {
        let id = comment.id.clone();
        element.child(
            el("div")
                .class("more")
                .text(&chrome::more_lines(fold.hidden_lines))
                .on("click", move |_| {
                    state.execute(ThreadCommand::TogglePost(id.clone()));
                    stop_propagation();
                }),
        );
    }
    element
}

/// Builds the thread of `task` over `source`'s store, read at once and
/// again whenever its log takes a write; `ask` hears what it asks for.
pub fn view(
    source: Source,
    background: Background,
    task: u64,
    ask: impl Fn(Asked) + 'static,
) -> (El, BoardThread) {
    let state = BoardThread {
        task: RwSignal::new(task),
        item: RwSignal::new(None),
        positions: RwSignal::new(HashMap::new()),
        cursor: RwSignal::new(None),
        expanded: RwSignal::new(HashSet::new()),
        notice: RwSignal::new(None),
        typed: RwSignal::new(String::new()),
        status_typed: RwSignal::new(String::new()),
        held: StoredValue::new_local(Rc::new(Held {
            source: source.clone(),
            background: background.clone(),
            ask: Box::new(ask),
            posts: RefCell::default(),
            reveal: Cell::new(None),
        })),
        element: el("div").attr("tabindex", "0"),
        reply: el("textarea"),
        status: el("input").attr("type", "text"),
        column: el("div").class("column"),
    };
    let (root, column, reply, status) = (state.element, state.column, state.reply, state.status);
    let held = state.held();
    state.load();
    if let Source::Root(root) = source {
        let stop = follow(root, background, move || state.load());
        on_cleanup(move || drop(stop));
    }
    let found = move || state.item.with(Option::is_some);

    // The band: what follows the task, then the field for a new status.
    let task_band = el("div")
        .class("band")
        .each(
            move || {
                let item = state.item.get();
                let unread = item.as_ref().map_or(0, |item| {
                    state
                        .positions
                        .with(|positions| unread_count(item, positions))
                });
                item.map(|item| (item, unread))
                    .into_iter()
                    .collect::<Vec<_>>()
            },
            |(item, unread)| {
                let updated = item.comments.last().map(|comment| comment.id.clone());
                (
                    item.title.clone(),
                    item.status.clone(),
                    item.is_closed,
                    item.session_id.is_some(),
                    updated,
                    *unread,
                )
            },
            move |(item, unread)| band(state, item, *unread),
        )
        .child(
            // blitz-dom does not draw a field's `placeholder`.
            el("div")
                .class("status")
                .child(
                    status
                        .on("input", move |event| {
                            if let DomEventData::Input(input) = &event.data {
                                state.status_typed.set(input.value.clone());
                            }
                        })
                        .on("keydown", move |event| {
                            let DomEventData::KeyDown(key) = &event.data else {
                                return;
                            };
                            match &key.key {
                                Key::Named(NamedKey::Enter) => {
                                    state.execute(ThreadCommand::SaveStatus);
                                    prevent_default();
                                }
                                Key::Named(NamedKey::Escape) => {
                                    state.execute(ThreadCommand::LeaveComposer);
                                }
                                _ => {}
                            }
                            // The field's keys are its own.
                            stop_propagation();
                        }),
                )
                .when(
                    move || state.status_typed.with(String::is_empty),
                    || el("div").class("placeholder").text(chrome::CHANGE_STATUS),
                ),
        );

    // The column: the task's body, then its posts.
    let known = held.clone();
    column
        .each(
            move || {
                let body = state.item.with(|item| {
                    let body = item.as_ref().map(|item| item.body.clone())?;
                    (!body.trim().is_empty()).then_some(body)
                });
                body.into_iter().collect::<Vec<_>>()
            },
            |body| body.clone(),
            |body| el("div").class("text").child(render(body, Rc::new(|_| {}))),
        )
        .each(
            move || {
                let Some(item) = state.item.get() else {
                    return Vec::new();
                };
                let read = state
                    .positions
                    .with(|positions| read_through(&item, positions));
                let expanded = state.expanded.get();
                (item.comments.into_iter().enumerate())
                    .map(|(index, comment)| {
                        let open = expanded.contains(&comment.id);
                        (index, comment, index >= read, open)
                    })
                    .collect::<Vec<_>>()
            },
            |(index, comment, unread, expanded)| {
                (
                    *index,
                    comment.id.clone(),
                    comment.text.clone(),
                    *unread,
                    *expanded,
                )
            },
            move |(index, comment, unread, expanded)| {
                let element = post(state, *index, comment, *unread, *expanded);
                (known.posts.borrow_mut()).insert(comment.id.clone(), element);
                element
            },
        );

    let rows = RwSignal::new(COMPOSER_MIN_ROWS);
    let composer = el("div")
        .class("composer")
        .child(
            el("div")
                .class("field")
                .child(
                    reply
                        .attr("rows", "2")
                        .attr_dyn("style", move || {
                            format!("height: calc(var(--body-line) * {})", rows.get())
                        })
                        .on("input", move |event| {
                            if let DomEventData::Input(input) = &event.data {
                                state.typed.set(input.value.clone());
                            }
                            // As tall as its lines, within its bounds.
                            dispatched(move |page| {
                                if let Some(lines) = page.lines(reply) {
                                    rows.set(lines.clamp(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS));
                                }
                            });
                        })
                        .on("keydown", move |event| {
                            let DomEventData::KeyDown(key) = &event.data else {
                                return;
                            };
                            match &key.key {
                                Key::Named(NamedKey::Enter)
                                    if !key.modifiers.shift() && !key.is_composing =>
                                {
                                    state.execute(ThreadCommand::PostMessage);
                                    rows.set(COMPOSER_MIN_ROWS);
                                    prevent_default();
                                }
                                Key::Named(NamedKey::Escape) => {
                                    state.execute(ThreadCommand::LeaveComposer);
                                }
                                _ => {}
                            }
                            stop_propagation();
                        }),
                )
                .when(
                    move || state.typed.with(String::is_empty),
                    || el("div").class("placeholder").text(chrome::WRITE_REPLY),
                ),
        )
        .child(
            el("div")
                .class("send")
                .child(el("span").class("key").text("Enter"))
                .child(el("span").class("hint").text(chrome::TO_SEND)),
        );

    // After each layout: the answers the store gave, the post the
    // keyboard moved to brought into view, and the posts in view read.
    let laid = held.clone();
    each_layout(move |page| {
        if laid.background.take_in() {
            relayout();
        }
        // (Not drawn, the column's box is whatever it last was.)
        if state.item.with_untracked(Option::is_none) {
            return;
        }
        let Some(view) = page.bounds(column) else {
            return;
        };
        let posts: Vec<(String, El)> = state.item.with_untracked(|item| {
            let ids = item.iter().flat_map(|item| &item.comments);
            let known = laid.posts.borrow();
            ids.filter_map(|comment| Some((comment.id.clone(), *known.get(&comment.id)?)))
                .collect()
        });
        if let Some(index) = laid.reveal.take()
            && let Some(bounds) = posts.get(index).and_then(|(_, post)| page.bounds(*post))
        {
            let above = bounds.y - view.y;
            let below = (bounds.y + bounds.height) - (view.y + view.height);
            if above < 0. {
                column.scroll_by(0., above);
            } else if below > 0. {
                column.scroll_by(0., below);
            }
        }
        let read = state.item.with_untracked(|item| {
            let item = item.as_ref()?;
            Some(
                state
                    .positions
                    .with_untracked(|positions| read_through(item, positions)),
            )
        });
        let Some(read) = read else {
            return;
        };
        // The last unread post any part of which is in view.
        let shown = (posts.iter().enumerate().skip(read)).rfind(|(_, (_, post))| {
            page.bounds(*post).is_some_and(|bounds| {
                bounds.y < view.y + view.height && bounds.y + bounds.height > view.y
            })
        });
        if let Some((_, (id, _))) = shown {
            state.mark_displayed(id.clone());
            relayout();
        }
    });

    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        if key.modifiers.ctrl() || key.modifiers.alt() || key.modifiers.meta() {
            return;
        }
        if let Some(command) = key_name(key).and_then(|name| command_for_key(&name)) {
            state.execute(command);
            prevent_default();
        }
    })
    // The band, the column and the field are built once and stay: with no
    // task to show they are only not drawn.
    .class_dyn(move || match found() {
        true => "board-thread".to_string(),
        false => "board-thread none".to_string(),
    })
    .when(
        move || !found(),
        || el("div").class("missing").text(chrome::TASK_NOT_FOUND),
    )
    .child(task_band)
    .when(
        move || found() && state.notice.with(Option::is_some),
        move || {
            el("div")
                .class("notice")
                .text_dyn(move || state.notice.get().unwrap_or_default())
        },
    )
    .child(column)
    .child(composer);
    (root, state)
}
