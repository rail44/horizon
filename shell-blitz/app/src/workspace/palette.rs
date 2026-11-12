//! The palette: the commands, narrowed by what is typed, the chosen one
//! run with Enter.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set};
use horizon_ui::{El, el, prevent_default, stop_propagation};
use horizon_workspace::Workspace;
use horizon_workspace::commands::{CommandEntry, command_entries, filter_command_entries};
use keyboard_types::{Key, NamedKey};

use super::Shell;
use super::commands::CARRIED_OUT;

/// The commands that match `query`, in the palette's order.
fn matching(shell: Shell, query: &str) -> Vec<CommandEntry> {
    let entries = (command_entries(shell.command_state()).into_iter())
        .filter(|entry| CARRIED_OUT.contains(&entry.spec.id))
        .collect();
    filter_command_entries(entries, query)
}

pub(super) fn view(shell: Shell) -> El {
    // Workspace mode stays on under the palette: leaving the palette
    // without a command goes back to the mode as it was.
    let close = move || {
        shell.palette.set(false);
        shell.focus_active();
    };
    // A command is run on the mode's cursor, and the mode ends with it.
    let run = move |entry: &CommandEntry| {
        if !entry.enabled {
            return;
        }
        shell.palette.set(false);
        shell.execute(entry.spec.id);
        shell.change(Workspace::exit_workspace_mode);
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
    let items = el("div").each(
        move || {
            (matching(shell, &shell.query.get()).into_iter())
                .enumerate()
                .collect::<Vec<_>>()
        },
        |(index, entry)| (*index, entry.spec.id, entry.enabled),
        move |(index, entry)| {
            let (index, entry) = (*index, entry.clone());
            let kind = match (entry.enabled, entry.spec.destructive) {
                (false, _) => "item disabled",
                (true, true) => "item destructive",
                (true, false) => "item",
            };
            let clicked = entry.clone();
            el("div")
                .class_dyn(move || match shell.chosen.get() == index {
                    true => format!("{kind} chosen"),
                    false => kind.to_string(),
                })
                .child(el("div").class("name").text(entry.spec.title))
                .child(el("div").class("about").text(entry.spec.description))
                .on("click", move |_| run(&clicked))
        },
    );
    el("div").class("scrim").child(
        el("div")
            .class("palette")
            .on("keydown", move |event| {
                let DomEventData::KeyDown(key) = &event.data else {
                    return;
                };
                let matching = matching(shell, &shell.query.get_untracked());
                let chosen = shell.chosen.get_untracked();
                let (down, up) = (
                    (chosen + 1).min(matching.len().saturating_sub(1)),
                    chosen.saturating_sub(1),
                );
                match &key.key {
                    Key::Named(NamedKey::ArrowDown) => shell.chosen.set(down),
                    Key::Named(NamedKey::ArrowUp) => shell.chosen.set(up),
                    Key::Named(NamedKey::Tab) if key.modifiers.shift() => shell.chosen.set(up),
                    Key::Named(NamedKey::Tab) => shell.chosen.set(down),
                    Key::Named(NamedKey::Enter) => {
                        if let Some(entry) = matching.get(chosen) {
                            run(entry);
                        }
                    }
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
                    move || shell.query.get().is_empty(),
                    || el("div").class("placeholder").text("Search..."),
                ),
            )
            .child(items),
    )
}
