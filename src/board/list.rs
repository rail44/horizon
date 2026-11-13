//! The board's task list: the tasks as the tree their parents make, in
//! rank order, the finished ones gathered behind one row at the bottom,
//! with a field to add one. Rows are selected, folded and opened from the
//! keyboard, and reordered among their siblings by dragging.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use horizon_board::{Item, Position};
use horizon_board_view::chrome;
use horizon_board_view::model::{
    self, DropHalf, ListCommand, Row, drop_position_from_half, finished_summary,
    list_command_for_key, parse_new_task, sibling_move, status_text, step_selection, tree_rows,
    visible_rows,
};
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, DomEventData};
use horizon_ui::reactive_graph::owner::{LocalStorage, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{
    Get, GetUntracked, Set, Update, With, WithUntracked, WithValue,
};
use horizon_ui::{
    El, OneOf, dispatched, each_layout, el, prevent_default, relayout, stop_propagation,
};
use keyboard_types::{Key, NamedKey};

#[cfg(not(target_family = "wasm"))]
use super::follow;
use super::{Activity, Job, Source, activity_chip, run, tone, write_refusal};
use crate::background::Background;

// The view's own style sheet. The measurements are in the theme's scaled
// lengths (`--u8` is 8 px at the reference type size).
const CSS: &str = "
    .board-list { position: relative; display: flex; flex-direction: column; height: 100%;
                  min-width: calc(var(--u28) * 10); outline: none;
                  background: var(--background); color: var(--text);
                  font-family: var(--font-family); font-size: var(--body-size);
                  line-height: var(--body-line);
        .head, .band, .notice { flex: none; padding: var(--u4) var(--u16);
                                font-size: var(--meta-size); line-height: var(--meta-line);
                                color: var(--text-muted); }
        .head { border-bottom: 1px solid var(--border); }
        .rows { flex: 1; min-height: 0; overflow-y: scroll; }
        .row { position: relative; display: flex; align-items: center; flex: none;
               box-sizing: border-box; height: calc(var(--u24) * 2); cursor: pointer;
               border: 1px solid transparent; }
        .row:hover, .band:hover { background: var(--band); }
        .row.selected { background: var(--selection); }
        .row.carried { opacity: 0.5; }
        .bar { flex: none; align-self: stretch; width: 2px; }
        .row.selected .bar { background: var(--accent); }
        .gutter { flex: none; width: var(--u8); display: flex; align-items: center;
                  justify-content: center; }
        .unread-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); }
        .disclosure { flex: none; width: var(--u12); text-align: center;
                      font-size: var(--micro-size); line-height: var(--micro-line);
                      color: var(--text-muted); }
        .label { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
        .title { overflow: hidden; white-space: nowrap; text-overflow: ellipsis;
                 opacity: 0.85; }
        .row.unread .title { opacity: 1; font-weight: 600; }
        .row.finished .title { opacity: 1; color: var(--text-muted); }
        .meta { overflow: hidden; white-space: nowrap; font-size: var(--micro-size);
                line-height: var(--micro-line); color: var(--text-muted); }
        .tail { flex: none; display: flex; align-items: center; gap: var(--u4);
                padding: 0 var(--u16) 0 var(--u4); }
        .chip { padding: 0 var(--u4); border-radius: 2px; font-size: var(--micro-size);
                line-height: var(--micro-line); }
        .chip.muted { background: var(--chip-muted-surface); color: var(--chip-muted-text); }
        .chip.accent { background: var(--chip-accent-surface); color: var(--chip-accent-text); }
        .chip.success { background: var(--chip-success-surface);
                        color: var(--chip-success-text); }
        .chip.info { background: var(--chip-info-surface); color: var(--chip-info-text); }
        .chip.warning { background: var(--chip-warning-surface);
                        color: var(--chip-warning-text); }
        .chip.danger { background: var(--chip-muted-surface); color: var(--danger); }
        .status-dot { width: 8px; height: 8px; border-radius: 50%; opacity: 0.55; }
        .status-dot.muted { background: var(--text-muted); }
        .status-dot.accent { background: var(--accent); }
        .status-dot.success { background: var(--success); }
        .status-dot.info { background: var(--info); }
        .status-dot.warning { background: var(--warning); }
        .line { position: absolute; left: 0; right: 0; height: 2px;
                background: var(--accent); }
        .line.above { top: -1px; }
        .line.below { bottom: -1px; }
        .band { display: flex; align-items: center; gap: var(--u4); cursor: pointer; }
        .add { flex: none; position: relative; padding: var(--u4) var(--u16);
               border-top: 1px solid var(--border); }
        .add input { width: 100%; font-family: var(--font-family);
                     font-size: var(--body-size); line-height: var(--body-line); padding: 0;
                     border: none; outline: none; background: transparent;
                     color: var(--text); }
        .add .placeholder { position: absolute; left: var(--u16); top: var(--u4);
                            color: var(--text-muted); pointer-events: none; }
        .ghost { position: absolute; z-index: 10; padding: 4px 8px;
                 background: var(--surface-selected); pointer-events: none; }
    }
    .board-list:focus .row.selected { border-color: var(--accent); }
";

/// How far the pointer moves with the button held before a press on a row
/// is a drag of it.
const SLOP: f32 = 4.;

/// What the list asks of whatever holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum Asked {
    /// The selected task's thread.
    OpenThread(u64),
}

/// What the view keeps that is not a signal.
struct Held {
    source: Source,
    background: Background,
    ask: Box<dyn Fn(Asked)>,
    /// Each task's row on the page.
    rows: RefCell<HashMap<u64, El>>,
    /// The task whose row is to be brought into view at the next layout.
    reveal: Cell<Option<u64>>,
}

/// The view's state, for whoever placed it to read or drive.
#[derive(Clone, Copy)]
pub struct BoardList {
    items: RwSignal<Vec<Item>>,
    positions: RwSignal<HashMap<u64, String>>,
    rows: RwSignal<Vec<Row>>,
    /// The parents whose children are folded away.
    pub collapsed: RwSignal<HashSet<u64>>,
    pub selected: RwSignal<Option<u64>>,
    pub finished_expanded: RwSignal<bool>,
    /// A line above the field: a write refused, a store that did not open.
    pub notice: RwSignal<Option<String>>,
    /// What the sessions the tasks are bound to are doing.
    pub activity: Activity,
    /// The row being dragged, and the row and half it would land by.
    carried: RwSignal<Option<u64>>,
    indicator: RwSignal<Option<(u64, DropHalf)>>,
    typed: RwSignal<String>,
    held: StoredValue<Rc<Held>, LocalStorage>,
    pub element: El,
    field: El,
    scroll: El,
}

/// The store as the list reads it: the tasks, and how far the owner has
/// read each one's posts.
type Read = (Vec<Item>, HashMap<u64, String>);

/// One line of the list: a task's row, or the row the finished tasks are
/// gathered behind.
#[derive(Clone)]
enum Line {
    Task(Box<Row>, String),
    Band {
        count: usize,
        unread: usize,
        expanded: bool,
    },
}

impl BoardList {
    fn held(&self) -> Rc<Held> {
        self.held.with_value(Rc::clone)
    }

    /// Every session the tasks are bound to, each once.
    pub fn bound_sessions(&self) -> Vec<horizon_workspace::SessionId> {
        self.items
            .with_untracked(|items| model::bound_sessions(items))
    }

    /// The directory of the project whose board this is.
    pub fn root(&self) -> Option<std::path::PathBuf> {
        self.held().source.root()
    }

    /// Reads the store again.
    pub fn load(&self) {
        let (this, held) = (*self, self.held());
        let read: fn(horizon_board::Store) -> Job<Read> = |store| {
            Box::pin(async move {
                Ok((
                    store.list(None, true)?.items,
                    store.read_positions("owner")?,
                ))
            })
        };
        run(
            &held.background,
            held.source.clone(),
            read,
            move |read| match read {
                Ok((items, positions)) => {
                    this.items.set(items);
                    this.positions.set(positions);
                    this.rebuild();
                }
                Err(error) => this.notice.set(Some(error.to_string())),
            },
        );
    }

    /// Writes to the store, then reads it again; a write refused is said
    /// on the notice line.
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

    fn rebuild(&self) {
        let rows = self.items.with_untracked(|items| {
            (self.positions).with_untracked(|positions| {
                (self.collapsed).with_untracked(|collapsed| tree_rows(items, positions, collapsed))
            })
        });
        self.rows.set(rows);
        self.clamp_selection();
    }

    /// The tasks whose rows are shown, in order.
    pub fn visible(&self) -> Vec<u64> {
        self.rows.with_untracked(|rows| {
            (visible_rows(rows, self.finished_expanded.get_untracked()).into_iter())
                .filter_map(|index| rows.get(index).map(|row| row.item.id))
                .collect()
        })
    }

    fn selected_row(&self) -> Option<Row> {
        let id = self.selected.get_untracked()?;
        (self.rows).with_untracked(|rows| rows.iter().find(|row| row.item.id == id).cloned())
    }

    /// The selection is a row that is shown: the first, if the one it was
    /// on is not.
    fn clamp_selection(&self) {
        let visible = self.visible();
        if !(self.selected.get_untracked()).is_some_and(|id| visible.contains(&id)) {
            self.selected.set(visible.first().copied());
        }
    }

    fn step(&self, forward: bool) {
        let visible = self.visible();
        let selected = self.selected.get_untracked();
        let next = step_selection(&visible, selected, forward);
        if next != selected {
            self.selected.set(next);
            self.held().reveal.set(next);
        }
    }

    pub fn execute(&self, command: ListCommand) {
        match command {
            ListCommand::SelectNext => self.step(true),
            ListCommand::SelectPrevious => self.step(false),
            ListCommand::SelectTask(id) => {
                self.selected.set(Some(id));
                self.element.focus();
            }
            ListCommand::Expand => {
                if let Some(row) = self.selected_row().filter(|row| row.collapsed) {
                    self.collapsed.update(|collapsed| {
                        collapsed.remove(&row.item.id);
                    });
                    self.rebuild();
                }
            }
            // A parent closes; a leaf, or a parent closed already, hands
            // the selection to its own parent.
            ListCommand::Collapse => {
                let Some(row) = self.selected_row() else {
                    return;
                };
                if row.has_children && !row.collapsed {
                    self.collapsed.update(|collapsed| {
                        collapsed.insert(row.item.id);
                    });
                    self.rebuild();
                } else if let Some(parent) = row.item.parent {
                    self.selected.set(Some(parent));
                    self.held().reveal.set(Some(parent));
                }
            }
            ListCommand::ToggleExpansion(id) => {
                let parent = (self.rows).with_untracked(|rows| {
                    rows.iter().any(|row| row.item.id == id && row.has_children)
                });
                if parent {
                    self.collapsed.update(|collapsed| {
                        if !collapsed.remove(&id) {
                            collapsed.insert(id);
                        }
                    });
                    self.rebuild();
                }
            }
            ListCommand::ToggleFinished => {
                self.finished_expanded
                    .update(|expanded| *expanded = !*expanded);
                self.clamp_selection();
            }
            ListCommand::OpenThread => {
                if let Some(row) = self.selected_row() {
                    (self.held().ask)(Asked::OpenThread(row.item.id));
                }
            }
            ListCommand::MoveUp | ListCommand::MoveDown => {
                let Some(id) = self.selected.get_untracked() else {
                    return;
                };
                let up = command == ListCommand::MoveUp;
                let position = self
                    .items
                    .with_untracked(|items| sibling_move(items, id, up));
                if let Some(position) = position {
                    self.move_task(id, position);
                }
            }
            ListCommand::AddTask => {
                let Some(title) = parse_new_task(&self.typed.get_untracked()) else {
                    return;
                };
                self.typed.set(String::new());
                self.field.set_value("");
                self.mutate(move |store| {
                    Box::pin(async move {
                        (store.add(&title, "", None, Position::Bottom).await).map(|_| ())
                    })
                });
            }
            // A drop: carried out by `move_task` where it lands.
            ListCommand::Reorder => {}
        }
    }

    /// The moved task stays the selected one.
    fn move_task(&self, id: u64, position: Position) {
        self.selected.set(Some(id));
        self.mutate(move |store| {
            Box::pin(async move { store.move_item(id, position).await.map(|_| ()) })
        });
    }

    /// The task ids the list was last read with, in store order.
    pub fn tasks(&self) -> Vec<u64> {
        (self.items).with_untracked(|items| items.iter().map(|item| item.id).collect())
    }
}

/// A key as the board's key maps name it.
pub(super) fn key_name(key: &BlitzKeyEvent) -> Option<String> {
    Some(match &key.key {
        Key::Character(character) => character.to_lowercase(),
        Key::Named(NamedKey::ArrowDown) => "down".to_string(),
        Key::Named(NamedKey::ArrowUp) => "up".to_string(),
        Key::Named(NamedKey::ArrowLeft) => "left".to_string(),
        Key::Named(NamedKey::ArrowRight) => "right".to_string(),
        Key::Named(NamedKey::Enter) => "enter".to_string(),
        Key::Named(NamedKey::Escape) => "escape".to_string(),
        _ => return None,
    })
}

/// What a row shows of its task, as text: a row is built again when any
/// of it changes.
fn shown(row: &Row, now: u64) -> String {
    let item = &row.item;
    let last = item.comments.last().and_then(|comment| comment.at);
    let age = last.map(|at| chrome::relative_time(at, now));
    format!(
        "{} {} {} {} {} {} {} {} {} {}",
        row.depth,
        item.title,
        status_text(item),
        super::tone(item),
        row.unread,
        row.subtree_unread,
        row.has_children,
        row.collapsed,
        row.finished,
        chrome::row_meta(age.as_deref(), item.comments.len()),
    )
}

/// Which row each of a list's marks is on. A row asks whether it is the
/// one, and is told when that changes: a mark that moves is the business
/// of the row it leaves and the row it reaches, not of every row.
#[derive(Clone)]
struct Marks {
    selected: OneOf<Option<u64>>,
    carried: OneOf<Option<u64>>,
    indicator: OneOf<Option<(u64, DropHalf)>>,
}

fn row(state: BoardList, marks: &Marks, row: &Row) -> El {
    let (marked, above, below) = (marks.clone(), marks.clone(), marks.clone());
    let (id, item) = (row.item.id, &row.item);
    let now = model::now_ms();
    let last = item.comments.last().and_then(|comment| comment.at);
    let age = last.map(|at| chrome::relative_time(at, now));
    let status = status_text(item);
    let fixed = match (row.unread > 0, row.finished) {
        (true, _) => "row unread",
        (false, true) => "row finished",
        (false, false) => "row",
    };
    let disclosure = el("div").class("disclosure");
    if row.has_children {
        disclosure
            .text(if row.collapsed { "▸" } else { "▾" })
            // The fold alone: not a press that selects or drags the row.
            .on("pointerdown", |_| stop_propagation())
            .on("click", move |_| {
                state.execute(ListCommand::ToggleExpansion(id))
            });
    }
    let tail = el("div").class("tail");
    // The unread posts folded away under the row, beside its own.
    if row.subtree_unread > row.unread {
        let unread = chrome::unread_count(row.subtree_unread);
        tail.child(el("span").class("chip accent").text(&unread));
    }
    activity_chip(tail, item, state.activity);
    let tone = tone(item);
    tail.child(match row.finished || status.is_empty() {
        true => el("div").class(&format!("status-dot {tone}")),
        false => el("span").class(&format!("chip {tone}")).text(&status),
    });
    let gutter = el("div").class("gutter");
    if row.subtree_unread > 0 {
        gutter.child(el("div").class("unread-dot"));
    }
    el("div")
        .class_dyn(move || {
            let mut class = fixed.to_string();
            if marked.selected.is(&Some(id)) {
                class += " selected";
            }
            if marked.carried.is(&Some(id)) {
                class += " carried";
            }
            class
        })
        .child(el("div").class("bar"))
        .child(gutter)
        .child(el("div").attr(
            "style",
            &format!("flex: none; width: calc(var(--u16) * {})", row.depth),
        ))
        .child(disclosure)
        .child(
            el("div")
                .class("label")
                .child(el("div").class("title").text(&item.title))
                .child(
                    el("div")
                        .class("meta")
                        .text(&chrome::row_meta(age.as_deref(), item.comments.len())),
                ),
        )
        .child(tail)
        .when(
            move || above.indicator.is(&Some((id, DropHalf::Above))),
            || el("div").class("line above"),
        )
        .when(
            move || below.indicator.is(&Some((id, DropHalf::Below))),
            || el("div").class("line below"),
        )
}

/// Builds the list over `source`'s store, read at once and again whenever
/// its log takes a write; `ask` hears what the list asks for.
pub fn view(
    source: Source,
    background: Background,
    ask: impl Fn(Asked) + 'static,
) -> (El, BoardList) {
    let state = BoardList {
        items: RwSignal::new(Vec::new()),
        positions: RwSignal::new(HashMap::new()),
        rows: RwSignal::new(Vec::new()),
        collapsed: RwSignal::new(HashSet::new()),
        selected: RwSignal::new(None),
        finished_expanded: RwSignal::new(false),
        notice: RwSignal::new(None),
        activity: RwSignal::new(HashMap::new()),
        carried: RwSignal::new(None),
        indicator: RwSignal::new(None),
        typed: RwSignal::new(String::new()),
        held: StoredValue::new_local(Rc::new(Held {
            source: source.clone(),
            background: background.clone(),
            ask: Box::new(ask),
            rows: RefCell::default(),
            reveal: Cell::new(None),
        })),
        element: el("div")
            .class("board-list")
            .attr("tabindex", "0")
            .styled(CSS),
        field: el("input").attr("type", "text"),
        scroll: el("div").class("rows"),
    };
    let (root, scroll, field) = (state.element, state.scroll, state.field);
    let held = state.held();
    state.load();
    #[cfg(not(target_family = "wasm"))]
    if let Source::Root(root) = source {
        let stop = follow(root, background.clone(), move || state.load());
        horizon_ui::reactive_graph::owner::on_cleanup(move || drop(stop));
    }
    #[cfg(target_family = "wasm")]
    let _ = (source, background);

    // The row pressed and where, until the button comes up.
    let pressed = RwSignal::new(None::<(u64, f32, f32)>);
    // Where the dragged row's copy is, in the view.
    let ghost_at = RwSignal::new((0f32, 0f32));

    let marks = Marks {
        selected: OneOf::new(move || state.selected.get()),
        carried: OneOf::new(move || state.carried.get()),
        indicator: OneOf::new(move || state.indicator.get()),
    };
    let known = held.clone();
    scroll.each(
        move || {
            let (count, unread) = state.rows.with(|rows| finished_summary(rows));
            let expanded = state.finished_expanded.get();
            let now = model::now_ms();
            let band = Line::Band {
                count,
                unread,
                expanded,
            };
            let mut lines = Vec::new();
            let mut band_placed = false;
            state.rows.with(|rows| {
                for index in visible_rows(rows, expanded) {
                    let Some(row) = rows.get(index) else {
                        continue;
                    };
                    if row.in_finished_band && !band_placed {
                        band_placed = true;
                        lines.push(band.clone());
                    }
                    lines.push(Line::Task(Box::new(row.clone()), shown(row, now)));
                }
            });
            if count > 0 && !band_placed {
                lines.push(band);
            }
            lines
        },
        |line| match line {
            Line::Task(row, shown) => format!("task {} {shown}", row.item.id),
            Line::Band {
                count,
                unread,
                expanded,
            } => format!("band {count} {unread} {expanded}"),
        },
        move |line| match line {
            Line::Task(task, _) => {
                let id = task.item.id;
                let element = row(state, &marks, task).on("pointerdown", move |event| {
                    if let DomEventData::PointerDown(pointer) = &event.data {
                        pressed.set(Some((id, pointer.page_x(), pointer.page_y())));
                    }
                });
                known.rows.borrow_mut().insert(id, element);
                element
            }
            Line::Band {
                count,
                unread,
                expanded,
            } => {
                let band = el("div")
                    .class("band")
                    .child(el("span").text(if *expanded { "▾" } else { "▸" }))
                    .child(el("span").text(&chrome::done_band(*count)))
                    .on("click", move |_| state.execute(ListCommand::ToggleFinished));
                if *unread > 0 {
                    let unread = chrome::unread_count(*unread);
                    band.child(el("span").class("chip accent").text(&unread));
                }
                band
            }
        },
    );

    // The selection is kept in view: a step from the keyboard scrolls the
    // rows as far as its row needs.
    let revealing = held.clone();
    each_layout(move |page| {
        if revealing.background.take_in() {
            relayout();
        }
        let Some(id) = revealing.reveal.take() else {
            return;
        };
        let row = revealing.rows.borrow().get(&id).copied();
        let (Some(row), Some(view)) = (row.and_then(|row| page.bounds(row)), page.bounds(scroll))
        else {
            return;
        };
        let above = row.y - view.y;
        let below = (row.y + row.height) - (view.y + view.height);
        if above < 0. {
            scroll.scroll_by(0., above);
        } else if below > 0. {
            scroll.scroll_by(0., below);
        }
    });

    let dragged = held.clone();
    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        if key.modifiers.ctrl() || key.modifiers.alt() || key.modifiers.meta() {
            return;
        }
        if let Some(command) = key_name(key).and_then(|name| list_command_for_key(&name)) {
            state.execute(command);
            prevent_default();
        }
    })
    .on("pointermove", move |event| {
        let DomEventData::PointerMove(pointer) = &event.data else {
            return;
        };
        let Some((id, from_x, from_y)) = pressed.get_untracked() else {
            return;
        };
        let (x, y) = (pointer.page_x(), pointer.page_y());
        if state.carried.get_untracked().is_none() {
            if (x - from_x).abs().max((y - from_y).abs()) < SLOP {
                return;
            }
            state.carried.set(Some(id));
        }
        let dragged = dragged.clone();
        // What is under the pointer and where its box is, asked of the
        // page once the event is done with.
        dispatched(move |page| {
            if let Some(origin) = page.bounds(root) {
                ghost_at.set((x - origin.x as f32 + 12., y - origin.y as f32 + 12.));
            }
            let rows = dragged.rows.borrow();
            let over = page.under(x, y).into_iter().find_map(|element| {
                let (target, _) = rows.iter().find(|(_, row)| **row == element)?;
                Some((element, *target))
            });
            // A row lands among its own siblings only.
            let next = over.and_then(|(row, target)| {
                let bounds = page.bounds(row)?;
                let half = match (y as f64) < bounds.y + bounds.height / 2. {
                    true => DropHalf::Above,
                    false => DropHalf::Below,
                };
                (state.items)
                    .with_untracked(|items| drop_position_from_half(id, items, target, half))
                    .map(|_| (target, half))
            });
            if state.indicator.get_untracked() != next {
                state.indicator.set(next);
            }
        });
    })
    .on("pointerup", move |_| {
        let Some((id, ..)) = pressed.get_untracked() else {
            return;
        };
        pressed.set(None);
        if state.carried.get_untracked().is_none() {
            // A press and a release in place: a click on the row.
            state.execute(ListCommand::SelectTask(id));
            return;
        }
        let indicator = state.indicator.get_untracked();
        state.carried.set(None);
        state.indicator.set(None);
        let position = indicator.and_then(|(target, half)| {
            (state.items).with_untracked(|items| drop_position_from_half(id, items, target, half))
        });
        if let Some(position) = position {
            state.move_task(id, position);
        }
    })
    .child(el("div").class("head").text_dyn(move || {
        state.rows.with(|rows| {
            let unread = rows.iter().map(|row| row.unread).sum();
            chrome::list_header_text(rows.len(), unread)
        })
    }))
    .child(scroll)
    .when(
        move || state.notice.with(Option::is_some),
        move || {
            el("div")
                .class("notice")
                .text_dyn(move || state.notice.get().unwrap_or_default())
        },
    )
    .child(
        // blitz-dom does not draw a field's `placeholder`.
        el("div")
            .class("add")
            .child(
                field
                    .on("input", move |event| {
                        if let DomEventData::Input(input) = &event.data {
                            state.typed.set(input.value.clone());
                        }
                    })
                    // The field's keys are its own: Enter adds the task,
                    // Escape goes back to the rows.
                    .on("keydown", move |event| {
                        let DomEventData::KeyDown(key) = &event.data else {
                            return;
                        };
                        match &key.key {
                            Key::Named(NamedKey::Enter) => {
                                state.execute(ListCommand::AddTask);
                                prevent_default();
                            }
                            Key::Named(NamedKey::Escape) => {
                                root.focus();
                            }
                            _ => {}
                        }
                        stop_propagation();
                    }),
            )
            .when(
                move || state.typed.with(String::is_empty),
                || el("div").class("placeholder").text(chrome::ADD_TASK),
            ),
    )
    .when(
        move || state.carried.get().is_some(),
        move || {
            el("div")
                .class("ghost")
                .style_dyn("left", move || format!("{}px", ghost_at.get().0))
                .style_dyn("top", move || format!("{}px", ghost_at.get().1))
                .text_dyn(move || {
                    let carried = state.carried.get();
                    state.items.with(|items| {
                        (items.iter().find(|item| Some(item.id) == carried))
                            .map(|item| item.title.clone())
                            .unwrap_or_default()
                    })
                })
        },
    );
    (root, state)
}
