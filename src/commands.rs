#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CommandId {
    NewTerminal,
    NewAgent,
    SplitActivePane,
    FocusNextPane,
    CloseActivePane,
    CloseActiveTab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandCategory {
    Workspace,
    Terminal,
    Agent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: &'static str,
    pub category: CommandCategory,
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandState {
    pub tab_count: usize,
    pub visible_pane_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEntry {
    pub spec: CommandSpec,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaletteItem {
    pub entry: CommandEntry,
}

pub fn core_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CommandId::NewTerminal,
            title: "New Terminal",
            category: CommandCategory::Terminal,
            description: "Open a new terminal tab.",
        },
        CommandSpec {
            id: CommandId::NewAgent,
            title: "New Agent",
            category: CommandCategory::Agent,
            description: "Open a new agent tab.",
        },
        CommandSpec {
            id: CommandId::SplitActivePane,
            title: "Split Active Pane",
            category: CommandCategory::Workspace,
            description: "Split the active pane in the current tab.",
        },
        CommandSpec {
            id: CommandId::FocusNextPane,
            title: "Focus Next Pane",
            category: CommandCategory::Workspace,
            description: "Move focus to the next pane in the active tab.",
        },
        CommandSpec {
            id: CommandId::CloseActivePane,
            title: "Close Active Pane",
            category: CommandCategory::Workspace,
            description: "Close the active pane when another pane remains.",
        },
        CommandSpec {
            id: CommandId::CloseActiveTab,
            title: "Close Active Tab",
            category: CommandCategory::Workspace,
            description: "Close the active tab when another tab remains.",
        },
    ]
}

pub fn command_enabled(command_id: CommandId, state: CommandState) -> bool {
    match command_id {
        CommandId::NewTerminal
        | CommandId::NewAgent
        | CommandId::SplitActivePane
        | CommandId::FocusNextPane => true,
        CommandId::CloseActivePane => state.visible_pane_count > 1,
        CommandId::CloseActiveTab => state.tab_count > 1,
    }
}

pub fn command_entries(state: CommandState) -> Vec<CommandEntry> {
    core_commands()
        .into_iter()
        .map(|spec| CommandEntry {
            enabled: command_enabled(spec.id, state),
            spec,
        })
        .collect()
}

pub fn filter_command_entries(entries: Vec<CommandEntry>, query: &str) -> Vec<PaletteItem> {
    let query = normalize_query(query);
    entries
        .into_iter()
        .filter(|entry| {
            if query.is_empty() {
                return true;
            }
            normalize_query(entry.spec.title).contains(&query)
                || normalize_query(entry.spec.description).contains(&query)
                || format!("{:?}", entry.spec.category)
                    .to_ascii_lowercase()
                    .contains(&query)
        })
        .map(|entry| PaletteItem { entry })
        .collect()
}

pub fn clamp_palette_selection(selection: usize, item_count: usize) -> usize {
    if item_count == 0 {
        return 0;
    }

    selection.min(item_count - 1)
}

fn normalize_query(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_commands_have_stable_ids_and_titles() {
        let commands = core_commands();

        assert_eq!(commands.len(), 6);
        assert_eq!(commands[0].id, CommandId::NewTerminal);
        assert_eq!(commands[0].title, "New Terminal");
        assert_eq!(commands[5].id, CommandId::CloseActiveTab);
        assert_eq!(commands[5].title, "Close Active Tab");
    }

    #[test]
    fn core_commands_have_descriptions() {
        for command in core_commands() {
            assert!(!command.title.is_empty());
            assert!(!command.description.is_empty());
        }
    }

    #[test]
    fn close_commands_are_disabled_for_single_tab_single_pane() {
        let entries = command_entries(CommandState {
            tab_count: 1,
            visible_pane_count: 1,
        });

        let close_pane = entries
            .iter()
            .find(|entry| entry.spec.id == CommandId::CloseActivePane)
            .expect("close pane command");
        let close_tab = entries
            .iter()
            .find(|entry| entry.spec.id == CommandId::CloseActiveTab)
            .expect("close tab command");
        assert!(!close_pane.enabled);
        assert!(!close_tab.enabled);
    }

    #[test]
    fn close_commands_enable_when_targets_exist() {
        assert!(command_enabled(
            CommandId::CloseActivePane,
            CommandState {
                tab_count: 1,
                visible_pane_count: 2,
            }
        ));
        assert!(command_enabled(
            CommandId::CloseActiveTab,
            CommandState {
                tab_count: 2,
                visible_pane_count: 1,
            }
        ));
    }

    #[test]
    fn filter_command_entries_matches_title_and_description() {
        let entries = command_entries(CommandState {
            tab_count: 2,
            visible_pane_count: 2,
        });

        let terminal = filter_command_entries(entries.clone(), "terminal");
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].entry.spec.id, CommandId::NewTerminal);

        let current = filter_command_entries(entries, "current tab");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].entry.spec.id, CommandId::SplitActivePane);
    }

    #[test]
    fn clamp_palette_selection_stays_in_bounds() {
        assert_eq!(clamp_palette_selection(5, 0), 0);
        assert_eq!(clamp_palette_selection(5, 2), 1);
        assert_eq!(clamp_palette_selection(1, 2), 1);
    }
}
