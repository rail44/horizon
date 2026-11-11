//! The part of Horizon's board list that asks the most of the stack: rows
//! reordered by dragging (`src/board/list.rs`).
//!
//! A row is picked up once the pointer has moved a little with the button
//! held. While it is carried, a copy of its title follows the pointer and a
//! line shows where a release would put it: above or below the row under
//! the pointer, by which half the pointer is in, and only where the release
//! would move something. The document has no dragging of its own; this is
//! pointer events, hit testing and the rows' boxes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use blitz_dom::NodeId;
use blitz_traits::events::DomEventData;
use keyboard_types::Key;
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, GetUntracked, Set, Update};

use crate::{El, el, later};

/// How far the pointer moves with the button held before a row is carried.
const SLOP: f32 = 4.;

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it.
pub const CSS: &str = "
    .board { position: relative; display: flex; flex-direction: column; height: 100%;
             min-width: 240px; outline: none; user-select: none;
             font-family: sans-serif; font-size: 14px; line-height: 22px;
             color: #202020; background: #fafafa;
        .head { flex: none; padding: 8px 16px; border-bottom: 1px solid #d8d8d8;
                font-size: 12px; line-height: 18px; color: #808080; }
        .rows { flex: 1; min-height: 0; overflow-y: scroll; padding: 8px 0; }
        .row { position: relative; display: flex; align-items: center; gap: 8px;
               box-sizing: border-box; height: 28px; padding: 0 16px; border: 1px solid transparent;
               cursor: pointer; }
        .row.selected { background: #e9effd; border-color: #356df3; }
        .row.carried { opacity: 0.4; }
        .title { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; }
        .chip { flex: none; padding: 0 4px; border-radius: 3px; font-size: 11px;
                line-height: 16px; background: #ececec; color: #606060; }
        .line { position: absolute; left: 8px; right: 8px; height: 2px;
                background: #356df3; pointer-events: none; }
        .line.above { top: -2px; }
        .line.below { bottom: -2px; }
        .ghost { position: absolute; padding: 2px 8px; border-radius: 4px;
                 background: #ffffff; border: 1px solid #356df3; white-space: nowrap;
                 pointer-events: none; z-index: 10; }
    }
";

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: u64,
    pub title: String,
    pub status: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Half {
    Above,
    Below,
}

#[derive(Clone, Copy)]
pub struct Board {
    pub tasks: RwSignal<Vec<Task>>,
    pub selected: RwSignal<Option<u64>>,
    /// The row being carried.
    pub carried: RwSignal<Option<u64>>,
    /// Where a release would put it.
    pub indicator: RwSignal<Option<(u64, Half)>>,
}

/// Where in the list, with `carried` taken out, a release on `half` of
/// `target` puts it; `None` when that is where it already is.
fn destination(tasks: &[Task], carried: u64, target: u64, half: Half) -> Option<usize> {
    let position = |id| tasks.iter().position(|task| task.id == id);
    let (from, target) = (position(carried)?, position(target)?);
    if from == target {
        return None;
    }
    let target = if from < target { target - 1 } else { target };
    let to = match half {
        Half::Above => target,
        Half::Below => target + 1,
    };
    (to != from).then_some(to)
}

pub fn view(tasks: Vec<Task>) -> (El, Board) {
    let state = Board {
        tasks: RwSignal::new(tasks),
        selected: RwSignal::new(None),
        carried: RwSignal::new(None),
        indicator: RwSignal::new(None),
    };
    // The row pressed and where, until the button comes up.
    let pressed = RwSignal::new(None::<(u64, f32, f32)>);
    // Where the carried row's copy is, in the view.
    let ghost_at = RwSignal::new((0f32, 0f32));
    // Which task a row's element shows, for the element under the pointer.
    let rows_by_node: Rc<RefCell<HashMap<NodeId, u64>>> = Rc::default();

    let root = el("div").class("board").attr("tabindex", "0");
    let rows = el("div").class("rows");
    let known = rows_by_node.clone();
    rows.each(
        move || state.tasks.get(),
        |task| task.id,
        move |task| {
            let id = task.id;
            let row = el("div")
                .class_dyn(move || {
                    let mut class = "row".to_string();
                    if state.selected.get() == Some(id) {
                        class += " selected";
                    }
                    if state.carried.get() == Some(id) {
                        class += " carried";
                    }
                    class
                })
                .on("pointerdown", move |event| {
                    if let DomEventData::PointerDown(pointer) = &event.data {
                        pressed.set(Some((id, pointer.page_x(), pointer.page_y())));
                    }
                })
                .child(el("span").class("title").text(&task.title))
                .child(el("span").class("chip").text(&task.status))
                .when(
                    move || state.indicator.get() == Some((id, Half::Above)),
                    || el("div").class("line above"),
                )
                .when(
                    move || state.indicator.get() == Some((id, Half::Below)),
                    || el("div").class("line below"),
                );
            known.borrow_mut().insert(row.0, id);
            row
        },
    );

    let known = rows_by_node.clone();
    root.on("pointermove", move |event| {
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
        let known = known.clone();
        // The document is busy with the event: what is under the pointer
        // and where its box is are asked once it is done.
        later(move |document| {
            if let Some(origin) = document.get_client_bounding_rect(root.0) {
                ghost_at.set((x - origin.x as f32 + 12., y - origin.y as f32 + 12.));
            }
            let mut node = document.hit(x, y).map(|hit| hit.node_id);
            let mut over = None;
            while let Some(id) = node {
                if let Some(task) = known.borrow().get(&id) {
                    over = Some((id, *task));
                    break;
                }
                node = document.get_node(id).and_then(|node| node.parent);
            }
            let next = over.and_then(|(node, target)| {
                let bounds = document.get_client_bounding_rect(node)?;
                let half = if (y as f64) < bounds.y + bounds.height / 2. {
                    Half::Above
                } else {
                    Half::Below
                };
                destination(&state.tasks.get_untracked(), id, target, half).map(|_| (target, half))
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
            state.selected.set(Some(id));
            return;
        }
        let indicator = state.indicator.get_untracked();
        state.carried.set(None);
        state.indicator.set(None);
        let Some((target, half)) = indicator else {
            return;
        };
        state.tasks.update(|tasks| {
            let Some(to) = destination(tasks, id, target, half) else {
                return;
            };
            let from = tasks
                .iter()
                .position(|task| task.id == id)
                .expect("the row");
            let task = tasks.remove(from);
            tasks.insert(to, task);
        });
        // The moved task stays the selected one.
        state.selected.set(Some(id));
    })
    .on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        let step: isize = match &key.key {
            Key::Character(character) if character == "j" => 1,
            Key::Character(character) if character == "k" => -1,
            _ => return,
        };
        let tasks = state.tasks.get_untracked();
        let at = (state.selected.get_untracked())
            .and_then(|id| tasks.iter().position(|task| task.id == id));
        let next = match at {
            Some(at) => (at as isize + step).clamp(0, tasks.len() as isize - 1) as usize,
            None => 0,
        };
        if let Some(task) = tasks.get(next) {
            state.selected.set(Some(task.id));
        }
    })
    .child(
        el("div")
            .class("head")
            .text_dyn(move || format!("{} tasks", state.tasks.get().len())),
    )
    .child(rows)
    .when(
        move || state.carried.get().is_some(),
        move || {
            el("div")
                .class("ghost")
                .style_dyn("left", move || format!("{}px", ghost_at.get().0))
                .style_dyn("top", move || format!("{}px", ghost_at.get().1))
                .text_dyn(move || {
                    let carried = state.carried.get();
                    (state.tasks.get().into_iter())
                        .find(|task| Some(task.id) == carried)
                        .map(|task| task.title)
                        .unwrap_or_default()
                })
        },
    )
    .focus();
    (root, state)
}
