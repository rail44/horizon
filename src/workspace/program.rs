//! What the program in a terminal session says of its own state, as the
//! shell takes it in from the terminal runtime and as it words it: the
//! mark before a tab's title, and the line in the session manager.

use horizon_terminal_core::{ProgramBlockedOn, TerminalProgramStatus};
use horizon_workspace::types::{BlockedOn, ProgramState, ProgramStatus};

/// The longest message a session's line carries, in chars.
const MESSAGE_CHARS: usize = 60;

/// A report as the terminal runtime sends it, as the model keeps it: not
/// yet looked at.
pub(super) fn arrived(status: TerminalProgramStatus) -> ProgramStatus {
    use horizon_terminal_core::ProgramState as Wire;
    ProgramStatus {
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
        message: status.message,
        seen: false,
    }
}

/// The mark before a tab's title. A program that reports leaves out the
/// mark it would put in its own title (Claude Code's `✳` and `◐`), so the
/// two it had are kept as they were, and what it could not say there has a
/// mark of its own.
pub(super) fn mark(status: &ProgramStatus) -> &'static str {
    match status.state {
        ProgramState::Working => "◐",
        ProgramState::Blocked => "?",
        ProgramState::Error => "!",
        ProgramState::Done if !status.seen => "●",
        ProgramState::Done | ProgramState::Idle => "✳",
    }
}

/// A session's state in a line: who, what, waiting for what, and why.
pub(super) fn line(status: &ProgramStatus) -> String {
    let mut line = String::new();
    if let Some(app) = &status.app {
        line.push_str(app);
        line.push_str(": ");
    }
    line.push_str(status.state.label());
    if let Some(on) = status.blocked_on {
        line.push_str(&format!(" ({})", on.label()));
    }
    if status.state == ProgramState::Done && !status.seen {
        line.push_str(", unseen");
    }
    if let Some(message) = &status.message {
        line.push_str(" — ");
        match message.chars().count() > MESSAGE_CHARS {
            true => {
                line.extend(message.chars().take(MESSAGE_CHARS - 1));
                line.push('…');
            }
            false => line.push_str(message),
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: ProgramState) -> ProgramStatus {
        ProgramStatus {
            state,
            blocked_on: None,
            app: Some("claude-code".into()),
            message: None,
            seen: false,
        }
    }

    #[test]
    fn a_done_program_is_marked_as_news_until_it_is_seen() {
        let mut done = status(ProgramState::Done);
        assert_eq!(mark(&done), "●");
        assert_eq!(line(&done), "claude-code: done, unseen");
        done.seen = true;
        assert_eq!(mark(&done), mark(&status(ProgramState::Idle)));
        assert_eq!(line(&done), "claude-code: done");
    }

    #[test]
    fn a_blocked_program_says_what_it_waits_for() {
        let blocked = ProgramStatus {
            blocked_on: Some(BlockedOn::Permission),
            message: Some("approve Bash: touch notes.txt".into()),
            ..status(ProgramState::Blocked)
        };
        assert_eq!(mark(&blocked), "?");
        assert_eq!(
            line(&blocked),
            "claude-code: blocked (permission) — approve Bash: touch notes.txt"
        );
        let long = ProgramStatus {
            app: None,
            message: Some("x".repeat(MESSAGE_CHARS + 1)),
            ..status(ProgramState::Error)
        };
        assert_eq!(
            line(&long),
            format!("error — {}…", "x".repeat(MESSAGE_CHARS - 1))
        );
    }
}
