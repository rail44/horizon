//! Hand-rolled argument parsing for the `horizon` control-plane client
//! (design mandate: no clap/new arg-parsing dependency -- see this crate's
//! `Cargo.toml`).
//!
//! [`parse`] never touches the filesystem, the environment, or a socket --
//! every failure it can produce is a pure function of `argv`, which is what
//! makes it a "usage error" (exit code 2) in [`crate::run`]'s scheme:
//! socket/split-target resolution ([`resolve_socket_path`]/[`resolve_split`])
//! and everything past it can fail too, but only after `argv` was already
//! accepted as well-formed, so those failures are categorized as
//! runtime/server problems (exit 1) instead. See
//! `docs/cli-control-plane-design.md`.

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

/// `--split`'s three parsed states, not yet resolved against the
/// environment (see [`resolve_split`]): omitted entirely (no `SplitFlag` at
/// all, tracked as `Option<SplitFlag>` on the owning [`Subcommand`]
/// variant), given bare (`Here` -- resolve to "this pane's own session" via
/// `HORIZON_SESSION_ID`), or given an explicit session id
/// (`docs/cli-control-plane-design.md`'s "Placement vocabulary" decision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitFlag {
    Here,
    Explicit(String),
}

/// One parsed subcommand, external name and all, per the task's "外部名と
/// 1:1" command list. [`crate::commands`] is the mapping table from this
/// type to the wire request. `NewTerminal`/`NewAgent`/`Attach` carry
/// `activate` (the CLI's `--active`, wired to `docs/cli-control-plane-
/// design.md`'s "activate rides on creating/attaching operations" decision)
/// and (for the first two) `split` (the CLI's `--split`, "Placement
/// vocabulary").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subcommand {
    NewTerminal {
        split: Option<SplitFlag>,
        activate: bool,
    },
    NewAgent {
        prompt: Option<String>,
        split: Option<SplitFlag>,
        activate: bool,
    },
    Attach {
        session_id: String,
        activate: bool,
    },
    TerminateSession {
        session_id: String,
    },
    TerminateAllDetached,
    Approve {
        session_id: String,
        call_id: String,
    },
    Deny {
        session_id: String,
        call_id: String,
    },
    CancelTurn {
        session_id: String,
    },
    ReloadAgentRuntime,
    ReloadConfig,
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

const USAGE: &str = "Usage: horizon [--socket <path>] [--json] [--yes] <subcommand> [args]\n\
Running `horizon` with no subcommand launches the GUI application.\n\
Subcommands:\n  \
  new-terminal [--split [<session-id>]] [--active]\n  \
  new-agent [--prompt <text>] [--split [<session-id>]] [--active]\n  \
  attach <session-id> [--active]\n  \
  terminate-session <session-id>\n  \
  terminate-all-detached\n  \
  approve <session-id> <call-id>\n  \
  deny <session-id> <call-id>\n  \
  cancel-turn <session-id>\n  \
  reload-agent-runtime\n  \
  reload-config\n  \
  sessions\n  \
  state";

/// Parses `argv` (already stripped of `argv[0]`). Global flags
/// (`--socket`/`--json`/`--yes`/`--prompt`/`--split`/`--active`) are
/// recognized anywhere in the argument list, not just before the subcommand
/// name -- simpler than enforcing a strict positional grammar, and
/// unambiguous because every subcommand's own positional arguments
/// (session/call ids) never start with `--`.
///
/// `--split` optionally takes a value: `--split` bare (or immediately
/// followed by another `--flag`, or by nothing) means "here" (resolved from
/// `HORIZON_SESSION_ID` later, see [`resolve_split`]); `--split <value>` (or
/// `--split=<value>`) is an explicit override. This is the one flag in this
/// parser with an optional value -- safe here specifically because no
/// subcommand that accepts `--split` has any positional argument of its own
/// for a bare token after it to collide with.
pub fn parse(args: &[String]) -> Result<ParsedArgs, UsageError> {
    let mut socket = None;
    let mut json = false;
    let mut yes = false;
    let mut prompt: Option<String> = None;
    let mut split: Option<SplitFlag> = None;
    let mut active = false;
    let mut positionals: Vec<String> = Vec::new();

    let mut iter = args.iter().peekable();
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
        } else if arg == "--active" {
            active = true;
        } else if arg == "--prompt" {
            let value = iter
                .next()
                .ok_or_else(|| UsageError("--prompt requires a value".to_string()))?;
            prompt = Some(value.clone());
        } else if let Some(value) = arg.strip_prefix("--prompt=") {
            prompt = Some(value.to_string());
        } else if arg == "--split" {
            split = Some(match iter.peek() {
                Some(next) if !next.starts_with("--") => {
                    let value = (*iter.next().expect("peeked Some")).clone();
                    SplitFlag::Explicit(value)
                }
                _ => SplitFlag::Here,
            });
        } else if let Some(value) = arg.strip_prefix("--split=") {
            split = Some(SplitFlag::Explicit(value.to_string()));
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
            Subcommand::NewTerminal {
                split: split.take(),
                activate: std::mem::take(&mut active),
            }
        }
        "new-agent" => {
            reject_extra(&mut positionals, "new-agent")?;
            Subcommand::NewAgent {
                prompt: prompt.take(),
                split: split.take(),
                activate: std::mem::take(&mut active),
            }
        }
        "attach" => {
            let session_id = next_required(&mut positionals, "attach", "session-id")?;
            reject_extra(&mut positionals, "attach")?;
            Subcommand::Attach {
                session_id,
                activate: std::mem::take(&mut active),
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
        "reload-config" => {
            reject_extra(&mut positionals, "reload-config")?;
            Subcommand::ReloadConfig
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
    if split.is_some() {
        return Err(UsageError(
            "--split is only valid with new-terminal/new-agent".to_string(),
        ));
    }
    if active {
        return Err(UsageError(
            "--active is only valid with new-terminal/new-agent/attach".to_string(),
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

/// Resolves the control socket path: `--socket` wins, then `HORIZON_SOCKET`,
/// then this build's fixed default -- `docs/cli-control-plane-design.md`'s
/// Second revision ("Fixed well-known socket path"): a client run from
/// anywhere (not just inside a pane) now finds a single-instance Horizon
/// with no environment set up at all, unlike v1's env-var-mandatory scheme.
/// `env_socket` is passed in rather than read here so the function stays a
/// pure, directly testable mapping -- [`crate::run`]'s caller does the one
/// real `std::env::var` read.
pub fn resolve_socket_path(cli_socket: Option<PathBuf>, env_socket: Option<String>) -> PathBuf {
    if let Some(path) = cli_socket {
        return path;
    }
    if let Some(value) = env_socket.filter(|v| !v.is_empty()) {
        return PathBuf::from(value);
    }
    default_socket_path()
}

/// This build's fixed default control socket path when neither `--socket`
/// nor `HORIZON_SOCKET` is given. Identical formula to (and deliberately
/// duplicated from, not shared with --
/// `control_plane::socket::default_socket_path`'s doc comment explains why)
/// the server side's own default -- both sides independently arrive at the
/// same path, the same shape `horizon_agent::socket::default_socket_path`
/// already uses between `horizon-agentd` and Horizon's own agent client.
fn default_socket_path() -> PathBuf {
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    default_socket_path_from(xdg_runtime_dir, uid)
}

fn default_socket_path_from(xdg_runtime_dir: Option<String>, uid: u32) -> PathBuf {
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("horizon").join("control.sock"),
        None => PathBuf::from(format!("/tmp/horizon-control-{uid}.sock")),
    }
}

/// Resolves `subcommand`'s `--split` flag (if it has one) against the
/// environment -- `docs/cli-control-plane-design.md`'s "Placement
/// vocabulary" decision. Subcommands with no `split` field at all (every
/// variant but `NewTerminal`/`NewAgent`) always resolve to `Ok(None)`.
/// `env_session_id` is passed in rather than read here for the same
/// testability reason [`resolve_socket_path`] takes `env_socket`.
pub fn resolved_split_for(
    subcommand: &Subcommand,
    env_session_id: Option<String>,
) -> Result<Option<String>, String> {
    match subcommand {
        Subcommand::NewTerminal { split, .. } | Subcommand::NewAgent { split, .. } => {
            resolve_split(split.clone(), env_session_id)
        }
        _ => Ok(None),
    }
}

/// Resolves `--split`'s three parsed states against the environment:
/// omitted -> no split target at all; explicit -> that value verbatim; bare
/// (`SplitFlag::Here`) -> `HORIZON_SESSION_ID`, the pane's own session id --
/// injected into every pane's environment alongside `HORIZON_SOCKET`
/// (`docs/cli-control-plane-design.md`'s "Placement vocabulary" decision).
/// Missing both an explicit value and the env var when `Here` was requested
/// is the one error case, with the "run inside a pane or say --split
/// explicitly" guidance the task spec calls for.
pub fn resolve_split(
    split: Option<SplitFlag>,
    env_session_id: Option<String>,
) -> Result<Option<String>, String> {
    match split {
        None => Ok(None),
        Some(SplitFlag::Explicit(session_id)) => Ok(Some(session_id)),
        Some(SplitFlag::Here) => env_session_id
            .filter(|value| !value.is_empty())
            .map(Some)
            .ok_or_else(|| {
                "no session id found for --split -- run this inside a Horizon pane (which sets \
             HORIZON_SESSION_ID) or pass --split <session-id> explicitly"
                    .to_string()
            }),
    }
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
                prompt: Some("fix the bug".to_string()),
                split: None,
                activate: false,
            }
        );
    }

    #[test]
    fn parses_new_agent_with_prompt_equals_form() {
        let parsed = parse(&args(&["new-agent", "--prompt=fix the bug"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewAgent {
                prompt: Some("fix the bug".to_string()),
                split: None,
                activate: false,
            }
        );
    }

    #[test]
    fn parses_new_agent_without_prompt() {
        let parsed = parse(&args(&["new-agent"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewAgent {
                prompt: None,
                split: None,
                activate: false,
            }
        );
    }

    #[test]
    fn parses_new_terminal_with_active() {
        let parsed = parse(&args(&["new-terminal", "--active"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewTerminal {
                split: None,
                activate: true,
            }
        );
    }

    #[test]
    fn parses_bare_split_as_here() {
        let parsed = parse(&args(&["new-terminal", "--split"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewTerminal {
                split: Some(SplitFlag::Here),
                activate: false,
            }
        );
    }

    #[test]
    fn parses_bare_split_as_here_when_followed_by_another_flag() {
        let parsed = parse(&args(&["new-terminal", "--split", "--active"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewTerminal {
                split: Some(SplitFlag::Here),
                activate: true,
            }
        );
    }

    #[test]
    fn parses_split_with_an_explicit_value() {
        let parsed = parse(&args(&["new-terminal", "--split", "s-1"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewTerminal {
                split: Some(SplitFlag::Explicit("s-1".to_string())),
                activate: false,
            }
        );
    }

    #[test]
    fn parses_the_split_equals_form() {
        let parsed = parse(&args(&["new-agent", "--split=s-1"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::NewAgent {
                prompt: None,
                split: Some(SplitFlag::Explicit("s-1".to_string())),
                activate: false,
            }
        );
    }

    #[test]
    fn parses_attach_with_active() {
        let parsed = parse(&args(&["attach", "s-1", "--active"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::Attach {
                session_id: "s-1".to_string(),
                activate: true,
            }
        );
    }

    #[test]
    fn parses_attach_without_active() {
        let parsed = parse(&args(&["attach", "s-1"])).unwrap();
        assert_eq!(
            parsed.subcommand,
            Subcommand::Attach {
                session_id: "s-1".to_string(),
                activate: false,
            }
        );
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
        assert_eq!(
            parse(&args(&["reload-config"])).unwrap().subcommand,
            Subcommand::ReloadConfig
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
        assert!(parse(&args(&["attach"])).is_err());
    }

    #[test]
    fn extra_positional_is_a_usage_error() {
        assert!(parse(&args(&["sessions", "extra"])).is_err());
        assert!(parse(&args(&["terminate-session", "s-1", "extra"])).is_err());
        assert!(parse(&args(&["attach", "s-1", "extra"])).is_err());
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
    fn split_on_an_unsupported_subcommand_is_a_usage_error() {
        assert!(parse(&args(&["sessions", "--split", "s-1"])).is_err());
        assert!(parse(&args(&["attach", "s-1", "--split", "s-2"])).is_err());
    }

    #[test]
    fn active_on_an_unsupported_subcommand_is_a_usage_error() {
        assert!(parse(&args(&["sessions", "--active"])).is_err());
        assert!(parse(&args(&["terminate-session", "s-1", "--active"])).is_err());
    }

    #[test]
    fn resolve_socket_path_prefers_the_flag() {
        let resolved = resolve_socket_path(
            Some(PathBuf::from("/from/flag")),
            Some("/from/env".to_string()),
        );
        assert_eq!(resolved, PathBuf::from("/from/flag"));
    }

    #[test]
    fn resolve_socket_path_falls_back_to_env() {
        let resolved = resolve_socket_path(None, Some("/from/env".to_string()));
        assert_eq!(resolved, PathBuf::from("/from/env"));
    }

    #[test]
    fn resolve_socket_path_falls_back_to_the_fixed_default_with_neither() {
        assert_eq!(resolve_socket_path(None, None), default_socket_path());
        assert_eq!(
            resolve_socket_path(None, Some(String::new())),
            default_socket_path()
        );
    }

    #[test]
    fn default_socket_path_prefers_xdg_runtime_dir_when_set() {
        assert_eq!(
            default_socket_path_from(Some("/run/user/1000".to_string()), 1000),
            PathBuf::from("/run/user/1000/horizon/control.sock")
        );
    }

    #[test]
    fn default_socket_path_falls_back_to_tmp_with_uid_when_xdg_runtime_dir_is_unset_or_empty() {
        assert_eq!(
            default_socket_path_from(None, 1000),
            PathBuf::from("/tmp/horizon-control-1000.sock")
        );
        assert_eq!(
            default_socket_path_from(Some(String::new()), 1000),
            PathBuf::from("/tmp/horizon-control-1000.sock")
        );
    }

    #[test]
    fn resolved_split_for_is_none_for_subcommands_without_a_split_field() {
        assert_eq!(
            resolved_split_for(&Subcommand::Sessions, Some("s-1".to_string())),
            Ok(None)
        );
        assert_eq!(
            resolved_split_for(
                &Subcommand::Attach {
                    session_id: "s-1".to_string(),
                    activate: false,
                },
                Some("s-2".to_string()),
            ),
            Ok(None)
        );
    }

    #[test]
    fn resolved_split_for_resolves_new_terminal_and_new_agent() {
        let new_terminal = Subcommand::NewTerminal {
            split: Some(SplitFlag::Explicit("s-1".to_string())),
            activate: false,
        };
        assert_eq!(
            resolved_split_for(&new_terminal, None),
            Ok(Some("s-1".to_string()))
        );

        let new_agent = Subcommand::NewAgent {
            prompt: None,
            split: Some(SplitFlag::Here),
            activate: false,
        };
        assert_eq!(
            resolved_split_for(&new_agent, Some("s-2".to_string())),
            Ok(Some("s-2".to_string()))
        );
    }

    #[test]
    fn resolve_split_is_none_when_omitted() {
        assert_eq!(resolve_split(None, Some("s-1".to_string())), Ok(None));
    }

    #[test]
    fn resolve_split_uses_the_explicit_value_regardless_of_the_environment() {
        assert_eq!(
            resolve_split(Some(SplitFlag::Explicit("s-1".to_string())), None),
            Ok(Some("s-1".to_string()))
        );
    }

    #[test]
    fn resolve_split_here_uses_the_environment_session_id() {
        assert_eq!(
            resolve_split(Some(SplitFlag::Here), Some("s-1".to_string())),
            Ok(Some("s-1".to_string()))
        );
    }

    #[test]
    fn resolve_split_here_errors_with_no_environment_session_id() {
        assert!(resolve_split(Some(SplitFlag::Here), None).is_err());
        assert!(resolve_split(Some(SplitFlag::Here), Some(String::new())).is_err());
    }
}
