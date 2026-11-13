//! The palette: the commands, narrowed by what is typed, the chosen one
//! run with Enter.

use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, Track};
use horizon_workspace::Workspace;
use horizon_workspace::commands::{CommandEntry, command_entries, filter_command_entries};

use super::Shell;
use super::picker::{self, Picked, Row};

/// The commands that match `query`, in the palette's order.
fn matching(shell: Shell, query: &str) -> Vec<CommandEntry> {
    filter_command_entries(command_entries(shell.command_state()), query)
}

pub(super) fn view(shell: Shell) -> El {
    let rows = move |query: &str| {
        // What a command can do follows the model.
        shell.model.track();
        (matching(shell, query).into_iter())
            .map(|entry| Row {
                key: format!("{} {}", entry.spec.name, entry.enabled),
                class: match (entry.enabled, entry.spec.destructive) {
                    (false, _) => "command disabled",
                    (true, true) => "command destructive",
                    (true, false) => "command",
                },
                parts: vec![
                    ("name", entry.spec.title.to_string()),
                    ("about", entry.spec.description.to_string()),
                ],
                indent: 0.,
            })
            .collect()
    };
    // A command is run on the mode's cursor, and the mode ends with it,
    // unless what it did was to open another list.
    let picked = move |index: usize, picked: Picked| {
        let entries = matching(shell, &shell.query.get_untracked());
        let Some(entry) = entries.get(index).filter(|entry| entry.enabled) else {
            return;
        };
        if !matches!(picked, Picked::Confirmed) {
            return;
        }
        shell.modal.set(None);
        shell.execute(entry.spec.id);
        if shell.modal.get_untracked().is_none() {
            shell.change(Workspace::exit_workspace_mode);
        }
    };
    picker::view(shell, rows, picked)
}
