//! Client-side confirmation for destructive subcommands -- the design
//! doc's "Authorization" decision: the server exposes destructiveness via
//! `State::destructive_commands`; a CLI front-end is expected to prompt
//! interactively, or require an explicit acknowledgment flag headlessly.
//!
//! [`resolve`] is the pure decision (testable with a canned `ask` callback,
//! no real tty needed); [`interactive_prompt`] is the one real side-effecting
//! implementation `main.rs`/`crate::run`'s production caller passes in.

use std::io::{BufRead, Write};

/// Decides whether a destructive action -- already confirmed to be one via
/// a live `state` query, see `crate::run` -- may proceed.
///
/// `--yes` always wins (interactive or not). Otherwise: an interactive
/// (`stdin_is_tty`) run asks via `ask`; a non-interactive run is rejected
/// outright, per the task spec ("tty でなければ --yes フラグ必須").
pub fn resolve(
    yes_flag: bool,
    stdin_is_tty: bool,
    command_name: &str,
    ask: &mut impl FnMut(&str) -> bool,
) -> Result<(), String> {
    if yes_flag {
        return Ok(());
    }
    if !stdin_is_tty {
        return Err(format!(
            "{command_name} is destructive and requires confirmation; \
             pass --yes when running non-interactively"
        ));
    }
    if ask(command_name) {
        Ok(())
    } else {
        Err(format!("aborted: {command_name} was not confirmed"))
    }
}

/// Prints a `y/N` prompt to stderr (stdout stays reserved for command
/// output/`--json`) and reads one line from stdin, accepting `y`/`yes`
/// (case-insensitive) as confirmation. Never called in tests -- exercised
/// indirectly via `tests/integration.rs`'s non-interactive (`--yes`) path,
/// since faking a real tty needs a pty this crate deliberately has no
/// dependency for.
pub fn interactive_prompt(command_name: &str) -> bool {
    eprint!("{command_name} is destructive. Proceed? [y/N]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_flag_always_proceeds() {
        assert_eq!(
            resolve(true, false, "terminate-session", &mut |_| false),
            Ok(())
        );
        assert_eq!(
            resolve(true, true, "terminate-session", &mut |_| false),
            Ok(())
        );
    }

    #[test]
    fn non_tty_without_yes_is_rejected_without_asking() {
        let mut asked = false;
        let result = resolve(false, false, "terminate-session", &mut |_| {
            asked = true;
            true
        });
        assert!(result.is_err());
        assert!(!asked, "non-interactive path must not prompt");
    }

    #[test]
    fn tty_without_yes_asks_and_honors_a_yes_answer() {
        let result = resolve(false, true, "terminate-session", &mut |name| {
            assert_eq!(name, "terminate-session");
            true
        });
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn tty_without_yes_honors_a_no_answer() {
        let result = resolve(false, true, "terminate-session", &mut |_| false);
        assert!(result.is_err());
    }
}
