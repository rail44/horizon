//! The palette: the commands, narrowed by what is typed, the chosen one
//! run with Enter. The commands of the plugin the cursor's pane shows
//! come first, then the shell's own.

use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, Track};
use horizon_workspace::Workspace;

use super::commands::Listed;
use super::picker::{self, Row};
use super::{Shell, styles};

/// The commands that match `query`, in the palette's order: by a
/// command's title, its description, or what the shell sorts it under.
fn matching(shell: Shell, query: &str) -> Vec<Listed> {
    let query = query.trim().to_ascii_lowercase();
    let holds = |text: &str| text.to_ascii_lowercase().contains(&query);
    (shell.commands().into_iter())
        .filter(|command| {
            query.is_empty()
                || holds(command.title)
                || holds(command.description)
                || (command.category).is_some_and(|category| holds(&format!("{category:?}")))
        })
        .collect()
}

pub(super) fn view(shell: Shell) -> El {
    let rows = move |query: &str| {
        // What a command can do follows the model.
        shell.model.track();
        (matching(shell, query).into_iter())
            .map(|entry| Row {
                key: format!("{} {}", entry.name, entry.enabled),
                class: match (entry.enabled, entry.destructive) {
                    (false, _) => (styles::COMMAND, Some(styles::DISABLED)),
                    (true, true) => (styles::COMMAND, Some(styles::DESTRUCTIVE)),
                    (true, false) => (styles::COMMAND, None),
                },
                parts: vec![
                    (styles::NAME, entry.title.to_string()),
                    (styles::ABOUT, entry.description.to_string()),
                ],
                indent: 0.,
            })
            .collect()
    };
    // A command is run on the mode's cursor, and the mode ends with it,
    // unless what it did was to open another list.
    let picked = move |index: usize| {
        let entries = matching(shell, &shell.search.query.get_untracked());
        let Some(entry) = entries.get(index).filter(|entry| entry.enabled) else {
            return;
        };
        shell.modal.set(None);
        shell.run(entry);
        if shell.modal.get_untracked().is_none() {
            shell.change(Workspace::exit_workspace_mode);
        }
    };
    picker::view(shell.search, rows, picked, "run")
}
