//! Stable, kebab-case external command names -- what a CLI client speaks
//! over the control-plane socket (`crates/horizon-control`) -- and their
//! conversion to/from Horizon's internal command model. See
//! `docs/cli-control-plane-design.md`'s "Command exposure" decision: an
//! internal `CommandId`/`CommandInvocation` rename never breaks a script
//! that only ever sees the names below (guardrail 6 of the agent-runtime
//! split, "keep a mapping table, not an implementation", applied to this
//! sibling contract).
//!
//! Deliberately a much smaller surface than the full `CommandId`/
//! `CommandInvocation` catalog: every entry here names an explicit target
//! where the underlying invocation needs one (`docs/cli-control-plane-
//! design.md`'s "Targets are explicit in v1" -- there is no external
//! equivalent of `Simple(CommandId::CloseActivePane)`/`TerminateActiveSession`,
//! since "active" is a human-cursor concept the CLI contract deliberately
//! never resolves implicitly).
//!
//! `src/app/keymap.rs`'s `command_id_from_str` is a partial, older
//! precedent for a similar idea (kebab-case name -> `CommandId`, for
//! `[keybindings]` config entries). Left separate rather than unified here:
//! it only ever needs a bare `CommandId` (config keybindings can't carry
//! arguments), it's missing entries this table needs
//! (`terminate-all-detached-sessions` has no keybinding entry at all today),
//! and unifying would mean deciding a single external vocabulary serves both
//! a config file *and* an external protocol with its own versioning story --
//! `docs/cli-control-plane-design.md` treats the CLI's names as
//! "provisional until Phase-1 delegation lands", which the config file's
//! names never were. Noted as a follow-up rather than done here, to keep
//! this change's surface area to the CLI control plane itself.
//!
//! [`dispatch_invoke`]/[`dispatch_query`] are this module's other half: once
//! a name/args pair (or a query's `what`) is validated, they run it against
//! a live [`CommandActionState`] and produce the wire-level
//! [`horizon_control::contract::EnvelopeBody`] a control-plane connection
//! sends back -- kept here (rather than in `control_plane::bridge`) so
//! they're directly unit-testable against the same `CommandActionState`
//! fixtures `command_actions`'s own tests already use; `control_plane` only
//! ever calls them, never reimplements them (its `bridge` module is
//! deliberately floem-plumbing-only).

use floem::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use horizon_control::contract::{EnvelopeBody, ErrorMessage, Invoke, Query, Sessions};

use crate::agent::contract::ToolCallId;
use crate::control_surface::command_state;
use crate::session::{Frames, SessionId};
use crate::workspace::Workspace;

use super::command_actions::{execute_command, CommandActionState, CommandInvocation};
use super::commands::{core_commands, CommandId};

/// One row of the external-name <-> `CommandId` table, for the plain
/// (unparameterized) commands that map 1:1 -- `new-agent`'s optional
/// `prompt`, and every `session_id`/`call_id`-carrying command, are handled
/// directly in [`invocation_from_external`] instead, since they don't have a
/// single backing `CommandId`.
const SIMPLE_EXTERNAL_COMMANDS: &[(&str, CommandId)] = &[
    ("new-terminal", CommandId::NewTerminal),
    (
        "terminate-all-detached",
        CommandId::TerminateAllDetachedSessions,
    ),
    ("reload-agent-runtime", CommandId::ReloadAgentRuntime),
];

/// Converts one external `{command, args}` pair (an `Invoke` request's
/// payload) into a `CommandInvocation`, validating `args`'s shape along the
/// way. An unknown command name or a missing/malformed argument is a plain
/// `Err(String)` -- [`dispatch_invoke`] turns that into an
/// `EnvelopeBody::Error` reply, never a panic or a silent no-op.
pub(crate) fn invocation_from_external(
    command: &str,
    args: &Value,
) -> Result<CommandInvocation, String> {
    if let Some((_, command_id)) = SIMPLE_EXTERNAL_COMMANDS
        .iter()
        .find(|(name, _)| *name == command)
    {
        return Ok(CommandInvocation::Simple(*command_id));
    }

    match command {
        "new-agent" => new_agent_invocation(args),
        "terminate-session" => Ok(CommandInvocation::TerminateSession {
            session_id: session_id_arg(args, "session_id")?,
        }),
        "approve" => Ok(CommandInvocation::ApproveToolCall {
            session_id: session_id_arg(args, "session_id")?,
            call_id: call_id_arg(args, "call_id")?,
        }),
        "deny" => Ok(CommandInvocation::DenyToolCall {
            session_id: session_id_arg(args, "session_id")?,
            call_id: call_id_arg(args, "call_id")?,
            reason: None,
        }),
        "cancel-turn" => Ok(CommandInvocation::CancelAgentTurn {
            session_id: session_id_arg(args, "session_id")?,
        }),
        other => Err(format!("unknown external command `{other}`")),
    }
}

fn new_agent_invocation(args: &Value) -> Result<CommandInvocation, String> {
    match args.get("prompt") {
        None | Some(Value::Null) => Ok(CommandInvocation::Simple(CommandId::NewAgent)),
        Some(Value::String(prompt)) => Ok(CommandInvocation::NewAgentWithPrompt {
            prompt: prompt.clone(),
        }),
        Some(_) => Err("`prompt` must be a string".to_string()),
    }
}

fn session_id_arg(args: &Value, key: &str) -> Result<SessionId, String> {
    let raw = string_arg(args, key)?;
    raw.parse::<Uuid>()
        .map(SessionId::from_uuid)
        .map_err(|_| format!("`{key}` must be a valid session id"))
}

fn call_id_arg(args: &Value, key: &str) -> Result<ToolCallId, String> {
    string_arg(args, key).map(ToolCallId)
}

fn string_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("`{key}` is required and must be a string"))
}

/// The stable external names of every currently-destructive external
/// command, for `Query { what: "state" }`'s reply (`docs/cli-control-plane-
/// design.md`'s "Authorization" decision: a CLI front-end uses this to
/// decide whether to prompt for confirmation before invoking one).
///
/// Derived, not redeclared: the plain commands' destructiveness comes
/// straight from `CommandSpec::destructive` via [`SIMPLE_EXTERNAL_COMMANDS`]
/// (today, only `terminate-all-detached` is both destructive and externally
/// exposed -- `TerminateActiveSession` is destructive too but has no
/// external name, since "active" is never an explicit CLI target).
/// `terminate-session` has no backing `CommandSpec` to derive from (it's
/// parameterized, not catalog-based) -- the same situation
/// `control_surface::view::row::palette_is_destructive` already resolved for
/// `PaletteItem::TerminateSession` by hardcoding `true` there; this mirrors
/// that precedent rather than duplicating a definition that doesn't exist
/// anywhere yet.
pub(crate) fn external_destructive_commands() -> Vec<String> {
    let mut names: Vec<String> = core_commands()
        .into_iter()
        .filter(|spec| spec.destructive)
        .filter_map(|spec| external_name_for(spec.id))
        .map(str::to_string)
        .collect();
    names.push("terminate-session".to_string());
    names
}

fn external_name_for(command_id: CommandId) -> Option<&'static str> {
    SIMPLE_EXTERNAL_COMMANDS
        .iter()
        .find(|(_, id)| *id == command_id)
        .map(|(name, _)| *name)
}

/// `Query { what: "sessions" }`'s reply: every session the workspace knows
/// about (attached or detached), reshaped into the contract's plain DTO.
pub(crate) fn sessions_query(
    workspace: &Workspace,
) -> Vec<horizon_control::contract::SessionEntry> {
    workspace
        .session_summaries()
        .into_iter()
        .map(|session| horizon_control::contract::SessionEntry {
            session_id: session.id.as_uuid().to_string(),
            kind: session.kind.label().to_string(),
            attached: session.attached,
            title: session.title,
        })
        .collect()
}

/// `Query { what: "state" }`'s reply: the same counts/flags
/// `control_surface::command_state` already computes for the palette, plus
/// [`external_destructive_commands`].
pub(crate) fn state_query(
    workspace: &Workspace,
    frames: &Frames,
) -> horizon_control::contract::State {
    let state = command_state(workspace, frames);
    horizon_control::contract::State {
        tab_count: state.tab_count,
        visible_pane_count: state.visible_pane_count,
        has_active_session: state.has_active_session,
        detached_session_count: state.detached_session_count,
        has_pending_approval: state.has_pending_approval,
        has_turn_in_flight: state.has_turn_in_flight,
        destructive_commands: external_destructive_commands(),
    }
}

/// Runs one `Invoke` request against `command_state`, through the exact same
/// `execute_command` every other surface (palette, keybindings) dispatches
/// through -- the one production call site is `control_plane::bridge`'s
/// effect. A conversion failure (bad command name, missing or malformed
/// argument) answers `EnvelopeBody::Error` without ever calling
/// `execute_command`; a successful dispatch always answers `Ok` -- whether
/// the underlying operation actually changed anything is not surfaced here,
/// matching `execute_command`'s own return type (`()`) for every existing
/// caller.
pub(crate) fn dispatch_invoke(invoke: &Invoke, command_state: &CommandActionState) -> EnvelopeBody {
    match invocation_from_external(&invoke.command, &invoke.args) {
        Ok(invocation) => {
            execute_command(invocation, command_state.clone());
            EnvelopeBody::Ok
        }
        Err(message) => error_body(message),
    }
}

/// Runs one `Query` request against `command_state`'s live `Workspace`/
/// `Frames` -- see [`sessions_query`]/[`state_query`]. `what` is an open
/// string in the contract (new query names can be added without a version
/// bump); an unrecognized one answers `EnvelopeBody::Error` here.
pub(crate) fn dispatch_query(query: &Query, command_state: &CommandActionState) -> EnvelopeBody {
    match query.what.as_str() {
        "sessions" => {
            let sessions = command_state.workspace().with_untracked(sessions_query);
            EnvelopeBody::Sessions(Sessions { sessions })
        }
        "state" => {
            let state = command_state.workspace().with_untracked(|workspace| {
                command_state
                    .frames()
                    .with_untracked(|frames| state_query(workspace, frames))
            });
            EnvelopeBody::State(state)
        }
        other => error_body(format!("unknown query `{other}`")),
    }
}

fn error_body(message: impl Into<String>) -> EnvelopeBody {
    EnvelopeBody::Error(ErrorMessage {
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agentd_runtime::AgentdConnection;
    use crate::app::runtime::SessionRuntimeState;
    use crate::session::Registry;
    use crate::workspace::Workspace;
    use floem::prelude::RwSignal;

    // --- invocation_from_external: one case per external command --------

    #[test]
    fn new_terminal_maps_to_the_simple_command() {
        assert_eq!(
            invocation_from_external("new-terminal", &serde_json::json!({})),
            Ok(CommandInvocation::Simple(CommandId::NewTerminal))
        );
    }

    #[test]
    fn new_agent_without_a_prompt_maps_to_the_simple_command() {
        assert_eq!(
            invocation_from_external("new-agent", &serde_json::json!({})),
            Ok(CommandInvocation::Simple(CommandId::NewAgent))
        );
        assert_eq!(
            invocation_from_external("new-agent", &serde_json::json!({ "prompt": null })),
            Ok(CommandInvocation::Simple(CommandId::NewAgent))
        );
    }

    #[test]
    fn new_agent_with_a_string_prompt_maps_to_the_composite_command() {
        assert_eq!(
            invocation_from_external("new-agent", &serde_json::json!({ "prompt": "fix the bug" })),
            Ok(CommandInvocation::NewAgentWithPrompt {
                prompt: "fix the bug".to_string()
            })
        );
    }

    #[test]
    fn new_agent_with_a_non_string_prompt_is_an_argument_error() {
        assert!(
            invocation_from_external("new-agent", &serde_json::json!({ "prompt": 5 })).is_err()
        );
    }

    #[test]
    fn terminate_session_requires_a_valid_session_id() {
        let session_id = SessionId::new();
        assert_eq!(
            invocation_from_external(
                "terminate-session",
                &serde_json::json!({ "session_id": session_id.as_uuid().to_string() }),
            ),
            Ok(CommandInvocation::TerminateSession { session_id })
        );
        assert!(invocation_from_external("terminate-session", &serde_json::json!({})).is_err());
        assert!(invocation_from_external(
            "terminate-session",
            &serde_json::json!({ "session_id": "not-a-uuid" }),
        )
        .is_err());
    }

    #[test]
    fn terminate_all_detached_maps_to_the_simple_command() {
        assert_eq!(
            invocation_from_external("terminate-all-detached", &serde_json::json!({})),
            Ok(CommandInvocation::Simple(
                CommandId::TerminateAllDetachedSessions
            ))
        );
    }

    #[test]
    fn approve_requires_session_id_and_call_id() {
        let session_id = SessionId::new();
        assert_eq!(
            invocation_from_external(
                "approve",
                &serde_json::json!({
                    "session_id": session_id.as_uuid().to_string(),
                    "call_id": "call-1",
                }),
            ),
            Ok(CommandInvocation::ApproveToolCall {
                session_id,
                call_id: ToolCallId("call-1".to_string()),
            })
        );
        assert!(invocation_from_external(
            "approve",
            &serde_json::json!({ "session_id": session_id.as_uuid().to_string() }),
        )
        .is_err());
    }

    #[test]
    fn deny_requires_session_id_and_call_id() {
        let session_id = SessionId::new();
        assert_eq!(
            invocation_from_external(
                "deny",
                &serde_json::json!({
                    "session_id": session_id.as_uuid().to_string(),
                    "call_id": "call-1",
                }),
            ),
            Ok(CommandInvocation::DenyToolCall {
                session_id,
                call_id: ToolCallId("call-1".to_string()),
                reason: None,
            })
        );
        assert!(invocation_from_external("deny", &serde_json::json!({})).is_err());
    }

    #[test]
    fn cancel_turn_requires_a_session_id() {
        let session_id = SessionId::new();
        assert_eq!(
            invocation_from_external(
                "cancel-turn",
                &serde_json::json!({ "session_id": session_id.as_uuid().to_string() }),
            ),
            Ok(CommandInvocation::CancelAgentTurn { session_id })
        );
        assert!(invocation_from_external("cancel-turn", &serde_json::json!({})).is_err());
    }

    #[test]
    fn reload_agent_runtime_maps_to_the_simple_command() {
        assert_eq!(
            invocation_from_external("reload-agent-runtime", &serde_json::json!({})),
            Ok(CommandInvocation::Simple(CommandId::ReloadAgentRuntime))
        );
    }

    #[test]
    fn an_unknown_command_name_is_an_error() {
        assert!(invocation_from_external("not-a-real-command", &serde_json::json!({})).is_err());
    }

    // --- destructive_commands is derived, not redeclared -----------------

    #[test]
    fn destructive_commands_include_terminate_all_detached_and_terminate_session() {
        let names = external_destructive_commands();
        assert!(names.contains(&"terminate-all-detached".to_string()));
        assert!(names.contains(&"terminate-session".to_string()));
    }

    #[test]
    fn destructive_commands_exclude_every_non_destructive_external_name() {
        let names = external_destructive_commands();
        for non_destructive in [
            "new-terminal",
            "new-agent",
            "approve",
            "deny",
            "cancel-turn",
            "reload-agent-runtime",
        ] {
            assert!(
                !names.contains(&non_destructive.to_string()),
                "{non_destructive} should not be listed as destructive"
            );
        }
    }

    // --- query DTOs --------------------------------------------------------

    #[test]
    fn sessions_query_reshapes_workspace_summaries() {
        let workspace = Workspace::mvp();
        let entries = sessions_query(&workspace);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "terminal");
        assert!(entries[0].attached);
    }

    #[test]
    fn state_query_reflects_workspace_counts_and_destructive_names() {
        let workspace = Workspace::mvp();
        let frames = Frames::default();
        let state = state_query(&workspace, &frames);
        assert_eq!(state.tab_count, 1);
        assert_eq!(state.visible_pane_count, 1);
        assert!(state
            .destructive_commands
            .contains(&"terminate-session".to_string()));
    }

    // --- dispatch_invoke / dispatch_query -----------------------------------

    fn test_state() -> CommandActionState {
        let runtime = SessionRuntimeState::new(
            RwSignal::new(Workspace::mvp()),
            RwSignal::new(Frames::default()),
            RwSignal::new(Registry::default()),
            RwSignal::new(None),
            None,
            None,
            RwSignal::new(Some(AgentdConnection::for_test())),
        );
        CommandActionState {
            runtime,
            pane_focus_requests: std::array::from_fn(|_| RwSignal::new(0_u64)),
        }
    }

    #[test]
    fn dispatch_invoke_runs_a_known_command_and_replies_ok() {
        let state = test_state();

        let body = dispatch_invoke(
            &Invoke {
                command: "new-terminal".to_string(),
                args: serde_json::json!({}),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Ok));
        assert_eq!(state.workspace().with_untracked(|ws| ws.tab_count()), 2);
    }

    #[test]
    fn dispatch_invoke_reports_an_unknown_command_as_an_error_without_running_anything() {
        let state = test_state();

        let body = dispatch_invoke(
            &Invoke {
                command: "not-a-real-command".to_string(),
                args: serde_json::json!({}),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Error(_)));
        assert_eq!(state.workspace().with_untracked(|ws| ws.tab_count()), 1);
    }

    #[test]
    fn dispatch_query_sessions_reflects_the_workspace() {
        let state = test_state();

        let body = dispatch_query(
            &Query {
                what: "sessions".to_string(),
            },
            &state,
        );

        match body {
            EnvelopeBody::Sessions(sessions) => assert_eq!(sessions.sessions.len(), 1),
            other => panic!("expected Sessions, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_query_state_includes_the_destructive_command_names() {
        let state = test_state();

        let body = dispatch_query(
            &Query {
                what: "state".to_string(),
            },
            &state,
        );

        match body {
            EnvelopeBody::State(state) => assert!(state
                .destructive_commands
                .contains(&"terminate-session".to_string())),
            other => panic!("expected State, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_query_reports_an_unknown_query_as_an_error() {
        let state = test_state();

        let body = dispatch_query(
            &Query {
                what: "not-a-real-query".to_string(),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Error(_)));
    }
}
