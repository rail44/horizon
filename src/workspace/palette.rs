//! The palette: the commands, narrowed by what is typed, the chosen one
//! run with Enter. The commands of the plugin the cursor's pane shows
//! come first, then the shell's own.

use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, Track};
use horizon_workspace::Workspace;
use horizon_workspace::commands::{CommandId, command_entries, filter_command_entries};

use super::picker::{self, Picked, Row};
use super::{Shell, plugins, styles};

/// What a palette row carries out.
#[derive(Clone, Copy)]
enum Run {
    Shell(CommandId),
    /// The command of this name of the cursor's pane's plugin.
    Plugin(&'static str),
}

struct Entry {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    destructive: bool,
    enabled: bool,
    run: Run,
}

/// The commands that match `query`, in the palette's order.
fn matching(shell: Shell, query: &str) -> Vec<Entry> {
    let mine = shell.cursor_plugin_commands();
    let query_lower = query.trim().to_ascii_lowercase();
    let plugin = (plugins::commands().into_iter())
        .filter(|listed| mine.contains(&listed.name))
        .filter(|listed| {
            query_lower.is_empty()
                || listed.title.to_ascii_lowercase().contains(&query_lower)
                || listed
                    .description
                    .to_ascii_lowercase()
                    .contains(&query_lower)
        })
        .map(|listed| Entry {
            name: listed.name,
            title: listed.title,
            description: listed.description,
            destructive: listed.destructive,
            enabled: shell.plugin_command_enabled(listed.name),
            run: Run::Plugin(listed.name),
        });
    let own = filter_command_entries(command_entries(shell.command_state()), query);
    let own = own.into_iter().map(|entry| Entry {
        name: entry.spec.name,
        title: entry.spec.title,
        description: entry.spec.description,
        destructive: entry.spec.destructive,
        enabled: entry.enabled,
        run: Run::Shell(entry.spec.id),
    });
    plugin.chain(own).collect()
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
    let picked = move |index: usize, picked: Picked| {
        let entries = matching(shell, &shell.query.get_untracked());
        let Some(entry) = entries.get(index).filter(|entry| entry.enabled) else {
            return;
        };
        if !matches!(picked, Picked::Confirmed) {
            return;
        }
        shell.modal.set(None);
        match entry.run {
            Run::Shell(command) => shell.execute(command),
            Run::Plugin(name) => _ = shell.execute_plugin(name),
        }
        if shell.modal.get_untracked().is_none() {
            shell.change(Workspace::exit_workspace_mode);
        }
    };
    picker::view(shell, rows, picked)
}
