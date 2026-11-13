//! A list to search and pick from, over everything: what the palette and
//! the session manager both are. The rows follow what is typed and
//! whatever else they are read from; the arrows and Tab move the choice,
//! Enter confirms it, Escape or a press outside leaves.

use horizon_ui::blitz_traits::events::{BlitzKeyEvent, DomEventData};
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, WithUntracked};
use horizon_ui::{El, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

use super::Shell;
use crate::parts::{field, prompt};

/// One row: its parts as text, each under a class of its own.
#[derive(Clone, PartialEq)]
pub(super) struct Row {
    /// What tells the row from the others, and from itself changed.
    pub(super) key: String,
    /// The row's classes, beside `item`.
    pub(super) class: &'static str,
    pub(super) parts: Vec<(&'static str, String)>,
    /// How far in the row starts, in CSS pixels.
    pub(super) indent: f32,
}

/// What was done to the chosen row.
pub(super) enum Picked<'a> {
    /// Enter, or a click.
    Confirmed,
    /// Enter with Ctrl held.
    ConfirmedSecondary,
    /// A chord the list has no use for.
    Chord(&'a BlitzKeyEvent),
}

/// Builds the list: `rows` gives the rows for what is typed, and `picked`
/// is told what was done to which of them. Leaving goes back to workspace
/// mode as it was.
pub(super) fn view(
    shell: Shell,
    rows: impl Fn(&str) -> Vec<Row> + Send + Sync + 'static,
    picked: impl Fn(usize, Picked) + Clone + 'static,
) -> El {
    view_in_steps(shell, rows, picked, || false)
}

/// [`view`] for a list of more than one step: Escape asks `back` first,
/// which says whether there was a step to go back to.
pub(super) fn view_in_steps(
    shell: Shell,
    rows: impl Fn(&str) -> Vec<Row> + Send + Sync + 'static,
    picked: impl Fn(usize, Picked) + Clone + 'static,
    back: impl Fn() -> bool + 'static,
) -> El {
    let close = move || {
        shell.modal.set(None);
        shell.focus_active();
    };
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
    let items = el("div").class("items").each(
        move || rows.get().into_iter().enumerate().collect::<Vec<_>>(),
        |(index, row)| (*index, row.key.clone()),
        move |(index, row)| {
            let (index, class, clicked) = (*index, row.class, clicked.clone());
            let item = el("div")
                .class_dyn(move || match shell.chosen.get() == index {
                    true => format!("item {class} chosen"),
                    false => format!("item {class}"),
                })
                .attr("style", &format!("padding-left: {}px", 12. + row.indent))
                .on("click", move |_| clicked(index, Picked::Confirmed));
            for (class, text) in &row.parts {
                item.child(el("div").class(class).text(text));
            }
            item
        },
    );
    let list =
        el("div")
            .class("picker")
            // A press in the list is not one outside it.
            .on("pointerdown", |_| stop_propagation())
            .on("keydown", move |event| {
                let DomEventData::KeyDown(key) = &event.data else {
                    return;
                };
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
                    Key::Named(NamedKey::Escape) => {
                        if !back() {
                            close();
                        }
                    }
                    _ if count == 0 => return,
                    Key::Named(NamedKey::Enter) if key.modifiers.ctrl() => {
                        picked(chosen, Picked::ConfirmedSecondary);
                    }
                    Key::Named(NamedKey::Enter) => picked(chosen, Picked::Confirmed),
                    _ if key.modifiers.ctrl() => picked(chosen, Picked::Chord(key)),
                    // Anything else is the field's.
                    _ => return,
                }
                prevent_default();
                stop_propagation();
            })
            .child(el("div").class("search").child(
                field(input).when(move || shell.query.get().is_empty(), || prompt("Search...")),
            ))
            .child(items);
    el("div")
        .class("scrim")
        .on("pointerdown", move |_| close())
        .child(list)
}
