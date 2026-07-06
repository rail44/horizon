//! Hand-rolled argument parsing for `horizon-ctl` (design mandate: no
//! clap/new arg-parsing dependency -- see this crate's `Cargo.toml`).
//!
//! [`parse`] never touches the filesystem, the environment, or a socket --
//! every failure it can produce is a pure function of `argv`, which is what
//! makes it a "usage error" (exit code 2) in [`crate::run`]'s scheme:
//! socket resolution ([`resolve_socket_path`]) and everything past it can
//! fail too, but only after `argv` was already accepted as well-formed, so
//! those failures are categorized as runtime/server problems (exit 1)
//! instead. See `docs/cli-control-plane-design.md`.

use std::fmt;
use std::path::PathBuf;

/// Flags meaningful across every subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalOptions {
    pub socket: Option<PathBuf>,
    pub json: bool,
    /// Explicit acknowledgment for a destructive subcommand run
    /// non-interactively -- see `crate::confirm`.
    pub yes: bool,
}

/// One parsed subcommand, external name and all, per the task's "外部名と
/// 1:1" command list. [`crate::commands`] is the mapping table from this
/// type to the wire request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subcommand {
    NewTerminal,
    NewAgent { prompt: Option<String> },
    TerminateSession { session_id: String },
    TerminateAllDetached,
    Approve { session_id: String, call_id: String },
    Deny { session_id: String, call_id: String },
    CancelTurn { session_id: String },
    ReloadAgentRuntime,
    Sessions,
    State,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedArgs {
    pub global: GlobalOptions,
    pub subcommand: Subcommand,
}

/// A pure `argv` problem: unknown subcommand, wrong arity, unrecognized
/// flag, or a flag used with the wrong subcommand. Always exit code 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n\n{USAGE}", self.0)
    }
}

const USAGE: &str = "Usage: horizon-ctl [--socket <path>] [--json] [--yes] <subcommand> [args]\n\
Subcommands:\n  \
  new-terminal\n  \
  new-agent [--prompt <text>]\n  \
  terminate-session <session-id>\n  \
  terminate-all-detached\n  \
  approve <session-id> <call-id>\n  \
  deny <session-id> <call-id>\n  \
  cancel-turn <session-id>\n  \
  reload-agent-runtime\n  \
  sessions\n  \
  state";

/// Parses `argv` (already stripped of `argv[0]`). Global flags
/// (`--socket`/`--json`/`--yes`/`--prompt`) are recognized anywhere in the
/// argument list, not just before the subcommand name -- simpler than
/// enforcing a strict positional grammar, and unambiguous because every
/// subcommand's own positional arguments (session/call ids) never start
/// with `--`.
pub fn parse(args: &[String]) -> Result<ParsedArgs, UsageError> {
    let mut socket = None;
    let mut json = false;
    let mut yes = false;
    let mut prompt: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--socket" {
            let value = iter
                .next()
                .ok_or_else(|| UsageError("--socket requires a value".to_string()))?;
            socket = Some(PathBuf::from(value));
        } else if let Some(value) = arg.strip_prefix("--socket=") {
            socket = Some(PathBuf::from(value));
        } else if arg == "--json" {
            json = true;
        } else if arg == "--yes" {
            yes = true;
        } else if arg == "--prompt" {
            let value = iter
                .next()
                .ok_or_else(|| UsageError("--prompt requires a value".to_string()))?;
            prompt = Some(value.clone());
        } else if let Some(value) = arg.strip_prefix("--prompt=") {
            prompt = Some(value.to_string());
        } else if arg.starts_with("--") {
            return Err(UsageError(format!("unrecognized flag: {arg}")));
        } else {
            positionals.push(arg.clone());
        }
    }

    let mut positionals = positionals.into_iter();
    let name = positionals
        .next()
        .ok_or_else(|| UsageError("missing subcommand".to_string()))?;

    let subcommand = match name.as_str() {
        "new-terminal" => {
            reject_extra(&mut positionals, "new-terminal")?;
            Subcommand::NewTerminal
        }
        "new-agent" => {
            reject_extra(&mut positionals, "new-agent")?;
            Subcommand::NewAgent {
                prompt: prompt.take(),
            }
        }
        "terminate-session" => {
            let session_id = next_required(&mut positionals, "terminate-session", "session-id")?;
            reject_extra(&mut positionals, "terminate-session")?;
            Subcommand::TerminateSession { session_id }
        }
        "terminate-all-detached" => {
            reject_extra(&mut positionals, "terminate-all-detached")?;
            Subcommand::TerminateAllDetached
        }
        "approve" => {
            let session_id = next_required(&mut positionals, "approve", "session-id")?;
            let call_id = next_required(&mut positionals, "approve", "call-id")?;
            reject_extra(&mut positionals, "approve")?;
            Subcommand::Approve {
                session_id,
                call_id,
            }
        }
        "deny" => {
            let session_id = next_required(&mut positionals, "deny", "session-id")?;
            let call_id = next_required(&mut positionals, "deny", "call-id")?;
            reject_extra(&mut positionals, "deny")?;
            Subcommand::Deny {
                session_id,
                call_id,
            }
        }
        "cancel-turn" => {
            let session_id = next_required(&mut positionals, "cancel-turn", "session-id")?;
            reject_extra(&mut positionals, "cancel-turn")?;
            Subcommand::CancelTurn { session_id }
        }
        "reload-agent-runtime" => {
            reject_extra(&mut positionals, "reload-agent-runtime")?;
            Subcommand::ReloadAgentRuntime
        }
        "sessions" => {
            reject_extra(&mut positionals, "sessions")?;
            Subcommand::Sessions
        }
        "state" => {
            reject_extra(&mut positionals, "state")?;
            Subcommand::State
        }
        other => return Err(UsageError(format!("unknown subcommand: {other}"))),
    };

    if prompt.is_some() {
        return Err(UsageError(
            "--prompt is only valid with new-agent".to_string(),
        ));
    }

    Ok(ParsedArgs {
        global: GlobalOptions { socket, json, yes },
        subcommand,
    })
}

fn next_required(
    positionals: &mut impl Iterator<Item = String>,
    subcommand: &str,
    what: &str,
) -> Result<String, UsageError> {
    positionals
        .next()
        .ok_or_else(|| UsageError(format!("{subcommand} requires a {what}")))
}

fn reject_extra(
    positionals: &mut impl Iterator<Item = String>,
    subcommand: &str,
) -> Result<(), UsageError> {
    match positionals.next() {
        Some(extra) => Err(UsageError(format!(
            "{subcommand} does not take extra argument: {extra}"
        ))),
        None => Ok(()),
    }
}

/// Resolves the control socket path: `--socket` wins, then `HORIZON_SOCKET`
/// (per the design doc's "Discovery" decision), then an explanatory error.
/// `env_socket` is passed in rather than read here so the function stays a
/// pure, directly testable mapping -- [`crate::run`]'s caller does the one
/// real `std::env::var` read.
pub fn resolve_socket_path(
    cli_socket: Option<PathBuf>,
    env_socket: Option<String>,
) -> Result<PathBuf, String> {
    if let Some(path) = cli_socket {
        return Ok(path);
    }
    if let Some(value) = env_socket.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    Err(
        "no control socket found -- run horizon-ctl inside a Horizon pane (which sets \
         HORIZON_SOCKET) or pass --socket <path> explicitly"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn parses_a_bare_subcommand() {
        let parsed = parse(&args(&["sessions"])).unwrap();
        assert_eq!(parsed.subcommand, Subcommand::Sessions);
        assert_eq!(
            parsed.global,
            GlobalOptions {
                socket: None,
                json: false,
                yes: false
            }
        );
    }

    #[test]
    fn parses_global_flags_before_the_subcommand() {
        let parsed = parse(&args(&["--socket", "/tmp/s.sock", "--json", "state"])).unwrap();
        assert_eq!(parsed.subcommand, Subcommand::State);
        assert_eq!(parsed.global.socket, Some(PathBuf::from("/tmp/s.sock")));
        assert!(parsed.global.json);
    }

    #[test]
    fn parses_the_socket_equals_form() {
        let parsed = parse(&args(&["--socket=/tmp/s.sock", "sessions"])).unwrap();
        assert_eq!(parsed.global.socket, Some(PathBuf::from("/tmp/s.sock")));
    }

    #[test]
    fn parses_yes_flag_after_the_subcommand() {
        let parsed = parse(&args(&["terminate-all-detached", "--yes"])).unwrap();
        assert_eq!(parsed.subcommand, Subcommand::TerminateAllDetached);
        assert!(parsed.global.yes);
    }

    #[test]
    fn parses_new_agent_with_prompt() {
        let parsed = parse(&args(&["new-agent", "--prompt", "fix the bug"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewAgent {
                prompt: Some("fix the bug".to_string())
            }
        );
    }

    #[test]
    fn parses_new_agent_with_prompt_equals_form() {
        let parsed = parse(&args(&["new-agent", "--prompt=fix the bug"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewAgent {
                prompt: Some("fix the bug".to_string())
            }
        );
    }

    #[test]
    fn parses_new_agent_without_prompt() {
        let parsed = parse(&args(&["new-agent"])).unwrap();
        assert_eq!(parsed.subcommand, Subcommand::NewAgent { prompt: None });
    }

    #[test]
    fn parses_terminate_session() {
        let parsed = parse(&args(&["terminate-session", "s-1"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::TerminateSession {
                session_id: "s-1".to_string()
            }
        );
    }

    #[test]
    fn parses_approve_and_deny() {
        assert_eq!(
            parse(&args(&["approve", "s-1", "c-1"])).unwrap().subcommand,
            Subcommand::Approve {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string()
            }
        );
        assert_eq!(
            parse(&args(&["deny", "s-1", "c-1"])).unwrap().subcommand,
            Subcommand::Deny {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string()
            }
        );
    }

    #[test]
    fn parses_cancel_turn_and_reload() {
        assert_eq!(
            parse(&args(&["cancel-turn", "s-1"])).unwrap().subcommand,
            Subcommand::CancelTurn {
                session_id: "s-1".to_string()
            }
        );
        assert_eq!(
            parse(&args(&["reload-agent-runtime"])).unwrap().subcommand,
            Subcommand::ReloadAgentRuntime
        );
    }

    #[test]
    fn missing_subcommand_is_a_usage_error() {
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["--json"])).is_err());
    }

    #[test]
    fn unknown_subcommand_is_a_usage_error() {
        assert!(parse(&args(&["frobnicate"])).is_err());
    }

    #[test]
    fn unrecognized_flag_is_a_usage_error() {
        assert!(parse(&args(&["sessions", "--bogus"])).is_err());
    }

    #[test]
    fn missing_required_positional_is_a_usage_error() {
        assert!(parse(&args(&["terminate-session"])).is_err());
        assert!(parse(&args(&["approve", "s-1"])).is_err());
    }

    #[test]
    fn extra_positional_is_a_usage_error() {
        assert!(parse(&args(&["sessions", "extra"])).is_err());
        assert!(parse(&args(&["terminate-session", "s-1", "extra"])).is_err());
    }

    #[test]
    fn dangling_flag_value_is_a_usage_error() {
        assert!(parse(&args(&["--socket"])).is_err());
        assert!(parse(&args(&["new-agent", "--prompt"])).is_err());
    }

    #[test]
    fn prompt_on_a_non_new_agent_subcommand_is_a_usage_error() {
        assert!(parse(&args(&["sessions", "--prompt", "x"])).is_err());
    }

    #[test]
    fn resolve_socket_path_prefers_the_flag() {
        let resolved = resolve_socket_path(
            Some(PathBuf::from("/from/flag")),
            Some("/from/env".to_string()),
        );
        assert_eq!(resolved, Ok(PathBuf::from("/from/flag")));
    }

    #[test]
    fn resolve_socket_path_falls_back_to_env() {
        let resolved = resolve_socket_path(None, Some("/from/env".to_string()));
        assert_eq!(resolved, Ok(PathBuf::from("/from/env")));
    }

    #[test]
    fn resolve_socket_path_errors_with_neither() {
        assert!(resolve_socket_path(None, None).is_err());
        assert!(resolve_socket_path(None, Some(String::new())).is_err());
    }
}
