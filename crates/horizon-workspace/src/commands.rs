/// Declares [`CommandId`] and [`CommandId::ALL`] from one list, so that the
/// list the palette and the keymap read holds every command: Rust has no way
/// to enumerate an enum's variants, and a list kept beside the enum could
/// miss one without the compiler noticing. The order is the palette's. The
/// enum's declaration is written at the call, so that its name is in the
/// source (indexers place the definition there).
macro_rules! command_ids {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$meta:meta])* $variant:ident,)*
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $($(#[$meta])* $variant,)*
        }

        impl $name {
            /// Every command, in the order the palette lists them.
            pub const ALL: &'static [$name] = &[$($name::$variant,)*];
        }
    };
}

command_ids! {
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
        ReloadAgentRuntime,
        /// Restarts `horizon-terminald` (`docs/terminald-split-design.md`
        /// decision 3). Deliberately separate from
        /// [`CommandId::ReloadAgentRuntime`] and deliberately destructive:
        /// picking up a `horizon-terminal-core` change means replacing the
        /// process that owns every PTY, so every terminal session -- and
        /// whatever is running inside it -- ends. The close-vs-terminate
        /// discipline applies to daemons too.
        ReloadTerminalRuntime,
        OpenSessionManager,
        ReloadConfig,
        /// Opens a new terminal tab whose cwd is the active session's
        /// `directory` (`docs/session-relationship-design.md` decision
        /// 4a) -- v1's "hunt down the agent's worktree and cd there" fix,
        /// scoped to the active session only.
        OpenTerminalInSessionDirectory,
        /// Opens the board's task list (the `board-list` plugin): a
        /// session-less first-party view over the `horizon-board` event store where the owner
        /// can browse the ranked task hierarchy. The pane is placed as a new
        /// tab or split via the view chooser's placement flow. Palette-only --
        /// no default keybinding (binding id `"open-board"`).
        OpenBoard,
        /// Moves the live terminal font size up one step (the shell crate's
        /// `terminal::FONT_SIZE_STEP` px, clamped). Affects the terminal grid
        /// and the agent transcript's base text size without a restart.
        /// Bound by default to the macOS-standard zoom chords (see the shell
        /// crate's `workspace::bindings`); a `[keybindings]` entry can
        /// override via `"increase-font-size"`.
        IncreaseFontSize,
        /// The counterpart of [`CommandId::IncreaseFontSize`]: one step down,
        /// clamped. Default chord / override id: `"decrease-font-size"`.
        DecreaseFontSize,
        /// Restores the startup-configured font size (`[terminal] font_size`
        /// or the built-in default), ending a zoom session. Default chord /
        /// override id: `"reset-font-size"`.
        ResetFontSize,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandCategory {
    Workspace,
    Agent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub id: CommandId,
    /// The command's stable kebab-case id, which a `[keybindings]` entry
    /// names it by (the shell's `keymap::command_for`).
    pub name: &'static str,
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
    /// Whether the active session has a known `directory` --
    /// `CommandId::OpenTerminalInSessionDirectory`'s enablement signal
    /// (`docs/session-relationship-design.md` decision 4a). Kept as a
    /// bare bool here (mirroring every other enablement flag on this
    /// struct) rather than carrying the path itself, which stays
    /// `Copy`-friendly and keeps the actual value's resolution
    /// (`Workspace::session_directory`) at the one call site that
    /// needs it (`WorkspaceShell::execute`).
    pub has_active_session_directory: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEntry {
    pub spec: CommandSpec,
    pub enabled: bool,
}

impl CommandId {
    /// The command's palette entry and keybinding name. An exhaustive match,
    /// so a new command does not compile until it has both.
    pub fn spec(self) -> CommandSpec {
        let (name, title, category, description) = match self {
            CommandId::SplitRight => (
                "split-right",
                "Split Right…",
                CommandCategory::Workspace,
                "Open the view chooser to split the active pane horizontally.",
            ),
            CommandId::SplitDown => (
                "split-down",
                "Split Down…",
                CommandCategory::Workspace,
                "Open the view chooser to split the active pane vertically.",
            ),
            CommandId::NewTab => (
                "new-tab",
                "New Tab…",
                CommandCategory::Workspace,
                "Open the view chooser to open a new tab.",
            ),
            CommandId::FocusNextPane => (
                "focus-next-pane",
                "Focus Next Pane",
                CommandCategory::Workspace,
                "Move focus to the next pane in the active tab.",
            ),
            CommandId::CloseActivePane => (
                "close-active-pane",
                "Close Active Pane",
                CommandCategory::Workspace,
                "Close the active pane when another pane remains.",
            ),
            CommandId::CloseActiveTab => (
                "close-active-tab",
                "Close Active Tab",
                CommandCategory::Workspace,
                "Close the active tab, detaching its sessions.",
            ),
            CommandId::TerminateActiveSession => (
                "terminate-active-session",
                "Terminate Active Session",
                CommandCategory::Workspace,
                "Terminate the active session and close its panes.",
            ),
            CommandId::TerminateAllDetachedSessions => (
                "terminate-all-detached-sessions",
                "Terminate All Detached Sessions",
                CommandCategory::Workspace,
                "Terminate every detached session (not attached to any pane).",
            ),
            CommandId::ReloadAgentRuntime => (
                "reload-agent-runtime",
                "Reload Agent Runtime",
                CommandCategory::Agent,
                "Restart horizon-agentd and reconnect every agent session. \
                          Terminals are unaffected.",
            ),
            CommandId::ReloadTerminalRuntime => (
                "reload-terminal-runtime",
                "Reload Terminal Runtime",
                CommandCategory::Workspace,
                "Restart horizon-terminald. Terminates every terminal session and its \
                          shell.",
            ),
            CommandId::OpenSessionManager => (
                "manage-sessions",
                "Manage Sessions",
                CommandCategory::Workspace,
                "Open the session manager to attach or terminate sessions.",
            ),
            CommandId::ReloadConfig => (
                "reload-config",
                "Reload Config",
                CommandCategory::Workspace,
                "Re-read the config file and apply theme and keybindings live.",
            ),
            CommandId::OpenTerminalInSessionDirectory => (
                "open-terminal-in-session-directory",
                "Open Terminal in Session Directory",
                CommandCategory::Workspace,
                "Open a new terminal whose working directory is the active session's directory.",
            ),
            CommandId::OpenBoard => (
                "open-board",
                "Open Board",
                CommandCategory::Workspace,
                "Browse the task board's ranked task list.",
            ),
            CommandId::IncreaseFontSize => (
                "increase-font-size",
                "Increase Font Size",
                CommandCategory::Workspace,
                "Increase the terminal and agent transcript font size.",
            ),
            CommandId::DecreaseFontSize => (
                "decrease-font-size",
                "Decrease Font Size",
                CommandCategory::Workspace,
                "Decrease the terminal and agent transcript font size.",
            ),
            CommandId::ResetFontSize => (
                "reset-font-size",
                "Reset Font Size",
                CommandCategory::Workspace,
                "Restore the startup-configured default font size.",
            ),
        };
        CommandSpec {
            id: self,
            name,
            title,
            category,
            description,
            destructive: matches!(
                self,
                CommandId::TerminateActiveSession
                    | CommandId::TerminateAllDetachedSessions
                    | CommandId::ReloadTerminalRuntime
            ),
        }
    }
}

pub fn core_commands() -> Vec<CommandSpec> {
    CommandId::ALL.iter().map(|id| id.spec()).collect()
}

pub fn command_enabled(command_id: CommandId, state: CommandState) -> bool {
    match command_id {
        // Both split placements target the active tab/session
        // (`split_active_tab_with_view`/`split_session_with_new_session` in
        // `horizon-workspace::operations`); with zero tabs there is neither,
        // so the view chooser used to confirm and silently no-op (backlog
        // 49). `NewTab` stays unconditionally enabled -- it is the empty
        // workspace's only way back.
        CommandId::SplitRight | CommandId::SplitDown => state.tab_count > 0,
        CommandId::NewTab
        | CommandId::FocusNextPane
        | CommandId::ReloadAgentRuntime
        | CommandId::ReloadTerminalRuntime
        | CommandId::OpenSessionManager
        | CommandId::ReloadConfig
        | CommandId::OpenBoard
        | CommandId::IncreaseFontSize
        | CommandId::DecreaseFontSize
        | CommandId::ResetFontSize => true,
        CommandId::CloseActivePane => state.visible_pane_count > 1,
        // Unlike `CloseActivePane` -- workspace-mode `x` falls through to
        // closing the tab when the pane is the last one
        // (`Workspace::close_cursor_pane_or_tab`), but a palette-invoked
        // pane command must not implicitly close the tab -- closing the
        // workspace's last tab is allowed: it leaves a valid, empty
        // workspace (2026-07-18 owner clarification), not something to
        // guard against.
        CommandId::CloseActiveTab => state.tab_count > 0,
        CommandId::TerminateActiveSession => state.has_active_session,
        CommandId::TerminateAllDetachedSessions => state.detached_session_count > 0,
        CommandId::OpenTerminalInSessionDirectory => state.has_active_session_directory,
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

        assert_eq!(commands.len(), 17);
        let ids: Vec<CommandId> = commands.iter().map(|command| command.id).collect();
        assert_eq!(
            ids,
            [
                CommandId::SplitRight,
                CommandId::SplitDown,
                CommandId::NewTab,
                CommandId::FocusNextPane,
                CommandId::CloseActivePane,
                CommandId::CloseActiveTab,
                CommandId::TerminateActiveSession,
                CommandId::TerminateAllDetachedSessions,
                CommandId::ReloadAgentRuntime,
                CommandId::ReloadTerminalRuntime,
                CommandId::OpenSessionManager,
                CommandId::ReloadConfig,
                CommandId::OpenTerminalInSessionDirectory,
                CommandId::OpenBoard,
                CommandId::IncreaseFontSize,
                CommandId::DecreaseFontSize,
                CommandId::ResetFontSize,
            ]
        );
        assert_eq!(commands[0].title, "Split Right…");
        assert_eq!(commands[2].title, "New Tab…");
        assert_eq!(commands[6].title, "Terminate Active Session");
        assert_eq!(commands[9].title, "Reload Terminal Runtime");
        assert_eq!(commands[10].title, "Manage Sessions");
        assert_eq!(commands[13].title, "Open Board");
        assert_eq!(commands[16].title, "Reset Font Size");
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
                has_active_session_directory: false,
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
    fn only_session_ending_commands_are_marked_destructive() {
        // `ReloadTerminalRuntime` belongs here with the two explicit
        // terminates: restarting `horizon-terminald` ends every terminal
        // session and the processes inside them
        // (`docs/terminald-split-design.md` decision 3). Its sibling
        // `ReloadAgentRuntime` does not -- since the split it restarts
        // only the agent daemon, whose sessions are resumed from the event
        // log rather than killed.
        for command in core_commands() {
            let expected = matches!(
                command.id,
                CommandId::TerminateActiveSession
                    | CommandId::TerminateAllDetachedSessions
                    | CommandId::ReloadTerminalRuntime
            );
            assert_eq!(
                command.destructive, expected,
                "{:?} should only be destructive if it ends session(s)",
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
            has_active_session_directory: false,
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
                has_active_session_directory: false,
            }
        ));
        assert!(command_enabled(
            CommandId::CloseActiveTab,
            CommandState {
                tab_count: 2,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_active_session_directory: false,
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
                has_active_session_directory: false,
            }
        ));
    }

    #[test]
    fn split_placements_are_disabled_once_the_workspace_is_already_empty() {
        // Nothing to split from once the workspace itself has zero tabs --
        // the view chooser used to confirm and silently no-op (backlog 49).
        assert!(!command_enabled(
            CommandId::SplitRight,
            CommandState {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_active_session_directory: false,
            }
        ));
        assert!(!command_enabled(
            CommandId::SplitDown,
            CommandState {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_active_session_directory: false,
            }
        ));
        assert!(command_enabled(
            CommandId::SplitRight,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_active_session_directory: false,
            }
        ));
        assert!(command_enabled(
            CommandId::SplitDown,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_active_session_directory: false,
            }
        ));
    }

    #[test]
    fn new_tab_stays_enabled_once_the_workspace_is_already_empty() {
        // Unlike the split placements, `NewTab` must stay reachable at zero
        // tabs -- it is the empty workspace's only way back (see
        // `open_session_manager_is_always_enabled` for the same reasoning
        // applied to the session manager).
        assert!(command_enabled(
            CommandId::NewTab,
            CommandState {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_active_session_directory: false,
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
                has_active_session_directory: false,
            }
        ));
        assert!(command_enabled(
            CommandId::TerminateActiveSession,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_active_session_directory: false,
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
                has_active_session_directory: false,
            }
        ));
        assert!(command_enabled(
            CommandId::TerminateAllDetachedSessions,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 2,
                has_active_session_directory: false,
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
            has_active_session_directory: false,
        });

        let split = filter_command_entries(entries.clone(), "split right");
        assert_eq!(split.len(), 1);
        assert_eq!(split[0].spec.id, CommandId::SplitRight);

        let new_tab = filter_command_entries(entries, "new tab");
        assert_eq!(new_tab.len(), 1);
        assert_eq!(new_tab[0].spec.id, CommandId::NewTab);
    }

    #[test]
    fn open_terminal_in_session_directory_requires_a_known_directory() {
        // `docs/session-relationship-design.md` decision 4a: disabled with
        // no active session, and disabled just the same when there is one
        // but its `directory` isn't known (a terminal whose cwd has not
        // been published yet, or a resumed agent session) -- only enabled
        // once both an active session and a known `directory` line up.
        assert!(!command_enabled(
            CommandId::OpenTerminalInSessionDirectory,
            CommandState {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_active_session_directory: false,
            }
        ));
        assert!(!command_enabled(
            CommandId::OpenTerminalInSessionDirectory,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_active_session_directory: false,
            }
        ));
        assert!(command_enabled(
            CommandId::OpenTerminalInSessionDirectory,
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                detached_session_count: 0,
                has_active_session_directory: true,
            }
        ));
    }
}
