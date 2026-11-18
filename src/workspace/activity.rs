//! What a session is doing, as the shell takes it in from the two
//! runtimes and as it words it: the mark before a tab's title, the line in
//! the session manager, and the entry `horizon sessions` answers with.

use horizon_control::contract::ActivityEntry;
use horizon_terminal_core::{ProgramBlockedOn, TerminalProgramStatus};
use horizon_workspace::types::{
    AgentStatus, Attention, BlockedOn, Doing, ProgramState, ProgramStatus, SessionActivity,
};

use crate::board::activity::BoardSessionActivity;

/// The longest message a session's line carries, in chars.
const MESSAGE_CHARS: usize = 60;

/// A program's report as the terminal runtime sends it, as the model
/// keeps it: what it is doing, and its words about it.
pub(super) fn reported(status: TerminalProgramStatus) -> (Doing, Option<String>) {
    use horizon_terminal_core::ProgramState as Wire;
    let doing = Doing::Program(ProgramStatus {
        state: match status.state {
            Wire::Idle => ProgramState::Idle,
            Wire::Working => ProgramState::Working,
            Wire::Done => ProgramState::Done,
            Wire::Blocked => ProgramState::Blocked,
            Wire::Error => ProgramState::Error,
        },
        blocked_on: status.blocked_on.map(|on| match on {
            ProgramBlockedOn::Permission => BlockedOn::Permission,
            ProgramBlockedOn::Question => BlockedOn::Question,
            ProgramBlockedOn::Auth => BlockedOn::Auth,
        }),
        app: status.app,
    });
    (doing, status.message)
}

/// What an agent session is doing, as the model keeps it.
pub(super) fn of_agent(activity: BoardSessionActivity) -> Doing {
    use BoardSessionActivity as Told;
    Doing::Agent(match activity {
        Told::Loading => AgentStatus::Loading,
        Told::Unavailable => AgentStatus::Unavailable,
        Told::Starting => AgentStatus::Starting,
        Told::Running => AgentStatus::Running,
        Told::ToolRunning => AgentStatus::ToolRunning,
        Told::WaitingForInput => AgentStatus::WaitingForInput,
        Told::WaitingForApproval => AgentStatus::WaitingForApproval,
        Told::Cancelled => AgentStatus::Cancelled,
        Told::Completed => AgentStatus::Completed,
        Told::Failed => AgentStatus::Failed,
        Told::Paused => AgentStatus::Paused,
        Told::Terminated => AgentStatus::Terminated,
    })
}

/// The mark before a tab's title, for a session there is one for. A
/// program that reports leaves out the mark it would put in its own title
/// (Claude Code's `✳` and `◐`), so the two it had are kept as they were,
/// and what it could not say there has a mark of its own. An agent
/// session has the same marks.
pub(super) fn mark(activity: &SessionActivity) -> Option<&'static str> {
    Some(match activity.attention() {
        Some(Attention::Unseen) => "●",
        Some(Attention::Failed) => "!",
        Some(Attention::Approval | Attention::Input | Attention::Login | Attention::Blocked) => "?",
        None if activity.working() => "◐",
        None if activity.present() => "✳",
        None => return None,
    })
}

/// A tab's title with its session's mark before it.
pub(super) fn marked(activity: Option<&SessionActivity>, title: String) -> String {
    match activity.and_then(mark) {
        Some(mark) => format!("{mark} {title}"),
        None => title,
    }
}

/// A session's activity in a line: who, what, waiting for what, and its
/// words.
pub(super) fn line(activity: &SessionActivity) -> String {
    let mut line = String::new();
    if let Doing::Program(ProgramStatus { app: Some(app), .. }) = &activity.doing {
        line.push_str(app);
        line.push_str(": ");
    }
    line.push_str(activity.state());
    if let Some(attention) = activity.attention() {
        line.push_str(&format!(" [{}]", attention.label()));
    }
    if let Some(detail) = &activity.detail {
        line.push_str(" — ");
        match detail.chars().count() > MESSAGE_CHARS {
            true => {
                line.extend(detail.chars().take(MESSAGE_CHARS - 1));
                line.push('…');
            }
            false => line.push_str(detail),
        }
    }
    line
}

/// A session's activity as `horizon sessions` answers with it.
pub(super) fn entry(activity: SessionActivity) -> ActivityEntry {
    ActivityEntry {
        state: activity.state().to_string(),
        attention: activity.attention().map(|on| on.label().to_string()),
        seen: activity.seen,
        since_ms: activity.since_ms,
        app: match &activity.doing {
            Doing::Program(program) => program.app.clone(),
            Doing::Agent(_) => None,
        },
        detail: activity.detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(state: ProgramState) -> SessionActivity {
        SessionActivity {
            doing: Doing::Program(ProgramStatus {
                state,
                blocked_on: None,
                app: Some("claude-code".into()),
            }),
            detail: None,
            since_ms: 0,
            seen: false,
        }
    }

    fn agent(status: AgentStatus) -> SessionActivity {
        SessionActivity {
            doing: Doing::Agent(status),
            detail: None,
            since_ms: 0,
            seen: false,
        }
    }

    #[test]
    fn finished_work_is_marked_as_news_until_it_is_seen() {
        for mut done in [program(ProgramState::Done), agent(AgentStatus::Completed)] {
            assert_eq!(mark(&done), Some("●"));
            assert!(line(&done).ends_with(" [unseen]"), "{}", line(&done));
            done.seen = true;
            assert_eq!(mark(&done), Some("✳"));
            assert!(!line(&done).contains("unseen"), "{}", line(&done));
        }
        assert_eq!(
            line(&program(ProgramState::Done)),
            "claude-code: done [unseen]"
        );
        assert_eq!(line(&agent(AgentStatus::Completed)), "completed [unseen]");
    }

    #[test]
    fn a_session_that_waits_says_what_for() {
        let mut blocked = program(ProgramState::Blocked);
        let Doing::Program(status) = &mut blocked.doing else {
            unreachable!()
        };
        status.blocked_on = Some(BlockedOn::Permission);
        blocked.detail = Some("approve Bash: touch notes.txt".into());
        assert_eq!(mark(&blocked), Some("?"));
        assert_eq!(
            line(&blocked),
            "claude-code: blocked [approval] — approve Bash: touch notes.txt"
        );
        let waiting = agent(AgentStatus::WaitingForApproval);
        assert_eq!(mark(&waiting), Some("?"));
        assert_eq!(line(&waiting), "waiting-for-approval [approval]");
        assert_eq!(entry(waiting).attention.as_deref(), Some("approval"));

        let mut long = program(ProgramState::Error);
        let Doing::Program(status) = &mut long.doing else {
            unreachable!()
        };
        status.app = None;
        long.detail = Some("x".repeat(MESSAGE_CHARS + 1));
        assert_eq!(mark(&long), Some("!"));
        assert_eq!(
            line(&long),
            format!("error [failed] — {}…", "x".repeat(MESSAGE_CHARS - 1))
        );
    }

    #[test]
    fn a_session_at_work_or_at_rest_is_marked_and_one_that_is_not_there_is_not() {
        assert_eq!(mark(&program(ProgramState::Working)), Some("◐"));
        assert_eq!(mark(&agent(AgentStatus::ToolRunning)), Some("◐"));
        assert_eq!(mark(&program(ProgramState::Idle)), Some("✳"));
        assert_eq!(mark(&agent(AgentStatus::WaitingForInput)), Some("✳"));
        for gone in [
            AgentStatus::Loading,
            AgentStatus::Unavailable,
            AgentStatus::Terminated,
        ] {
            assert_eq!(mark(&agent(gone)), None);
        }
        assert_eq!(marked(None, "notes".into()), "notes");
        assert_eq!(
            marked(Some(&agent(AgentStatus::Running)), "notes".into()),
            "◐ notes"
        );
    }
}
