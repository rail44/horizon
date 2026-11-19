//! A list to search and pick from, in a sheet over the workspace
//! (`over`): what the palette, the view chooser and the model picker are.
//! The rows follow what is typed and whatever else they are read from;
//! the arrows and Tab move the choice, and Enter confirms it.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, WithUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Class, El, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

use super::{Shell, styles};
use horizon_components::{Field, Prompt};

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
/// is told which of them was confirmed, by Enter or a click. Leaving goes back to workspace
/// mode as it was.
pub(super) fn view(
    shell: Shell,
    rows: impl Fn(&str) -> Vec<Row> + Send + Sync + 'static,
    picked: impl Fn(usize) + Clone + 'static,
) -> El {
    view_in_steps(shell, rows, picked, || false)
}

/// [`view`] for a list of more than one step: Escape asks `back`, which
/// says whether there was a step to go back to, and is the sheet's where
/// there was none.
pub(super) fn view_in_steps(
    shell: Shell,
    rows: impl Fn(&str) -> Vec<Row> + Send + Sync + 'static,
    picked: impl Fn(usize) + Clone + 'static,
    back: impl Fn() -> bool + 'static,
) -> El {
    let input = el("input")
        .attr("type", "text")
        .on("input", move |event| {
            if let DomEventData::Input(input) = &event.data {
                shell.query.set(input.value.clone());
                shell.chosen.set(0);
            }
        })
        .focus();
    // What was typed is taken back by whoever empties it: a step taken.
    Effect::new(move |_| {
        if shell.query.get().is_empty() {
            input.set_value("");
        }
    });
    let rows = Memo::new(move |_| rows(&shell.query.get()));
    let clicked = picked.clone();
    let items = el("div").class(styles::ITEMS).each(
        move || rows.get().into_iter().enumerate().collect::<Vec<_>>(),
        |(index, row)| (*index, row.key.clone()),
        move |(index, row)| {
            let (index, class, clicked) = (*index, row.class, clicked.clone());
            let item = el("div")
                .class((styles::ITEM, class))
                .class_dyn(move || (shell.chosen.get() == index).then_some(styles::CHOSEN))
                .attr("style", &format!("padding-left: {}px", 12. + row.indent))
                .on("click", move |_| clicked(index));
            item.children(
                (row.parts.iter()).map(|(class, text)| el("div").class(*class).text(text)),
            )
        },
    );
    el("div")
        .class(styles::PICKER)
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            if key.key == Key::Named(NamedKey::Escape) && !back() {
                return;
            }
            let count = rows.with_untracked(Vec::len);
            let chosen = shell.chosen.get_untracked().min(count.saturating_sub(1));
            let (down, up) = (
                (chosen + 1).min(count.saturating_sub(1)),
                chosen.saturating_sub(1),
            );
            match &key.key {
                Key::Named(NamedKey::ArrowDown) => shell.chosen.set(down),
                Key::Named(NamedKey::ArrowUp) => shell.chosen.set(up),
                Key::Named(NamedKey::Tab) if key.modifiers.shift() => shell.chosen.set(up),
                Key::Named(NamedKey::Tab) => shell.chosen.set(down),
                Key::Named(NamedKey::Escape) => {}
                _ if count == 0 => return,
                Key::Named(NamedKey::Enter) => picked(chosen),
                // Anything else is the field's.
                _ => return,
            }
            prevent_default();
            stop_propagation();
        })
        .child(el("div").class(styles::SEARCH).child(Field {
            input,
            prompt: Some(Prompt {
                text: "Search...".into(),
                shown: Signal::derive(move || shell.query.get().is_empty()),
            }),
        }))
        .child(items)
}
