#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CommandId {
    SplitRight,
    SplitDown,
    NewTab,
    FocusNextPane,
    CloseActivePane,
    CloseActiveTab,
    TerminateActiveSession,
    TerminateAllDetachedSessions,
    ApproveToolCall,
    DenyToolCall,
    CancelAgentTurn,
    ContinueAgentTurn,
    ReloadSessionRuntime,
    OpenSessionManager,
    ReloadConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandCategory {
    Workspace,
    Agent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: &'static str,
    pub category: CommandCategory,
    pub description: &'static str,
    /// Marks a command as destructive (ends a session, discards state, ...)
    /// so surfaces that list commands (the palette) can give it a visually
    /// distinct treatment, per `docs/ux-principles.md`'s "termination should
    /// be explicit and visually distinct from closing a surface". Carried on
    /// the spec rather than matched off `title` so future destructive
    /// commands inherit the treatment automatically.
    pub destructive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandState {
    pub tab_count: usize,
    pub visible_pane_count: usize,
    pub has_active_session: bool,
    pub detached_session_count: usize,
    pub has_pending_approval: bool,
    pub has_turn_in_flight: bool,
    /// Whether the active agent session is sitting on a turn the turn-loop
    /// guard halted -- `CommandId::ContinueAgentTurn`'s enablement signal
    /// (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s
    /// resolution, decision 3). Independent of `has_turn_in_flight`: a
    /// guard halt returns the session to `WaitingForUser`, so the two are
    /// never both true at once for the same session.
    pub has_paused_turn: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEntry {
    pub spec: CommandSpec,
    pub enabled: bool,
}

pub fn core_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CommandId::SplitRight,
            title: "Split Right…",
            category: CommandCategory::Workspace,
            description: "Open the view chooser to split the active pane horizontally.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::SplitDown,
            title: "Split Down…",
            category: CommandCategory::Workspace,
            description: "Open the view chooser to split the active pane vertically.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::NewTab,
            title: "New Tab…",
            category: CommandCategory::Workspace,
            description: "Open the view chooser to open a new tab.",
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
            description: "Close the active tab, detaching its sessions.",
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
            id: CommandId::ContinueAgentTurn,
            title: "Continue Agent Turn",
            category: CommandCategory::Agent,
            description: "Resume a turn the turn-loop guard paused, without a new message.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::ReloadSessionRuntime,
            title: "Reload Session Runtime",
            category: CommandCategory::Agent,
            description: "Restart horizon-sessiond and reconnect every agent session.",
            destructive: false,
        },
        CommandSpec {
            id: CommandId::OpenSessionManager,
            title: "Manage Sessions",
            category: CommandCategory::Workspace,
            description: "Open the session manager to attach or terminate sessions.",
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
        CommandId::SplitRight
        | CommandId::SplitDown
        | CommandId::NewTab
        | CommandId::FocusNextPane
        | CommandId::ReloadSessionRuntime
        | CommandId::OpenSessionManager
        | CommandId::ReloadConfig => true,
        CommandId::CloseActivePane => state.visible_pane_count > 1,
        // Unlike `CloseActivePane` (closing a tab's last pane must go
        // through closing the tab itself instead), closing the
        // workspace's last tab is allowed -- it leaves a valid, empty
        // workspace (2026-07-18 owner clarification), not something to
        // guard against.
        CommandId::CloseActiveTab => state.tab_count > 0,
        CommandId::TerminateActiveSession => state.has_active_session,
        CommandId::TerminateAllDetachedSessions => state.detached_session_count > 0,
        CommandId::ApproveToolCall | CommandId::DenyToolCall => state.has_pending_approval,
        CommandId::CancelAgentTurn => state.has_turn_in_flight,
        CommandId::ContinueAgentTurn => state.has_paused_turn,
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

pub fn filter_command_entries(entries: Vec<CommandEntry>, query: &str) -> Vec<CommandEntry> {
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

fn normalize_query(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_commands_have_stable_ids_and_titles() {
        let commands = core_commands();

        assert_eq!(commands.len(), 15);
        assert_eq!(commands[0].id, CommandId::SplitRight);
        assert_eq!(commands[0].title, "Split Right…");
        assert_eq!(commands[1].id, CommandId::SplitDown);
        assert_eq!(commands[1].title, "Split Down…");
        assert_eq!(commands[2].id, CommandId::NewTab);
        assert_eq!(commands[2].title, "New Tab…");
        assert_eq!(commands[6].id, CommandId::TerminateActiveSession);
        assert_eq!(commands[6].title, "Terminate Active Session");
        assert_eq!(commands[7].id, CommandId::TerminateAllDetachedSessions);
        assert_eq!(commands[7].title, "Terminate All Detached Sessions");
        assert_eq!(commands[10].id, CommandId::CancelAgentTurn);
        assert_eq!(commands[10].title, "Cancel Agent Turn");
        assert_eq!(commands[11].id, CommandId::ContinueAgentTurn);
        assert_eq!(commands[11].title, "Continue Agent Turn");
        assert_eq!(commands[13].id, CommandId::OpenSessionManager);
        assert_eq!(commands[13].title, "Manage Sessions");
        assert_eq!(commands[14].id, CommandId::ReloadConfig);
        assert_eq!(commands[14].title, "Reload Config");
    }

    #[test]
    fn open_session_manager_is_always_enabled() {
        assert!(command_enabled(
            CommandId::OpenSessionManager,
            CommandState {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
                has_paused_turn: false,
            }
        ));
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
    fn close_pane_is_disabled_but_close_tab_is_enabled_for_the_last_tab() {
        // `CloseActivePane` still requires another pane in the tab to
        // fall back to -- closing a tab's sole pane must go through
        // closing the tab itself instead. `CloseActiveTab`, though, is
        // enabled even for the workspace's last tab: closing it is now
        // allowed to leave a valid, empty workspace (2026-07-18 owner
        // clarification), unlike the old "another tab must remain" rule.
        let entries = command_entries(CommandState {
            tab_count: 1,
            visible_pane_count: 1,
            has_active_session: true,
            detached_session_count: 0,
            has_pending_approval: false,
            has_turn_in_flight: false,
            has_paused_turn: false,
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
        assert!(close_tab.enabled);
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
                has_paused_turn: false,
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
                has_paused_turn: false,
            }
        ));
    }

    #[test]
    fn close_active_tab_is_disabled_once_the_workspace_is_already_empty() {
        // Nothing to close once the workspace itself has zero tabs.
        assert!(!command_enabled(
            CommandId::CloseActiveTab,
            CommandState {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
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
                has_paused_turn: false,
            }
        ));
    }

    #[test]
    fn continue_agent_turn_requires_a_paused_turn() {
        assert!(!command_enabled(
            CommandId::ContinueAgentTurn,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
                has_paused_turn: false,
            }
        ));
        assert!(command_enabled(
            CommandId::ContinueAgentTurn,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
                has_paused_turn: true,
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
            has_paused_turn: false,
        });

        let split = filter_command_entries(entries.clone(), "split right");
        assert_eq!(split.len(), 1);
        assert_eq!(split[0].spec.id, CommandId::SplitRight);

        let new_tab = filter_command_entries(entries, "new tab");
        assert_eq!(new_tab.len(), 1);
        assert_eq!(new_tab[0].spec.id, CommandId::NewTab);
    }
}
