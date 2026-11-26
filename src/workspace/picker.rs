//! A list to search and pick from: what the palette, the view chooser and
//! the model picker are, in a sheet over the workspace (`over`), and what a
//! workspace with no tab shows. The rows follow what is typed and whatever
//! else they are read from; the arrows and Tab move the choice, and Enter
//! confirms it.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::keys::Key;
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, WithUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Class, El, el};

use super::styles;
use horizon_components::{Field, Prompt, ROW_SELECTED, list_row};

/// What a list is searched by and where its choice is: a list's own, so
/// that one list does not follow what is typed into another (the sheet's
/// over a workspace with no tab, whose own list is under it).
#[derive(Clone, Copy)]
pub(super) struct Search {
    pub(super) query: RwSignal<String>,
    /// The row chosen, by its place among the rows.
    pub(super) chosen: RwSignal<usize>,
}

impl Search {
    pub(super) fn new() -> Self {
        Self {
            query: RwSignal::new(String::new()),
            chosen: RwSignal::new(0),
        }
    }

    /// Nothing typed, the first row chosen.
    pub(super) fn clear(&self) {
        self.query.set(String::new());
        self.chosen.set(0);
    }
}

/// One row: its parts as text, each under a class of its own.
#[derive(Clone, PartialEq)]
pub(super) struct Row {
    /// What tells the row from the others, and from itself changed.
    pub(super) key: String,
    /// What kind of row it is, and what state it is in.
    pub(super) class: (Class, Option<Class>),
    pub(super) parts: Vec<(Class, String)>,
    /// How far in the row starts, in CSS pixels.
    pub(super) indent: f32,
}

/// Builds the list: `rows` gives the rows for what is typed, and `picked`
/// is told which of them was confirmed, by Enter or a click, which does
/// `enter` in a few words. Leaving goes back to workspace mode as it was.
pub(super) fn view(
    search: Search,
    rows: impl Fn(&str) -> Vec<Row> + Send + Sync + 'static,
    picked: impl Fn(usize) + Clone + 'static,
    enter: &str,
) -> El {
    list(search, rows, picked, enter, ("close", || false))
}

/// [`view`] for a list of more than one step: Escape asks `back`, which
/// says whether there was a step to go back to, and is the sheet's where
/// there was none.
pub(super) fn view_in_steps(
    search: Search,
    rows: impl Fn(&str) -> Vec<Row> + Send + Sync + 'static,
    picked: impl Fn(usize) + Clone + 'static,
    enter: &str,
    back: impl Fn() -> bool + 'static,
) -> El {
    list(search, rows, picked, enter, ("back", back))
}

/// The list, Escape doing `leave` by `back` or else left to the sheet.
fn list(
    search: Search,
    rows: impl Fn(&str) -> Vec<Row> + Send + Sync + 'static,
    picked: impl Fn(usize) + Clone + 'static,
    enter: &str,
    (leave, back): (&str, impl Fn() -> bool + 'static),
) -> El {
    let input = el("input")
        .attr("type", "text")
        .on("input", move |event| {
            if let DomEventData::Input(input) = &event.data {
                search.query.set(input.value.clone());
                search.chosen.set(0);
            }
        })
        .focus();
    // What was typed is taken back by whoever empties it: a step taken.
    Effect::new(move |_| {
        if search.query.get().is_empty() {
            input.set_value("");
        }
    });
    let rows = Memo::new(move |_| rows(&search.query.get()));
    let clicked = picked.clone();
    let items = el("div").class(styles::ITEMS).each(
        move || rows.get().into_iter().enumerate().collect::<Vec<_>>(),
        |(index, row)| (*index, row.key.clone()),
        move |(index, row)| {
            let (index, class, clicked) = (*index, row.class, clicked.clone());
            let item = list_row()
                .class((styles::ITEM, class))
                .class_dyn(move || (search.chosen.get() == index).then_some(ROW_SELECTED))
                .attr(
                    "style",
                    &format!("padding-left: calc({}px - var(--line))", 12. + row.indent),
                )
                .on("click", move |_| clicked(index));
            item.children(
                (row.parts.iter()).map(|(class, text)| el("div").class(*class).text(text)),
            )
        },
    );
    // The choice, among the rows there are now.
    let chosen = move || {
        let count = rows.with_untracked(Vec::len);
        (
            search.chosen.get_untracked().min(count.saturating_sub(1)),
            count,
        )
    };
    let step = move |by: isize| {
        let (at, count) = chosen();
        let to = (at as isize + by).clamp(0, count.saturating_sub(1) as isize);
        search.chosen.set(to as usize);
    };
    el("div")
        .class(styles::PICKER)
        // Anything else is the field's.
        .keys(vec![
            Key::group("↑/↓", "move")
                .bind("down", move || step(1))
                .bind("up", move || step(-1))
                .bind("tab", move || step(1))
                .bind("shift+tab", move || step(-1)),
            Key::when("enter", enter, move || {
                let (at, count) = chosen();
                if count > 0 {
                    picked(at);
                }
                count > 0
            }),
            Key::when("esc", leave, back),
        ])
        .child(el("div").class(styles::SEARCH).child(Field {
            input,
            prompt: Some(Prompt {
                text: "Search...".into(),
                shown: Signal::derive(move || search.query.get().is_empty()),
            }),
        }))
        .child(items)
}
