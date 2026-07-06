//! The external-name mapping table: turns a parsed [`crate::cli::Subcommand`]
//! into the stable string name and [`horizon_control::contract`] request
//! Horizon's control-plane endpoint expects, per the design doc's "Command
//! exposure" decision ("a mapping table, not an implementation"). The
//! server-side twin (external name -> internal `CommandId`) is a different
//! table, out of scope here -- this crate only ever speaks the external
//! vocabulary.

use horizon_control::contract::{Invoke, Query};

use crate::cli::Subcommand;

/// The wire request built for one subcommand: either an [`Invoke`]
/// (fire-and-forget-with-reply) or a [`Query`] (read-only snapshot), per
/// the design doc's "v1 operation shapes" decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Invoke(Invoke),
    Query(Query),
}

/// The stable external name for a subcommand -- identical to the CLI
/// subcommand string by construction (task spec: "外部名と1:1").
pub fn external_name(subcommand: &Subcommand) -> &'static str {
    match subcommand {
        Subcommand::NewTerminal { .. } => "new-terminal",
        Subcommand::NewAgent { .. } => "new-agent",
        Subcommand::Attach { .. } => "attach",
        Subcommand::TerminateSession { .. } => "terminate-session",
        Subcommand::TerminateAllDetached => "terminate-all-detached",
        Subcommand::Approve { .. } => "approve",
        Subcommand::Deny { .. } => "deny",
        Subcommand::CancelTurn { .. } => "cancel-turn",
        Subcommand::ReloadAgentRuntime => "reload-agent-runtime",
        Subcommand::ReloadConfig => "reload-config",
        Subcommand::Sessions => "sessions",
        Subcommand::State => "state",
    }
}

/// Whether this subcommand is even a *candidate* for the design doc's
/// client-side destructive confirmation -- independent of whether the
/// server currently lists it in `State::destructive_commands` (checked at
/// runtime; see `crate::run`). Only the two subcommands the task spec calls
/// out ever require the check.
pub fn is_destructive(subcommand: &Subcommand) -> bool {
    matches!(
        subcommand,
        Subcommand::TerminateSession { .. } | Subcommand::TerminateAllDetached
    )
}

/// Builds the wire request for `subcommand`. `resolved_split` is
/// `NewTerminal`/`NewAgent`'s `--split` flag already resolved against the
/// environment (see `cli::resolved_split_for`) -- by the time this function
/// runs, "here" has already become a concrete session id or the caller has
/// already bailed out, so the wire only ever carries an explicit target
/// (`docs/cli-control-plane-design.md`'s "Targets are explicit in v1"
/// principle); every other subcommand ignores this parameter. `args`
/// interpretation on the server side is entirely its own concern (per
/// [`Invoke`]'s doc comment); this function's only job is to fill it in
/// consistently.
pub fn to_request(subcommand: &Subcommand, resolved_split: Option<&str>) -> Request {
    match subcommand {
        Subcommand::NewTerminal { activate, .. } => invoke(
            "new-terminal",
            create_session_args(resolved_split, *activate, None),
        ),
        Subcommand::NewAgent {
            prompt, activate, ..
        } => invoke(
            "new-agent",
            create_session_args(resolved_split, *activate, prompt.as_deref()),
        ),
        Subcommand::Attach {
            session_id,
            activate,
        } => invoke(
            "attach",
            serde_json::json!({ "session_id": session_id, "activate": activate }),
        ),
        Subcommand::TerminateSession { session_id } => invoke(
            "terminate-session",
            serde_json::json!({ "session_id": session_id }),
        ),
        Subcommand::TerminateAllDetached => invoke("terminate-all-detached", serde_json::json!({})),
        Subcommand::Approve {
            session_id,
            call_id,
        } => invoke(
            "approve",
            serde_json::json!({ "session_id": session_id, "call_id": call_id }),
        ),
        Subcommand::Deny {
            session_id,
            call_id,
        } => invoke(
            "deny",
            serde_json::json!({ "session_id": session_id, "call_id": call_id }),
        ),
        Subcommand::CancelTurn { session_id } => invoke(
            "cancel-turn",
            serde_json::json!({ "session_id": session_id }),
        ),
        Subcommand::ReloadAgentRuntime => invoke("reload-agent-runtime", serde_json::json!({})),
        Subcommand::ReloadConfig => invoke("reload-config", serde_json::json!({})),
        Subcommand::Sessions => Request::Query(Query {
            what: "sessions".to_string(),
        }),
        Subcommand::State => Request::Query(Query {
            what: "state".to_string(),
        }),
    }
}

fn invoke(command: &str, args: serde_json::Value) -> Request {
    Request::Invoke(Invoke {
        command: command.to_string(),
        args,
    })
}

/// `new-terminal`/`new-agent`'s wire args -- the mirror image of
/// `app::external_commands::create_session_invocation`'s parsing on the
/// server side. `split`/`prompt` are only included when present, so an
/// unadorned `new-terminal`/`new-agent` sends the same `{"activate":false}`
/// shape v1 already sent (plus the always-present `activate` field the
/// Second revision adds).
fn create_session_args(
    split: Option<&str>,
    activate: bool,
    prompt: Option<&str>,
) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    if let Some(split) = split {
        args.insert(
            "split".to_string(),
            serde_json::Value::String(split.to_string()),
        );
    }
    args.insert("activate".to_string(), serde_json::Value::Bool(activate));
    if let Some(prompt) = prompt {
        args.insert(
            "prompt".to_string(),
            serde_json::Value::String(prompt.to_string()),
        );
    }
    serde_json::Value::Object(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::SplitFlag;

    fn new_terminal(split: Option<SplitFlag>, activate: bool) -> Subcommand {
        Subcommand::NewTerminal { split, activate }
    }

    fn new_agent(prompt: Option<&str>, split: Option<SplitFlag>, activate: bool) -> Subcommand {
        Subcommand::NewAgent {
            prompt: prompt.map(str::to_string),
            split,
            activate,
        }
    }

    #[test]
    fn external_name_matches_the_cli_subcommand_string() {
        assert_eq!(external_name(&new_terminal(None, false)), "new-terminal");
        assert_eq!(external_name(&new_agent(None, None, false)), "new-agent");
        assert_eq!(
            external_name(&Subcommand::Attach {
                session_id: "s-1".to_string(),
                activate: false,
            }),
            "attach"
        );
        assert_eq!(
            external_name(&Subcommand::TerminateAllDetached),
            "terminate-all-detached"
        );
        assert_eq!(external_name(&Subcommand::Sessions), "sessions");
        assert_eq!(external_name(&Subcommand::State), "state");
    }

    #[test]
    fn only_the_two_terminate_subcommands_are_destructive_candidates() {
        assert!(is_destructive(&Subcommand::TerminateAllDetached));
        assert!(is_destructive(&Subcommand::TerminateSession {
            session_id: "s-1".to_string()
        }));
        assert!(!is_destructive(&new_terminal(None, false)));
        assert!(!is_destructive(&Subcommand::Attach {
            session_id: "s-1".to_string(),
            activate: false,
        }));
        assert!(!is_destructive(&Subcommand::CancelTurn {
            session_id: "s-1".to_string()
        }));
        assert!(!is_destructive(&Subcommand::Sessions));
    }

    #[test]
    fn new_terminal_with_no_flags_sends_activate_false_and_no_split() {
        let Request::Invoke(invoke) = to_request(&new_terminal(None, false), None) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "new-terminal");
        assert_eq!(invoke.args, serde_json::json!({ "activate": false }));
    }

    #[test]
    fn new_terminal_with_active_sends_activate_true() {
        let Request::Invoke(invoke) = to_request(&new_terminal(None, true), None) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.args, serde_json::json!({ "activate": true }));
    }

    #[test]
    fn new_terminal_carries_the_resolved_split_target() {
        let Request::Invoke(invoke) =
            to_request(&new_terminal(Some(SplitFlag::Here), false), Some("s-1"))
        else {
            panic!("expected an Invoke request");
        };
        assert_eq!(
            invoke.args,
            serde_json::json!({ "split": "s-1", "activate": false })
        );
    }

    #[test]
    fn new_agent_without_prompt_sends_activate_false_and_no_prompt() {
        let Request::Invoke(invoke) = to_request(&new_agent(None, None, false), None) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "new-agent");
        assert_eq!(invoke.args, serde_json::json!({ "activate": false }));
    }

    #[test]
    fn new_agent_with_prompt_and_split_and_active_carries_all_three() {
        let Request::Invoke(invoke) = to_request(
            &new_agent(
                Some("fix the bug"),
                Some(SplitFlag::Explicit("s-1".to_string())),
                true,
            ),
            Some("s-1"),
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(
            invoke.args,
            serde_json::json!({ "split": "s-1", "activate": true, "prompt": "fix the bug" })
        );
    }

    #[test]
    fn attach_carries_the_session_id_and_activate() {
        let Request::Invoke(invoke) = to_request(
            &Subcommand::Attach {
                session_id: "s-1".to_string(),
                activate: true,
            },
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "attach");
        assert_eq!(
            invoke.args,
            serde_json::json!({ "session_id": "s-1", "activate": true })
        );
    }

    #[test]
    fn terminate_session_carries_the_session_id() {
        let Request::Invoke(invoke) = to_request(
            &Subcommand::TerminateSession {
                session_id: "s-1".to_string(),
            },
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "terminate-session");
        assert_eq!(invoke.args, serde_json::json!({ "session_id": "s-1" }));
    }

    #[test]
    fn approve_and_deny_carry_both_ids() {
        let Request::Invoke(approve) = to_request(
            &Subcommand::Approve {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
            },
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(approve.command, "approve");
        assert_eq!(
            approve.args,
            serde_json::json!({ "session_id": "s-1", "call_id": "c-1" })
        );

        let Request::Invoke(deny) = to_request(
            &Subcommand::Deny {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
            },
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(deny.command, "deny");
    }

    #[test]
    fn reload_agent_runtime_and_reload_config_are_bare_invokes() {
        assert_eq!(
            external_name(&Subcommand::ReloadAgentRuntime),
            "reload-agent-runtime"
        );
        assert_eq!(external_name(&Subcommand::ReloadConfig), "reload-config");

        let Request::Invoke(invoke) = to_request(&Subcommand::ReloadConfig, None) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "reload-config");
        assert_eq!(invoke.args, serde_json::json!({}));
    }

    #[test]
    fn sessions_and_state_are_queries() {
        assert!(matches!(
            to_request(&Subcommand::Sessions, None),
            Request::Query(q) if q.what == "sessions"
        ));
        assert!(matches!(
            to_request(&Subcommand::State, None),
            Request::Query(q) if q.what == "state"
        ));
    }
}
