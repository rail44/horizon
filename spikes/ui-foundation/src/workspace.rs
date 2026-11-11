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

use blitz_dom::NodeId;
use blitz_traits::events::DomEventData;
use keyboard_types::{Key, NamedKey};
use reactive_graph::effect::Effect;
use reactive_graph::owner::{LocalStorage, Owner, StoredValue};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, GetUntracked, Set, Update, WithValue};

use crate::agent::sample::Session;
use crate::board::Task;
use crate::term::{Motion, Screen};
use crate::with_document;
use crate::{El, agent, board, dispatched, el, later, markdown, prevent_default, stop_propagation};

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it.
pub const CSS: &str = "
    .workspace { position: relative; display: flex; flex-direction: column; height: 100%;
                 outline: none; font-family: sans-serif; font-size: 13px;
                 line-height: 20px; color: #202020; background: #e4e4e4;
        &.resizing { user-select: none; }
        .tabs { flex: none; display: flex; align-items: center; gap: 2px;
                padding: 4px 8px 0 8px; }
        .tab-label { padding: 2px 12px; border-radius: 4px 4px 0 0; color: #606060;
                     cursor: pointer; }
        .tab-label.active { background: #fafafa; color: #202020; }
        .mode { margin-left: auto; padding: 0 6px; border-radius: 3px; background: #356df3;
                color: white; font-size: 11px; line-height: 16px; }
        .tab { flex: 1; min-height: 0; display: flex; }
        .tab.hidden { display: none; }
        .split { flex: 1; min-width: 0; min-height: 0; display: flex; }
        .split.down { flex-direction: column; }
        .cell { flex-shrink: 1; flex-basis: 0; min-width: 0; min-height: 0; display: flex; }
        .divider { flex: none; background: #c8c8c8; }
        .split.across > .divider { width: 5px; cursor: col-resize; }
        .split.down > .divider { height: 5px; cursor: row-resize; }
        .pane { flex: 1; min-width: 0; min-height: 0; box-sizing: border-box;
                display: flex; flex-direction: column; overflow: hidden;
                border: 1px solid transparent; background: #fafafa; }
        .pane.focused { border-color: #356df3; }
        .pane > * { flex: 1; min-height: 0; }
        .empty { display: flex; align-items: center; justify-content: center;
                 color: #808080; outline: none; }
        .terminal { outline: none; }
        .scrim { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 100;
                 display: flex; justify-content: center; align-items: flex-start;
                 padding-top: 80px; background: rgba(0, 0, 0, 0.15); }
        .palette { width: 420px; display: flex; flex-direction: column; gap: 2px;
                   padding: 6px; background: white; border: 1px solid #c0c0c0;
                   border-radius: 6px; }
        .palette input { font-size: 14px; line-height: 22px; padding: 4px 8px;
                         border: none; outline: none; }
        .item { padding: 2px 8px; border-radius: 4px; cursor: pointer; }
        .item.chosen { background: #e9effd; }
    }
";

/// The page's style sheet with this view and the views its panes show.
pub fn css() -> String {
    [CSS, agent::CSS, markdown::CSS, board::CSS].concat()
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
    resizing: RwSignal<Option<(NodeId, bool, RwSignal<f32>)>>,
    next_id: RwSignal<u32>,
    panes: StoredValue<Panes, LocalStorage>,
    owner: StoredValue<Owner, LocalStorage>,
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
                .class_dyn(move || {
                    if workspace.focused.get() == id {
                        "pane focused".to_string()
                    } else {
                        "pane".to_string()
                    }
                })
                .on("pointerdown", move |_| {
                    if workspace.focused.get_untracked() != id {
                        workspace.focused.set(id);
                    }
                })
                .each(
                    move || vec![kind.get()],
                    |kind| *kind,
                    |kind| content(*kind),
                )
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
                    later(move |document| {
                        document.mutate().remove_and_drop_node(pane.element.0);
                    });
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

fn content(kind: Kind) -> El {
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
        Kind::Terminal => el("div")
            .class("terminal")
            .attr("tabindex", "0")
            .widget(Screen::new(Motion::Still)),
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
    Effect::new(move |previous: Option<(Option<NodeId>, Vec<NodeId>)>| {
        let tree = tree.get();
        let (split, panes) = previous.unwrap_or_default();
        with_document(|document| {
            let mut mutator = document.mutate();
            for pane in panes {
                if mutator.node_has_parent(pane) {
                    mutator.remove_node(pane);
                }
            }
            // With the boxes inside it.
            if let Some(split) = split {
                mutator.remove_and_drop_node(split);
            }
        });
        let top = arrange(&tree, workspace);
        root.child(top);
        let panes: Vec<NodeId> = workspace.panes.with_value(|panes| {
            let panes = panes.borrow();
            (tree.panes().iter())
                .map(|id| panes[id].element.0)
                .collect()
        });
        ((!panes.contains(&top.0)).then_some(top.0), panes)
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
                workspace.resizing.set(Some((split.0, across, ratio)));
                // Not the start of a selection in the text around it.
                prevent_default();
            });
            split.child(first).child(divider).child(second)
        }
    }
}

fn palette(workspace: Workspace) -> El {
    let close = move || {
        workspace.palette.set(false);
        workspace.focus_pane();
    };
    let run = move |command: Command| {
        workspace.palette.set(false);
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
                .text(command.label())
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
            .child(input)
            .child(items),
    )
}

pub fn view() -> (El, Workspace) {
    let workspace = Workspace {
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
        .on("keydown", move |event| {
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
                let mode = !workspace.mode.get_untracked();
                workspace.mode.set(mode);
                if mode {
                    // The document hands a key to the focused element and
                    // then outwards: for the shell to have the keys before
                    // a view does, the focus is the shell's own.
                    root.focus();
                } else {
                    workspace.focus_pane();
                }
                prevent_default();
                return;
            }
            if !workspace.mode.get_untracked() {
                return;
            }
            if character == ":" {
                workspace.mode.set(false);
                workspace.query.set(String::new());
                workspace.chosen.set(0);
                workspace.palette.set(true);
            } else if key.key == Key::Named(NamedKey::Escape) {
                workspace.mode.set(false);
                workspace.focus_pane();
            }
            prevent_default();
        })
        .on("pointermove", move |event| {
            let DomEventData::PointerMove(pointer) = &event.data else {
                return;
            };
            let Some((split, across, ratio)) = workspace.resizing.get_untracked() else {
                return;
            };
            let (x, y) = (pointer.page_x() as f64, pointer.page_y() as f64);
            dispatched(move |document| {
                let Some(bounds) = document.get_client_bounding_rect(split) else {
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
