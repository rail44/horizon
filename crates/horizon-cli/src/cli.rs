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

mod options;

use std::collections::BTreeMap;
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

/// One parsed subcommand, external name and all. [`crate::commands`] is
/// the mapping table from this type to the wire request. `New` and
/// `Attach` carry `activate` (the CLI's `--active`, wired to
/// `docs/cli-control-plane-design.md`'s "activate rides on
/// creating/attaching operations" decision), and `New` carries `split`
/// (the CLI's `--split`, "Placement vocabulary").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subcommand {
    /// A new pane of a plugin: a terminal, an agent, a view, an extension.
    /// `arguments`, and those `named` (`--prompt`, `--role`),
    /// are what the plugin's pane is made from, as the plugin says; the
    /// shell, which knows the plugin, reads them. A file among them is a
    /// path from `directory`, the caller's working directory, which
    /// [`crate::run`] fills in; `parse` leaves it empty.
    New {
        plugin: String,
        arguments: Vec<String>,
        named: BTreeMap<String, String>,
        directory: PathBuf,
        split: Option<SplitFlag>,
        activate: bool,
    },
    /// A command of the plugin the cursor's pane shows, by the name the
    /// palette lists it under: any subcommand that is not one of this
    /// program's own.
    PluginCommand {
        name: String,
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
        occurrence_id: String,
    },
    Deny {
        session_id: String,
        call_id: String,
        occurrence_id: String,
        /// Optional human-supplied deny reason, forwarded to the wire
        /// `Command::DenyToolCall.reason` and recorded on the resulting
        /// `Event::ApprovalResolved`. `None` (the `--reason` flag omitted) is
        /// the no-reason case.
        reason: Option<String>,
    },
    CancelTurn {
        session_id: String,
    },
    /// Delivers a user message to an already-running agent session -- the
    /// same path as pasting into the composer (`AgentSession::send_user_message`).
    /// `text` is `None` when the caller omitted it on the command line, in
    /// which case [`crate::run`] reads stdin to EOF as the message body
    /// (the multi-line-brief-pasting use case); after that resolution `text`
    /// is always `Some` (and non-empty) before it reaches the wire.
    Send {
        session_id: String,
        text: Option<String>,
    },
    /// Switches an already-running agent session's model -- the CLI
    /// counterpart of the model picker's `set_session_model` call.
    /// `provider`/`model` are the same pair the picker sends: a
    /// `[[providers]]` entry name plus a model id. Non-destructive, so no
    /// `--yes`; the daemon's own validation (unknown provider, missing model
    /// id, unknown session) is what the caller sees on failure.
    SetModel {
        session_id: String,
        provider: String,
        model: String,
    },
    /// Switch an agent session's reasoning effort: the CLI counterpart of
    /// the model picker's effort step. `level` is a level's word (`none`,
    /// `minimal`, `low`, `medium`, `high`, `xhigh`, `max`); the daemon
    /// refuses one the session's model does not take.
    SetEffort {
        session_id: String,
        level: String,
    },
    ReloadAgentRuntime,
    /// `reload-agent-runtime`'s terminal-daemon counterpart
    /// (`docs/terminald-split-design.md` decision 3). Destructive: it
    /// restarts the process that owns every PTY, so every terminal session
    /// ends with it.
    ReloadTerminalRuntime,
    ReloadConfig,
    /// Opens a new terminal tab whose cwd is the active session's directory
    /// (`docs/session-relationship-design.md` decision 4a) -- a bare invoke
    /// like `ReloadAgentRuntime`/`ReloadConfig`, since v1 always targets
    /// "whichever session is active", never a client-supplied id.
    OpenTerminalInSessionDirectory,
    Sessions,
    State,
    /// `extension use <file>`: the extension whose component is `file`
    /// in place of the one of its name, until it is reset or the app
    /// restarts. The file is a path from where the command is run.
    ExtensionUse {
        file: String,
    },
    /// `extension reset [<name>]`: lets go of what `extension use` put in,
    /// the one named or all.
    ExtensionReset {
        name: Option<String>,
    },
    /// `extension list`: the extensions loaded, and where each is from.
    ExtensionList,
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
  new <plugin> [<argument>...] [--split [<session-id>]] [--active]\n  \
      a new pane of a plugin, made from what the plugin takes:\n  \
      new terminal\n  \
      new agent [--prompt <text>] [--role <id>]\n  \
      new markdown <path>\n  \
      new wiki [<page>]\n  \
      new preview <extension.wasm> [<argument>...]\n  \
      new board-list | board-thread <task> | theme-settings | sessions\n  \
      new <extension's name> [<argument>...]\n  \
  attach <session-id> [--active]\n  \
  terminate-session <session-id>\n  \
  terminate-all-detached\n  \
  approve <session-id> <call-id> <occurrence-id>\n  \
  deny <session-id> <call-id> <occurrence-id> [--reason <text>]\n  \
  cancel-turn <session-id>\n  \
  send <session-id> [text]\n  \
  set-model <session-id> <provider> <model>\n  \
  set-effort <session-id> <level>\n  \
  reload-agent-runtime\n  \
  reload-terminal-runtime\n  \
  reload-config\n  \
  open-terminal-in-session-directory\n  \
  sessions\n  \
  state\n  \
  extension use <extension.wasm>\n  \
      takes an extension from its file in place of the one of its name,\n  \
      until it is reset or Horizon restarts: one still being written, tried\n  \
      where the one in use is\n  \
  extension reset [<name>]\n  \
      lets go of what `extension use` put in: the one named, or all\n  \
  extension list\n  \
      the extensions loaded, and where each is from\n  \
  <command>    a command of the plugin the cursor's pane shows";

/// Parses `argv` (already stripped of `argv[0]`). Global flags
/// (`--socket`/`--json`/`--yes`/`--prompt`/`--role`/`--split`/`--active`)
/// are recognized anywhere in the argument list, not just before the
/// subcommand name -- simpler than enforcing a strict positional grammar, and
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
    let options::Arguments {
        global,
        mut options,
        positionals,
    } = options::scan(args)?;
    let mut positionals = positionals.into_iter();
    let name = positionals
        .next()
        .ok_or_else(|| UsageError("missing subcommand".to_string()))?;

    let subcommand = match name.as_str() {
        "new" => {
            let plugin = next_required(&mut positionals, "new", "plugin")?;
            let named = [
                ("prompt", options.prompt.take()),
                ("role", options.role.take()),
            ];
            Subcommand::New {
                plugin,
                arguments: positionals.by_ref().collect(),
                named: (named.into_iter())
                    .filter_map(|(name, value)| Some((name.to_string(), value?)))
                    .collect(),
                directory: PathBuf::new(),
                split: options.split.take(),
                activate: std::mem::take(&mut options.active),
            }
        }
        "attach" => {
            let session_id = next_required(&mut positionals, "attach", "session-id")?;
            reject_extra(&mut positionals, "attach")?;
            Subcommand::Attach {
                session_id,
                activate: std::mem::take(&mut options.active),
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
            let occurrence_id = next_required(&mut positionals, "approve", "occurrence-id")?;
            reject_extra(&mut positionals, "approve")?;
            Subcommand::Approve {
                session_id,
                call_id,
                occurrence_id,
            }
        }
        "deny" => {
            let session_id = next_required(&mut positionals, "deny", "session-id")?;
            let call_id = next_required(&mut positionals, "deny", "call-id")?;
            let occurrence_id = next_required(&mut positionals, "deny", "occurrence-id")?;
            reject_extra(&mut positionals, "deny")?;
            Subcommand::Deny {
                session_id,
                call_id,
                occurrence_id,
                reason: options.reason.take(),
            }
        }
        "cancel-turn" => {
            let session_id = next_required(&mut positionals, "cancel-turn", "session-id")?;
            reject_extra(&mut positionals, "cancel-turn")?;
            Subcommand::CancelTurn { session_id }
        }
        "send" => {
            let session_id = next_required(&mut positionals, "send", "session-id")?;
            let text = positionals.next();
            reject_extra(&mut positionals, "send")?;
            Subcommand::Send { session_id, text }
        }
        "set-model" => {
            let session_id = next_required(&mut positionals, "set-model", "session-id")?;
            let provider = next_required(&mut positionals, "set-model", "provider")?;
            let model = next_required(&mut positionals, "set-model", "model")?;
            reject_extra(&mut positionals, "set-model")?;
            Subcommand::SetModel {
                session_id,
                provider,
                model,
            }
        }
        "set-effort" => {
            let session_id = next_required(&mut positionals, "set-effort", "session-id")?;
            let level = next_required(&mut positionals, "set-effort", "level")?;
            reject_extra(&mut positionals, "set-effort")?;
            Subcommand::SetEffort { session_id, level }
        }
        "reload-agent-runtime" => {
            reject_extra(&mut positionals, "reload-agent-runtime")?;
            Subcommand::ReloadAgentRuntime
        }
        "reload-terminal-runtime" => {
            reject_extra(&mut positionals, "reload-terminal-runtime")?;
            Subcommand::ReloadTerminalRuntime
        }
        "reload-config" => {
            reject_extra(&mut positionals, "reload-config")?;
            Subcommand::ReloadConfig
        }
        "open-terminal-in-session-directory" => {
            reject_extra(&mut positionals, "open-terminal-in-session-directory")?;
            Subcommand::OpenTerminalInSessionDirectory
        }
        "sessions" => {
            reject_extra(&mut positionals, "sessions")?;
            Subcommand::Sessions
        }
        "state" => {
            reject_extra(&mut positionals, "state")?;
            Subcommand::State
        }
        "extension" => {
            let verb = next_required(&mut positionals, "extension", "use | reset | list")?;
            match verb.as_str() {
                "use" => {
                    let file = next_required(&mut positionals, "extension use", "extension.wasm")?;
                    reject_extra(&mut positionals, "extension use")?;
                    Subcommand::ExtensionUse { file }
                }
                "reset" => {
                    let name = positionals.next();
                    reject_extra(&mut positionals, "extension reset")?;
                    Subcommand::ExtensionReset { name }
                }
                "list" => {
                    reject_extra(&mut positionals, "extension list")?;
                    Subcommand::ExtensionList
                }
                other => {
                    return Err(UsageError(format!(
                        "unknown extension command: {other} (there are: use, reset, list)"
                    )));
                }
            }
        }
        // Not one of this program's own: a plugin's command, which the
        // running Horizon knows.
        other if !other.starts_with('-') => {
            reject_extra(&mut positionals, other)?;
            Subcommand::PluginCommand {
                name: other.to_string(),
            }
        }
        other => return Err(UsageError(format!("unknown subcommand: {other}"))),
    };

    options.validate_unused()?;
    Ok(ParsedArgs { global, subcommand })
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
/// real `std::env::var` read. The fixed default itself
/// (`horizon_control::host::socket::default_socket_path`) is shared with the
/// server side rather than duplicated here: this crate already depends on
/// `horizon-control` directly, so both sides calling the one function is
/// simpler than keeping two copies of the same formula in sync.
pub fn resolve_socket_path(cli_socket: Option<PathBuf>, env_socket: Option<String>) -> PathBuf {
    if let Some(path) = cli_socket {
        return path;
    }
    if let Some(value) = env_socket.filter(|v| !v.is_empty()) {
        return PathBuf::from(value);
    }
    horizon_control::host::socket::default_socket_path()
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
        Subcommand::New { split, .. } => resolve_split(split.clone(), env_session_id),
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

    /// `new <plugin>` with `arguments` and those `named`, as parsed.
    fn new(
        plugin: &str,
        arguments: &[&str],
        named: &[(&str, &str)],
        split: Option<SplitFlag>,
        activate: bool,
    ) -> Subcommand {
        Subcommand::New {
            plugin: plugin.to_string(),
            arguments: arguments.iter().map(|text| text.to_string()).collect(),
            named: (named.iter())
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            directory: PathBuf::new(),
            split,
            activate,
        }
    }

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
    fn option_values_preserve_equals_flag_like_text_and_last_value() {
        let parsed = parse(&args(&[
            "--socket=first",
            "new",
            "agent",
            "--prompt",
            "old",
            "--prompt=a=b",
            "--socket",
            "last",
        ]))
        .unwrap();
        assert_eq!(parsed.global.socket, Some(PathBuf::from("last")));
        assert_eq!(
            parsed.subcommand,
            new("agent", &[], &[("prompt", "a=b")], None, false)
        );
        let parsed = parse(&args(&["new", "agent", "--prompt", "--json"])).unwrap();
        assert!(!parsed.global.json);
        assert_eq!(
            parsed.subcommand,
            new("agent", &[], &[("prompt", "--json")], None, false)
        );
        assert_eq!(
            parse(&args(&["state", "--json=true"])).unwrap_err().0,
            "unrecognized flag: --json=true"
        );
    }

    #[test]
    fn command_arity_errors_precede_unused_option_errors() {
        assert_eq!(
            parse(&args(&["send", "--prompt=x", "--active"]))
                .unwrap_err()
                .0,
            "send requires a session-id"
        );
        assert_eq!(
            parse(&args(&["state", "--prompt=x", "--active"]))
                .unwrap_err()
                .0,
            "--prompt is only valid with new"
        );
        assert_eq!(
            parse(&args(&["state", "--role=custom"])).unwrap_err().0,
            "--role is only valid with new"
        );
    }

    #[test]
    fn unused_reason_keeps_its_existing_acceptance() {
        let parsed = parse(&args(&["state", "--reason=note"])).unwrap();
        assert_eq!(parsed.subcommand, Subcommand::State);
    }

    #[test]
    fn new_takes_a_plugin_and_what_its_pane_is_made_from_as_typed() {
        // What follows the plugin is the plugin's, in order: the shell,
        // which knows the plugin, reads it.
        for (typed, plugin, arguments) in [
            (&["new", "terminal"][..], "terminal", &[][..]),
            (
                &["new", "markdown", "notes/index.md"],
                "markdown",
                &["notes/index.md"],
            ),
            (&["new", "wiki"], "wiki", &[]),
            (&["new", "wiki", "sandbox"], "wiki", &["sandbox"]),
            (
                &["new", "preview", "target/x.wasm", "README.md"],
                "preview",
                &["target/x.wasm", "README.md"],
            ),
            (&["new", "board-thread", "76"], "board-thread", &["76"]),
        ] {
            assert_eq!(
                parse(&args(typed)).unwrap().subcommand,
                new(plugin, arguments, &[], None, false),
                "{typed:?}"
            );
        }
        assert_eq!(
            parse(&args(&["new"])).unwrap_err().0,
            "new requires a plugin"
        );
    }

    #[test]
    fn new_takes_what_is_given_by_name() {
        assert_eq!(
            parse(&args(&[
                "new", "agent", "--role", "config", "--split", "abc", "--active",
            ]))
            .unwrap()
            .subcommand,
            new(
                "agent",
                &[],
                &[("role", "config")],
                Some(SplitFlag::Explicit("abc".to_string())),
                true,
            )
        );
        assert_eq!(
            parse(&args(&[
                "new",
                "agent",
                "--role",
                "config",
                "--prompt",
                "make it dark",
                "--split",
                "s-1",
                "--active",
            ]))
            .unwrap()
            .subcommand,
            new(
                "agent",
                &[],
                &[("prompt", "make it dark"), ("role", "config")],
                Some(SplitFlag::Explicit("s-1".to_string())),
                true,
            )
        );
        assert_eq!(
            parse(&args(&["new", "agent", "--role=keeper"]))
                .unwrap()
                .subcommand,
            new("agent", &[], &[("role", "keeper")], None, false)
        );
    }

    #[test]
    fn what_is_given_by_name_is_only_for_new() {
        for flag in ["--prompt", "--role"] {
            let said = parse(&args(&["sessions", flag, "x"])).unwrap_err().0;
            assert_eq!(said, format!("{flag} is only valid with new"));
        }
    }

    #[test]
    fn parses_the_extension_commands() {
        let parsed = |words: &[&str]| parse(&args(words)).map(|parsed| parsed.subcommand);
        assert_eq!(
            parsed(&["extension", "use", "target/a.wasm"]),
            Ok(Subcommand::ExtensionUse {
                file: "target/a.wasm".to_string()
            })
        );
        assert_eq!(
            parsed(&["extension", "reset", "sessions"]),
            Ok(Subcommand::ExtensionReset {
                name: Some("sessions".to_string())
            })
        );
        assert_eq!(
            parsed(&["extension", "reset"]),
            Ok(Subcommand::ExtensionReset { name: None })
        );
        assert_eq!(
            parsed(&["extension", "list"]),
            Ok(Subcommand::ExtensionList)
        );
        // What is not one of them, or is short of what it takes, is a
        // usage error.
        for words in [
            &["extension"][..],
            &["extension", "use"],
            &["extension", "use", "a.wasm", "b.wasm"],
            &["extension", "list", "more"],
            &["extension", "swap", "a.wasm"],
        ] {
            assert!(parsed(words).is_err(), "{words:?}");
        }
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
    fn new_takes_a_split_and_active() {
        for (typed, split, activate) in [
            (&["new", "terminal", "--active"][..], None, true),
            (
                &["new", "terminal", "--split"],
                Some(SplitFlag::Here),
                false,
            ),
            // (Bare, when another flag follows it.)
            (
                &["new", "terminal", "--split", "--active"],
                Some(SplitFlag::Here),
                true,
            ),
            (
                &["new", "terminal", "--split", "s-1"],
                Some(SplitFlag::Explicit("s-1".to_string())),
                false,
            ),
            (
                &["new", "terminal", "--split=s-1"],
                Some(SplitFlag::Explicit("s-1".to_string())),
                false,
            ),
        ] {
            assert_eq!(
                parse(&args(typed)).unwrap().subcommand,
                new("terminal", &[], &[], split, activate),
                "{typed:?}"
            );
        }
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
            parse(&args(&["approve", "s-1", "c-1", "occ"]))
                .unwrap()
                .subcommand,
            Subcommand::Approve {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
                occurrence_id: "occ".into(),
            }
        );
        assert_eq!(
            parse(&args(&["deny", "s-1", "c-1", "occ"]))
                .unwrap()
                .subcommand,
            Subcommand::Deny {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
                occurrence_id: "occ".into(),
                reason: None,
            }
        );
        // `--reason <text>` (and the `--reason=<text>` form) ride as the
        // optional deny reason; the flag is recognized anywhere in the
        // argument list, like the other global flags.
        assert_eq!(
            parse(&args(&[
                "deny",
                "s-1",
                "c-1",
                "occ",
                "--reason",
                "too risky"
            ]))
            .unwrap()
            .subcommand,
            Subcommand::Deny {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
                occurrence_id: "occ".into(),
                reason: Some("too risky".to_string()),
            }
        );
        assert_eq!(
            parse(&args(&["deny", "--reason=too risky", "s-1", "c-1", "occ"]))
                .unwrap()
                .subcommand,
            Subcommand::Deny {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
                occurrence_id: "occ".into(),
                reason: Some("too risky".to_string()),
            }
        );
    }

    #[test]
    fn parses_send_with_and_without_text() {
        assert_eq!(
            parse(&args(&["send", "s-1", "fix the bug"]))
                .unwrap()
                .subcommand,
            Subcommand::Send {
                session_id: "s-1".to_string(),
                text: Some("fix the bug".to_string())
            }
        );
        // Omitted text -- `run` resolves it from stdin later; `parse` is
        // pure and never touches stdin, so it records `None` here.
        assert_eq!(
            parse(&args(&["send", "s-1"])).unwrap().subcommand,
            Subcommand::Send {
                session_id: "s-1".to_string(),
                text: None
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
            parse(&args(&["reload-terminal-runtime"]))
                .unwrap()
                .subcommand,
            Subcommand::ReloadTerminalRuntime
        );
        assert_eq!(
            parse(&args(&["reload-config"])).unwrap().subcommand,
            Subcommand::ReloadConfig
        );
        assert_eq!(
            parse(&args(&["open-terminal-in-session-directory"]))
                .unwrap()
                .subcommand,
            Subcommand::OpenTerminalInSessionDirectory
        );
    }

    #[test]
    fn missing_subcommand_is_a_usage_error() {
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["--json"])).is_err());
    }

    #[test]
    fn a_subcommand_that_is_not_this_program_s_is_a_plugin_s_command() {
        assert_eq!(
            parse(&args(&["back"])).unwrap().subcommand,
            Subcommand::PluginCommand {
                name: "back".to_string()
            }
        );
        // (A plugin's command takes nothing yet.)
        assert!(parse(&args(&["back", "now"])).is_err());
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
        assert!(parse(&args(&["send"])).is_err());
    }

    #[test]
    fn extra_positional_is_a_usage_error() {
        assert!(parse(&args(&["sessions", "extra"])).is_err());
        assert!(parse(&args(&["terminate-session", "s-1", "extra"])).is_err());
        assert!(parse(&args(&["attach", "s-1", "extra"])).is_err());
        assert!(parse(&args(&["send", "s-1", "a", "b"])).is_err());
    }

    #[test]
    fn dangling_flag_value_is_a_usage_error() {
        assert!(parse(&args(&["--socket"])).is_err());
        assert!(parse(&args(&["new", "agent", "--prompt"])).is_err());
        assert!(parse(&args(&["new", "agent", "--role"])).is_err());
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
        assert_eq!(
            resolve_socket_path(None, None),
            horizon_control::host::socket::default_socket_path()
        );
        assert_eq!(
            resolve_socket_path(None, Some(String::new())),
            horizon_control::host::socket::default_socket_path()
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
    fn resolved_split_for_resolves_a_new_pane_s() {
        let explicit = new(
            "terminal",
            &[],
            &[],
            Some(SplitFlag::Explicit("s-1".to_string())),
            false,
        );
        assert_eq!(
            resolved_split_for(&explicit, None),
            Ok(Some("s-1".to_string()))
        );
        let here = new("agent", &[], &[], Some(SplitFlag::Here), false);
        assert_eq!(
            resolved_split_for(&here, Some("s-2".to_string())),
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

    #[test]
    fn approval_commands_require_an_explicit_occurrence() {
        for command in ["approve", "deny"] {
            assert!(parse(&args(&[command, "session", "call"])).is_err());
        }
    }
}
