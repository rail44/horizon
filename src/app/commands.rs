#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CommandId {
    NewTerminal,
    NewAgent,
    SplitActivePane,
    FocusNextPane,
    CloseActivePane,
    CloseActiveTab,
    TerminateActiveSession,
    TerminateAllDetachedSessions,
    ApproveToolCall,
    DenyToolCall,
    CancelAgentTurn,
    ReloadAgentRuntime,
    ReloadConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandCategory {
    Workspace,
    Terminal,
    Agent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandSpec {
    pub(crate) id: CommandId,
    pub(crate) title: &'static str,
    pub(crate) category: CommandCategory,
    pub(crate) description: &'static str,
    /// Marks a command as destructive (ends a session, discards state, ...)
    /// so surfaces that list commands (the palette) can give it a visually
    /// distinct treatment, per `docs/ux-principles.md`'s "termination should
    /// be explicit and visually distinct from closing a surface". Carried on
    /// the spec rather than matched off `title` so future destructive
    /// commands inherit the treatment automatically.
    pub(crate) destructive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandState {
    pub(crate) tab_count: usize,
    pub(crate) visible_pane_count: usize,
    pub(crate) has_active_session: bool,
    pub(crate) detached_session_count: usize,
    pub(crate) has_pending_approval: bool,
    pub(crate) has_turn_in_flight: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandEntry {
    pub(crate) spec: CommandSpec,
    pub(crate) enabled: bool,
}

pub(crate) fn core_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CommandId::NewTerminal,
            title: "New Terminal",
            category: CommandCategory::Terminal,
            description: "Open a new terminal tab.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::NewAgent,
            title: "New Agent",
            category: CommandCategory::Agent,
            description: "Open a new agent tab.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::SplitActivePane,
            title: "Split Active Pane",
            category: CommandCategory::Workspace,
            description: "Split the active pane in the current tab.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::FocusNextPane,
            title: "Focus Next Pane",
            category: CommandCategory::Workspace,
            description: "Move focus to the next pane in the active tab.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::CloseActivePane,
            title: "Close Active Pane",
            category: CommandCategory::Workspace,
            description: "Close the active pane when another pane remains.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::CloseActiveTab,
            title: "Close Active Tab",
            category: CommandCategory::Workspace,
            description: "Close the active tab when another tab remains.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::TerminateActiveSession,
            title: "Terminate Active Session",
            category: CommandCategory::Workspace,
            description: "Terminate the active session and close its panes.",
            destructive: true,
        },
        CommandSpec {
            id: CommandId::TerminateAllDetachedSessions,
            title: "Terminate All Detached Sessions",
            category: CommandCategory::Workspace,
            description: "Terminate every detached session (not attached to any pane).",
            destructive: true,
        },
        CommandSpec {
            id: CommandId::ApproveToolCall,
            title: "Approve Pending Tool Call",
            category: CommandCategory::Agent,
            description: "Approve the pending tool call awaiting approval.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::DenyToolCall,
            title: "Deny Pending Tool Call",
            category: CommandCategory::Agent,
            description: "Deny the pending tool call awaiting approval.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::CancelAgentTurn,
            title: "Cancel Agent Turn",
            category: CommandCategory::Agent,
            description: "Cancel the agent turn currently in flight.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::ReloadAgentRuntime,
            title: "Reload Agent Runtime",
            category: CommandCategory::Agent,
            description: "Restart horizon-agentd and reconnect every agent session.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::ReloadConfig,
            title: "Reload Config",
            category: CommandCategory::Workspace,
            description: "Re-read the config file and apply theme and keybindings live.",
            destructive: false,
        },
    ]
}

pub(crate) fn command_enabled(command_id: CommandId, state: CommandState) -> bool {
    match command_id {
        CommandId::NewTerminal
        | CommandId::NewAgent
        | CommandId::SplitActivePane
        | CommandId::FocusNextPane
        | CommandId::ReloadAgentRuntime
        | CommandId::ReloadConfig => true,
        CommandId::CloseActivePane => state.visible_pane_count > 1,
        CommandId::CloseActiveTab => state.tab_count > 1,
        CommandId::TerminateActiveSession => state.has_active_session,
        CommandId::TerminateAllDetachedSessions => state.detached_session_count > 0,
        CommandId::ApproveToolCall | CommandId::DenyToolCall => state.has_pending_approval,
        CommandId::CancelAgentTurn => state.has_turn_in_flight,
    }
}

pub(crate) fn command_entries(state: CommandState) -> Vec<CommandEntry> {
    core_commands()
        .into_iter()
        .map(|spec| CommandEntry {
            enabled: command_enabled(spec.id, state),
            spec,
        })
        .collect()
}

pub(crate) fn filter_command_entries(entries: Vec<CommandEntry>, query: &str) -> Vec<CommandEntry> {
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
        .collect()
}

pub(crate) fn clamp_palette_selection(selection: usize, item_count: usize) -> usize {
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

        assert_eq!(commands.len(), 13);
        assert_eq!(commands[0].id, CommandId::NewTerminal);
        assert_eq!(commands[0].title, "New Terminal");
        assert_eq!(commands[6].id, CommandId::TerminateActiveSession);
        assert_eq!(commands[6].title, "Terminate Active Session");
        assert_eq!(commands[7].id, CommandId::TerminateAllDetachedSessions);
        assert_eq!(commands[7].title, "Terminate All Detached Sessions");
    }

    #[test]
    fn core_commands_have_descriptions() {
        for command in core_commands() {
            assert!(!command.title.is_empty());
            assert!(!command.description.is_empty());
        }
    }

    #[test]
    fn only_terminate_commands_are_marked_destructive() {
        for command in core_commands() {
            let expected = matches!(
                command.id,
                CommandId::TerminateActiveSession | CommandId::TerminateAllDetachedSessions
            );
            assert_eq!(
                command.destructive, expected,
                "{:?} should only be destructive if it terminates session(s)",
                command.id
            );
        }
    }

    #[test]
    fn close_commands_are_disabled_for_single_tab_single_pane() {
        let entries = command_entries(CommandState {
            tab_count: 1,
            visible_pane_count: 1,
            has_active_session: true,
            detached_session_count: 0,
            has_pending_approval: false,
            has_turn_in_flight: false,
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
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
        assert!(command_enabled(
            CommandId::CloseActiveTab,
            CommandState {
                tab_count: 2,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
    }

    #[test]
    fn terminate_active_session_requires_active_session() {
        assert!(!command_enabled(
            CommandId::TerminateActiveSession,
            CommandState {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
        assert!(command_enabled(
            CommandId::TerminateActiveSession,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
    }

    #[test]
    fn terminate_all_detached_sessions_requires_detached_sessions() {
        assert!(!command_enabled(
            CommandId::TerminateAllDetachedSessions,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
        assert!(command_enabled(
            CommandId::TerminateAllDetachedSessions,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 2,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
    }

    #[test]
    fn approve_and_deny_tool_call_require_pending_approval() {
        assert!(!command_enabled(
            CommandId::ApproveToolCall,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
        assert!(command_enabled(
            CommandId::ApproveToolCall,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: true,
                has_turn_in_flight: false,
            }
        ));
        assert!(!command_enabled(
            CommandId::DenyToolCall,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
        assert!(command_enabled(
            CommandId::DenyToolCall,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: true,
                has_turn_in_flight: false,
            }
        ));
    }

    #[test]
    fn cancel_agent_turn_requires_turn_in_flight() {
        assert!(!command_enabled(
            CommandId::CancelAgentTurn,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        ));
        assert!(command_enabled(
            CommandId::CancelAgentTurn,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: true,
            }
        ));
    }

    #[test]
    fn filter_command_entries_matches_title_and_description() {
        let entries = command_entries(CommandState {
            tab_count: 2,
            visible_pane_count: 2,
            has_active_session: true,
            detached_session_count: 0,
            has_pending_approval: false,
            has_turn_in_flight: false,
        });

        let terminal = filter_command_entries(entries.clone(), "terminal");
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].spec.id, CommandId::NewTerminal);

        let current = filter_command_entries(entries, "current tab");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].spec.id, CommandId::SplitActivePane);
    }

    #[test]
    fn clamp_palette_selection_stays_in_bounds() {
        assert_eq!(clamp_palette_selection(5, 0), 0);
        assert_eq!(clamp_palette_selection(5, 2), 1);
        assert_eq!(clamp_palette_selection(1, 2), 1);
    }
}
