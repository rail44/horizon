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
        Subcommand::NewTerminal => "new-terminal",
        Subcommand::NewAgent { .. } => "new-agent",
        Subcommand::TerminateSession { .. } => "terminate-session",
        Subcommand::TerminateAllDetached => "terminate-all-detached",
        Subcommand::Approve { .. } => "approve",
        Subcommand::Deny { .. } => "deny",
        Subcommand::CancelTurn { .. } => "cancel-turn",
        Subcommand::ReloadAgentRuntime => "reload-agent-runtime",
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

/// Builds the wire request for `subcommand`. `args` interpretation on the
/// server side is entirely its own concern (per [`Invoke`]'s doc comment);
/// this function's only job is to fill it in consistently.
pub fn to_request(subcommand: &Subcommand) -> Request {
    match subcommand {
        Subcommand::NewTerminal => invoke("new-terminal", serde_json::json!({})),
        Subcommand::NewAgent { prompt } => invoke(
            "new-agent",
            match prompt {
                Some(prompt) => serde_json::json!({ "prompt": prompt }),
                None => serde_json::json!({}),
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_name_matches_the_cli_subcommand_string() {
        assert_eq!(external_name(&Subcommand::NewTerminal), "new-terminal");
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
        assert!(!is_destructive(&Subcommand::NewTerminal));
        assert!(!is_destructive(&Subcommand::CancelTurn {
            session_id: "s-1".to_string()
        }));
        assert!(!is_destructive(&Subcommand::Sessions));
    }

    #[test]
    fn new_agent_without_prompt_sends_empty_args() {
        let Request::Invoke(invoke) = to_request(&Subcommand::NewAgent { prompt: None }) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "new-agent");
        assert_eq!(invoke.args, serde_json::json!({}));
    }

    #[test]
    fn new_agent_with_prompt_carries_it_in_args() {
        let Request::Invoke(invoke) = to_request(&Subcommand::NewAgent {
            prompt: Some("fix the bug".to_string()),
        }) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.args, serde_json::json!({ "prompt": "fix the bug" }));
    }

    #[test]
    fn terminate_session_carries_the_session_id() {
        let Request::Invoke(invoke) = to_request(&Subcommand::TerminateSession {
            session_id: "s-1".to_string(),
        }) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "terminate-session");
        assert_eq!(invoke.args, serde_json::json!({ "session_id": "s-1" }));
    }

    #[test]
    fn approve_and_deny_carry_both_ids() {
        let Request::Invoke(approve) = to_request(&Subcommand::Approve {
            session_id: "s-1".to_string(),
            call_id: "c-1".to_string(),
        }) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(approve.command, "approve");
        assert_eq!(
            approve.args,
            serde_json::json!({ "session_id": "s-1", "call_id": "c-1" })
        );

        let Request::Invoke(deny) = to_request(&Subcommand::Deny {
            session_id: "s-1".to_string(),
            call_id: "c-1".to_string(),
        }) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(deny.command, "deny");
    }

    #[test]
    fn sessions_and_state_are_queries() {
        assert!(
            matches!(to_request(&Subcommand::Sessions), Request::Query(q) if q.what == "sessions")
        );
        assert!(matches!(to_request(&Subcommand::State), Request::Query(q) if q.what == "state"));
    }
}
