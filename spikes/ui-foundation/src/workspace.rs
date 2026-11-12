//! The shell around the views, on this stack: tabs, panes split across and
//! down with a boundary to drag, a mode the keyboard enters, and a palette
//! of commands over everything (`src/workspace/`, `src/palette.rs`).
//!
//! A pane's element is made once and lives until the pane is closed.
//! Splitting, closing or resizing changes only the boxes around it: the
//! pane's element is taken out of the old arrangement and put into the new
//! one, with whatever its view holds (typed text, a scroll position).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use blitz_traits::events::DomEventData;
use keyboard_types::{Key, NamedKey};
use reactive_graph::effect::Effect;
use reactive_graph::owner::{LocalStorage, Owner, StoredValue};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, GetUntracked, Set, Update, WithValue};

use crate::agent::sample::Session;
use crate::board::Task;
use crate::pty::Session as Shell;
use crate::terminal::{self, Terminal};
use crate::theme::TerminalLook;
use crate::{El, agent, board, dispatched, el, markdown, prevent_default, stop_propagation};
use horizon_terminal_core::TerminalSize;

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it.
pub const CSS: &str = "
    .workspace { position: relative; display: flex; flex-direction: column; height: 100%;
                 outline: none; background: var(--background); color: var(--text);
                 font-family: var(--font-family); font-size: var(--body-size);
                 line-height: var(--body-line);
        &.resizing { user-select: none; }
        .tabs { flex: none; display: flex; gap: 4px; padding: 4px 28px 4px 8px;
                background: var(--tab-track); border-top: 1px solid var(--border);
                border-bottom: 1px solid var(--border); }
        .tab-label { flex: 1; min-width: 0; padding: 0 8px; border-radius: 6px;
                     line-height: 24px; text-align: center; overflow: hidden;
                     white-space: nowrap; text-overflow: ellipsis;
                     color: var(--tab-text); cursor: pointer; }
        .tab-label.active { background: var(--background); color: var(--text); }
        .mode { display: none; }
        .tab { flex: 1; min-height: 0; display: flex; }
        .tab.hidden { display: none; }
        .split { flex: 1; min-width: 0; min-height: 0; display: flex; }
        .split.down { flex-direction: column; }
        .cell { flex-shrink: 1; flex-basis: 0; min-width: 0; min-height: 0; display: flex; }
        .divider { flex: none; background: var(--surface-chrome); }
        .split.across > .divider { width: 5px; cursor: col-resize; }
        .split.down > .divider { height: 5px; cursor: row-resize; }
        .pane { position: relative; flex: 1; min-width: 0; min-height: 0;
                box-sizing: border-box; display: flex; flex-direction: column;
                overflow: hidden; border: 2px solid var(--background);
                background: var(--background); }
        .pane.focused { border-color: var(--border); }
        .pane.cursor { border-color: var(--accent); }
        .pane > * { flex: 1; min-height: 0; }
        .pane > .veil { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 50;
                        background: var(--background); opacity: 0.5; pointer-events: none; }
        .empty { display: flex; align-items: center; justify-content: center;
                 color: var(--text-muted); outline: none; }
        .scrim { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 100;
                 display: flex; justify-content: center; align-items: flex-start;
                 padding-top: 97px; }
        .palette { width: 560px; display: flex; flex-direction: column; overflow: hidden;
                   background: var(--background); border: 1px solid var(--border);
                   border-radius: 8px;
                   box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18), 0 2px 8px rgba(0, 0, 0, 0.1); }
        .search { position: relative; display: flex; align-items: center; height: 32px;
                  padding: 0 12px; border-bottom: 1px solid var(--border); }
        .search input { flex: 1; font-family: var(--font-family);
                        font-size: var(--body-size); line-height: 20px; padding: 0;
                        border: none; outline: none; background: transparent;
                        color: var(--text); }
        .search .placeholder { position: absolute; left: 12px; top: 6px; line-height: 20px;
                               color: var(--text-muted); pointer-events: none; }
        .item { display: flex; flex-direction: column; padding: 7px 12px; cursor: pointer; }
        .item .name { line-height: 20px; }
        .item .about { font-size: var(--meta-size); line-height: var(--meta-line); }
        .item.chosen { background: var(--surface-selected); }
    }
";

/// The page's style sheet with this view and the views its panes show.
pub fn css() -> String {
    [CSS, agent::CSS, markdown::CSS, board::CSS, terminal::CSS].concat()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Empty,
    Agent,
    Markdown,
    Terminal,
    Board,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    NewTab,
    NextTab,
    SplitRight,
    SplitDown,
    ClosePane,
    Show(Kind),
}

impl Command {
    const ALL: [Command; 9] = [
        Command::NewTab,
        Command::NextTab,
        Command::SplitRight,
        Command::SplitDown,
        Command::ClosePane,
        Command::Show(Kind::Agent),
        Command::Show(Kind::Markdown),
        Command::Show(Kind::Terminal),
        Command::Show(Kind::Board),
    ];

    fn label(self) -> &'static str {
        match self {
            Command::NewTab => "New Tab",
            Command::NextTab => "Next Tab",
            Command::SplitRight => "Split Right",
            Command::SplitDown => "Split Down",
            Command::ClosePane => "Close Pane",
            Command::Show(Kind::Agent) => "Show Agent",
            Command::Show(Kind::Markdown) => "Show Markdown",
            Command::Show(Kind::Terminal) => "Show Terminal",
            Command::Show(Kind::Board) => "Show Board",
            Command::Show(Kind::Empty) => "Show Nothing",
        }
    }

    /// What the command does, under its name in the palette.
    fn about(self) -> &'static str {
        match self {
            Command::NewTab => "Open a new tab with an empty pane.",
            Command::NextTab => "Show the next tab.",
            Command::SplitRight => "Split the active pane horizontally.",
            Command::SplitDown => "Split the active pane vertically.",
            Command::ClosePane => "Close the active pane when another pane remains.",
            Command::Show(Kind::Empty) => "Show nothing in the active pane.",
            Command::Show(_) => "Show this view in the active pane.",
        }
    }

    fn matching(query: &str) -> Vec<Command> {
        let query = query.to_lowercase();
        (Command::ALL.into_iter())
            .filter(|command| command.label().to_lowercase().contains(&query))
            .collect()
    }
}

/// How a tab's panes are arranged.
#[derive(Clone)]
enum Tree {
    Pane(u32),
    Split {
        across: bool,
        /// The first side's share of the split.
        ratio: RwSignal<f32>,
        first: Box<Tree>,
        second: Box<Tree>,
    },
}

impl Tree {
    fn panes(&self) -> Vec<u32> {
        match self {
            Tree::Pane(id) => vec![*id],
            Tree::Split { first, second, .. } => [first.panes(), second.panes()].concat(),
        }
    }

    /// `pane` becomes a split of itself and `new`.
    fn split(&mut self, pane: u32, across: bool, new: u32) {
        match self {
            Tree::Pane(id) if *id == pane => {
                *self = Tree::Split {
                    across,
                    ratio: RwSignal::new(0.5),
                    first: Box::new(Tree::Pane(pane)),
                    second: Box::new(Tree::Pane(new)),
                };
            }
            Tree::Pane(_) => {}
            Tree::Split { first, second, .. } => {
                first.split(pane, across, new);
                second.split(pane, across, new);
            }
        }
    }

    /// Takes `pane` out: the other side of its split takes the split's
    /// place. A tree that is `pane` alone stays as it is.
    fn close(&mut self, pane: u32) {
        let Tree::Split { first, second, .. } = self else {
            return;
        };
        let replacement = match (first.as_ref(), second.as_ref()) {
            (Tree::Pane(id), other) | (other, Tree::Pane(id)) if *id == pane => other.clone(),
            _ => {
                first.close(pane);
                second.close(pane);
                return;
            }
        };
        *self = replacement;
    }

    fn describe(&self, kinds: &dyn Fn(u32) -> Kind) -> String {
        match self {
            Tree::Pane(id) => format!("{:?}", kinds(*id)),
            Tree::Split {
                across,
                ratio,
                first,
                second,
            } => format!(
                "{} {:.2} [{}, {}]",
                if *across { "across" } else { "down" },
                ratio.get_untracked(),
                first.describe(kinds),
                second.describe(kinds)
            ),
        }
    }
}

struct Pane {
    element: El,
    kind: RwSignal<Kind>,
    /// What the pane's view made: effects that end with the pane.
    owner: Owner,
}

type Panes = Rc<RefCell<HashMap<u32, Pane>>>;

#[derive(Clone, Copy)]
pub struct Workspace {
    tabs: RwSignal<Vec<(u32, RwSignal<Tree>)>>,
    pub active: RwSignal<u32>,
    /// The pane keys go to.
    pub focused: RwSignal<u32>,
    /// Workspace mode: keys are the shell's, not the focused view's.
    pub mode: RwSignal<bool>,
    pub palette: RwSignal<bool>,
    query: RwSignal<String>,
    chosen: RwSignal<usize>,
    /// The split whose boundary is being dragged.
    resizing: RwSignal<Option<(El, bool, RwSignal<f32>)>>,
    next_id: RwSignal<u32>,
    panes: StoredValue<Panes, LocalStorage>,
    owner: StoredValue<Owner, LocalStorage>,
    look: StoredValue<TerminalLook, LocalStorage>,
    /// The terminals in the panes, and who they wake when a frame comes.
    terminals: StoredValue<Rc<RefCell<Vec<Terminal>>>, LocalStorage>,
    waker: StoredValue<Rc<RefCell<Option<std::task::Waker>>>, LocalStorage>,
    root: El,
}

impl Workspace {
    fn id(&self) -> u32 {
        self.next_id.update(|id| *id += 1);
        self.next_id.get_untracked()
    }

    fn pane(&self, kind: Kind) -> u32 {
        let id = self.id();
        let workspace = *self;
        let owner = self.owner.with_value(|owner| owner.child());
        let kind = RwSignal::new(kind);
        let element = owner.with(|| {
            el("div")
                // The focused pane's border is the theme's quiet line; in
                // workspace mode it is the accent, and every pane is veiled
                // (`src/workspace/render.rs`'s `pane_border_role`).
                .class_dyn(
                    move || match (workspace.focused.get() == id, workspace.mode.get()) {
                        (true, true) => "pane cursor".to_string(),
                        (true, false) => "pane focused".to_string(),
                        (false, _) => "pane".to_string(),
                    },
                )
                .on("pointerdown", move |_| {
                    if workspace.focused.get_untracked() != id {
                        workspace.focused.set(id);
                    }
                })
                .each(
                    move || vec![kind.get()],
                    |kind| *kind,
                    move |kind| content(*kind, workspace),
                )
                .when(move || workspace.mode.get(), || el("div").class("veil"))
        });
        self.panes.with_value(|panes| {
            panes.borrow_mut().insert(
                id,
                Pane {
                    element,
                    kind,
                    owner,
                },
            );
        });
        id
    }

    fn tab_of(&self, pane: u32) -> Option<RwSignal<Tree>> {
        (self.tabs.get_untracked().into_iter())
            .find(|(_, tree)| tree.get_untracked().panes().contains(&pane))
            .map(|(_, tree)| tree)
    }

    pub fn execute(&self, command: Command) {
        let focused = self.focused.get_untracked();
        match command {
            Command::NewTab => {
                let pane = self.pane(Kind::Empty);
                let id = self.id();
                self.tabs
                    .update(|tabs| tabs.push((id, RwSignal::new(Tree::Pane(pane)))));
                self.active.set(id);
                self.focused.set(pane);
            }
            Command::NextTab => {
                let tabs = self.tabs.get_untracked();
                let active = self.active.get_untracked();
                let at = tabs.iter().position(|(id, _)| *id == active).unwrap_or(0);
                let (id, tree) = tabs[(at + 1) % tabs.len()];
                self.show_tab(id, tree);
            }
            Command::SplitRight | Command::SplitDown => {
                let Some(tree) = self.tab_of(focused) else {
                    return;
                };
                let pane = self.pane(Kind::Empty);
                tree.update(|tree| tree.split(focused, command == Command::SplitRight, pane));
                self.focused.set(pane);
            }
            Command::ClosePane => {
                let Some(tree) = self.tab_of(focused) else {
                    return;
                };
                if tree.get_untracked().panes().len() == 1 {
                    // A tab's last pane stays, showing nothing.
                    self.execute(Command::Show(Kind::Empty));
                    return;
                }
                tree.update(|tree| tree.close(focused));
                self.focused.set(tree.get_untracked().panes()[0]);
                let closed = self
                    .panes
                    .with_value(|panes| panes.borrow_mut().remove(&focused));
                if let Some(pane) = closed {
                    pane.owner.cleanup();
                    // Once the arrangement it was in has let go of it.
                    pane.element.remove();
                }
            }
            Command::Show(kind) => {
                let shown = self
                    .panes
                    .with_value(|panes| panes.borrow().get(&focused).map(|pane| pane.kind));
                if let Some(shown) = shown {
                    shown.set(kind);
                }
            }
        }
    }

    fn show_tab(&self, id: u32, tree: RwSignal<Tree>) {
        self.active.set(id);
        self.focused.set(tree.get_untracked().panes()[0]);
    }

    /// Who the panes' terminals wake when their shells have printed.
    pub fn set_waker(&self, waker: std::task::Waker) {
        self.terminals.with_value(|terminals| {
            for terminal in terminals.borrow().iter() {
                terminal.set_waker(waker.clone());
            }
        });
        self.waker
            .with_value(|kept| *kept.borrow_mut() = Some(waker));
    }

    /// The screen of the terminal made last, as text.
    pub fn terminal_text(&self) -> Option<String> {
        (self.terminals).with_value(|terminals| terminals.borrow().last().map(Terminal::text))
    }

    /// The active tab's arrangement, in words.
    pub fn describe(&self) -> String {
        let active = self.active.get_untracked();
        let panes = self.panes.with_value(Rc::clone);
        let kind = move |id: u32| panes.borrow()[&id].kind.get_untracked();
        (self.tabs.get_untracked().into_iter())
            .find(|(id, _)| *id == active)
            .map(|(_, tree)| tree.get_untracked().describe(&kind))
            .unwrap_or_default()
    }

    /// Gives the keys to the focused pane's view: the first thing in it
    /// that takes focus.
    fn focus_pane(&self) {
        let pane = self.focused.get_untracked();
        let element = self
            .panes
            .with_value(|panes| panes.borrow().get(&pane).map(|pane| pane.element));
        if let Some(element) = element {
            element.focus_within();
        }
    }
}

fn content(kind: Kind, workspace: Workspace) -> El {
    match kind {
        Kind::Empty => el("div")
            .class("empty")
            .attr("tabindex", "0")
            .text("Empty pane"),
        Kind::Agent => {
            let (root, state) = agent::view(|_| {});
            let mut session = Session::long(12);
            session.begin_turn("clippy の警告も直してください");
            session.stream("承知しました。まず `cargo clippy --workspace` の出力を確認します。");
            session.show(&state);
            root
        }
        Kind::Markdown => {
            let path = PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/ux-principles.md"
            ));
            markdown::view(Some(path), Rc::new(|_| {})).0
        }
        Kind::Terminal => match Shell::spawn("sh", &[], TerminalSize::new(80, 24)) {
            Ok(session) => {
                let look = workspace.look.with_value(Clone::clone);
                let (root, terminal) = terminal::view(session, look);
                let waker = workspace.waker.with_value(|waker| waker.borrow().clone());
                if let Some(waker) = waker {
                    terminal.set_waker(waker);
                }
                (workspace.terminals).with_value(|terminals| terminals.borrow_mut().push(terminal));
                root
            }
            Err(error) => el("div").class("empty").text(&error.to_string()),
        },
        Kind::Board => {
            let tasks = (1..=8)
                .map(|id| Task {
                    id,
                    title: format!("タスク {id}"),
                    status: if id % 3 == 0 { "review" } else { "open" }.to_string(),
                })
                .collect();
            board::view(tasks).0
        }
    }
}

/// One tab's panes in their arrangement, built again around the same pane
/// elements whenever the arrangement changes.
fn tab(workspace: Workspace, id: u32, tree: RwSignal<Tree>) -> El {
    let root = el("div").class_dyn(move || {
        if workspace.active.get() == id {
            "tab".to_string()
        } else {
            "tab hidden".to_string()
        }
    });
    // The outermost box of the last arrangement, when it was a split, and
    // the panes that were in it.
    Effect::new(move |previous: Option<(Option<El>, Vec<El>)>| {
        let tree = tree.get();
        let (split, panes) = previous.unwrap_or_default();
        for pane in panes {
            pane.detach();
        }
        // With the boxes inside it.
        if let Some(split) = split {
            split.remove();
        }
        let top = arrange(&tree, workspace);
        root.child(top);
        let panes: Vec<El> = workspace.panes.with_value(|panes| {
            let panes = panes.borrow();
            tree.panes().iter().map(|id| panes[id].element).collect()
        });
        ((!panes.contains(&top)).then_some(top), panes)
    });
    root
}

fn arrange(tree: &Tree, workspace: Workspace) -> El {
    match tree {
        Tree::Pane(id) => workspace
            .panes
            .with_value(|panes| panes.borrow()[id].element),
        Tree::Split {
            across,
            ratio,
            first,
            second,
        } => {
            let (across, ratio) = (*across, *ratio);
            let split = el("div").class(if across { "split across" } else { "split down" });
            let cell = |share: fn(f32) -> f32, tree: &Tree| {
                el("div")
                    .class("cell")
                    .style_dyn("flex-grow", move || share(ratio.get()).to_string())
                    .child(arrange(tree, workspace))
            };
            let first = cell(|ratio| ratio, first);
            let second = cell(|ratio| 1. - ratio, second);
            let divider = el("div").class("divider").on("pointerdown", move |_| {
                workspace.resizing.set(Some((split, across, ratio)));
                // Not the start of a selection in the text around it.
                prevent_default();
            });
            split.child(first).child(divider).child(second)
        }
    }
}

fn palette(workspace: Workspace) -> El {
    // Workspace mode stays on under the palette, and ends with it.
    let close = move || {
        workspace.palette.set(false);
        workspace.mode.set(false);
        workspace.focus_pane();
    };
    let run = move |command: Command| {
        workspace.palette.set(false);
        workspace.mode.set(false);
        workspace.execute(command);
        workspace.focus_pane();
    };
    let input = el("input")
        .attr("type", "text")
        .on("input", move |event| {
            if let DomEventData::Input(input) = &event.data {
                workspace.query.set(input.value.clone());
                workspace.chosen.set(0);
            }
        })
        .focus();
    let items = el("div").each(
        move || {
            (Command::matching(&workspace.query.get())
                .into_iter()
                .enumerate())
            .collect::<Vec<_>>()
        },
        |(index, command)| (*index, command.label()),
        move |(index, command)| {
            let (index, command) = (*index, *command);
            el("div")
                .class_dyn(move || {
                    if workspace.chosen.get() == index {
                        "item chosen".to_string()
                    } else {
                        "item".to_string()
                    }
                })
                .child(el("div").class("name").text(command.label()))
                .child(el("div").class("about").text(command.about()))
                .on("click", move |_| run(command))
        },
    );
    el("div").class("scrim").child(
        el("div")
            .class("palette")
            .on("keydown", move |event| {
                let DomEventData::KeyDown(key) = &event.data else {
                    return;
                };
                let matching = Command::matching(&workspace.query.get_untracked());
                let chosen = workspace.chosen.get_untracked();
                match &key.key {
                    Key::Named(NamedKey::ArrowDown) => {
                        (workspace.chosen).set((chosen + 1).min(matching.len().saturating_sub(1)));
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        workspace.chosen.set(chosen.saturating_sub(1));
                    }
                    Key::Named(NamedKey::Enter) => match matching.get(chosen) {
                        Some(command) => run(*command),
                        None => close(),
                    },
                    Key::Named(NamedKey::Escape) => close(),
                    // Anything else is the field's.
                    _ => return,
                }
                prevent_default();
                stop_propagation();
            })
            .child(
                // blitz-dom does not draw a field's `placeholder`.
                el("div").class("search").child(input).when(
                    move || workspace.query.get().is_empty(),
                    || el("div").class("placeholder").text("Search..."),
                ),
            )
            .child(items),
    )
}

/// Builds the shell; its terminals are drawn in `look`.
pub fn view(look: TerminalLook) -> (El, Workspace) {
    let workspace = Workspace {
        look: StoredValue::new_local(look),
        tabs: RwSignal::new(Vec::new()),
        active: RwSignal::new(0),
        focused: RwSignal::new(0),
        mode: RwSignal::new(false),
        palette: RwSignal::new(false),
        query: RwSignal::new(String::new()),
        chosen: RwSignal::new(0),
        resizing: RwSignal::new(None),
        next_id: RwSignal::new(0),
        panes: StoredValue::new_local(Rc::default()),
        owner: StoredValue::new_local(Owner::current().expect("an owner")),
        terminals: StoredValue::new_local(Rc::default()),
        waker: StoredValue::new_local(Rc::default()),
        root: el("div"),
    };
    let root = workspace.root;

    // What it opens with: an agent beside a document over a board, and a
    // terminal in a tab of its own.
    let (agent, document, board, terminal) = (
        workspace.pane(Kind::Agent),
        workspace.pane(Kind::Markdown),
        workspace.pane(Kind::Board),
        workspace.pane(Kind::Terminal),
    );
    let first = Tree::Split {
        across: true,
        ratio: RwSignal::new(0.5),
        first: Box::new(Tree::Pane(agent)),
        second: Box::new(Tree::Split {
            across: false,
            ratio: RwSignal::new(0.6),
            first: Box::new(Tree::Pane(document)),
            second: Box::new(Tree::Pane(board)),
        }),
    };
    let tabs = vec![
        (workspace.id(), RwSignal::new(first)),
        (workspace.id(), RwSignal::new(Tree::Pane(terminal))),
    ];
    workspace.active.set(tabs[0].0);
    workspace.focused.set(agent);
    workspace.tabs.set(tabs);

    let strip = el("div")
        .class("tabs")
        .each(
            move || workspace.tabs.get(),
            |(id, _)| *id,
            move |(id, tree)| {
                let (id, tree) = (*id, *tree);
                el("div")
                    .class_dyn(move || {
                        if workspace.active.get() == id {
                            "tab-label active".to_string()
                        } else {
                            "tab-label".to_string()
                        }
                    })
                    .text_dyn(move || {
                        let position = (workspace.tabs.get().iter())
                            .position(|(tab, _)| *tab == id)
                            .unwrap_or(0);
                        format!("Tab {}", position + 1)
                    })
                    .on("click", move |_| {
                        workspace.show_tab(id, tree);
                        workspace.focus_pane();
                    })
            },
        )
        .when(
            move || workspace.mode.get(),
            || el("span").class("mode").text("WORKSPACE"),
        );

    root.attr("tabindex", "0")
        .attr_dyn("class", move || {
            if workspace.resizing.get().is_some() {
                "workspace resizing".to_string()
            } else {
                "workspace".to_string()
            }
        })
        // On the keys' way in: a view with the focus, a terminal above
        // all, would otherwise have the shell's chords before the shell.
        .on_capture("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            if workspace.palette.get_untracked() {
                return;
            }
            let character = match &key.key {
                Key::Character(character) => character.as_str(),
                _ => "",
            };
            if key.modifiers.ctrl() && character == "'" {
                workspace.mode.set(!workspace.mode.get_untracked());
                prevent_default();
                stop_propagation();
                return;
            }
            if !workspace.mode.get_untracked() {
                return;
            }
            if character == ":" {
                workspace.query.set(String::new());
                workspace.chosen.set(0);
                workspace.palette.set(true);
            } else if key.key == Key::Named(NamedKey::Escape) {
                workspace.mode.set(false);
            }
            // In the mode, no key is the view's.
            prevent_default();
            stop_propagation();
        })
        .on("pointermove", move |event| {
            let DomEventData::PointerMove(pointer) = &event.data else {
                return;
            };
            let Some((split, across, ratio)) = workspace.resizing.get_untracked() else {
                return;
            };
            let (x, y) = (pointer.page_x() as f64, pointer.page_y() as f64);
            dispatched(move |page| {
                let Some(bounds) = page.bounds(split) else {
                    return;
                };
                let share = if across {
                    (x - bounds.x) / bounds.width
                } else {
                    (y - bounds.y) / bounds.height
                };
                ratio.set((share as f32).clamp(0.1, 0.9));
            });
        })
        .on("pointerup", move |_| {
            if workspace.resizing.get_untracked().is_some() {
                workspace.resizing.set(None);
            }
        })
        .child(strip)
        .each(
            move || workspace.tabs.get(),
            |(id, _)| *id,
            move |(id, tree)| tab(workspace, *id, *tree),
        )
        .when(move || workspace.palette.get(), move || palette(workspace));
    workspace.focus_pane();
    (root, workspace)
}
