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
use crate::workspace::{PaneKind, Workspace};

use super::command_actions::{execute_command, CommandActionState, CommandInvocation};
use super::commands::{core_commands, CommandId};

/// One row of the external-name <-> `CommandId` table, for the plain
/// (unparameterized, non-placement-aware) commands that map 1:1 --
/// `new-terminal`/`new-agent` (placement- and activation-aware, see
/// [`CommandInvocation::CreateSession`]), `attach`, and every
/// `session_id`/`call_id`-carrying command are handled directly in
/// [`invocation_from_external`] instead, since none of them have a single
/// context-free backing `CommandId`.
const SIMPLE_EXTERNAL_COMMANDS: &[(&str, CommandId)] = &[
    (
        "terminate-all-detached",
        CommandId::TerminateAllDetachedSessions,
    ),
    ("reload-agent-runtime", CommandId::ReloadAgentRuntime),
    ("reload-config", CommandId::ReloadConfig),
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
        "new-terminal" => create_session_invocation(PaneKind::Terminal, args, false),
        "new-agent" => create_session_invocation(PaneKind::Agent, args, true),
        "attach" => Ok(CommandInvocation::AttachSession {
            session_id: session_id_arg(args, "session_id")?,
            activate: activate_arg(args)?,
        }),
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

/// Builds `CommandInvocation::CreateSession` for `new-terminal`/`new-agent`
/// (`docs/cli-control-plane-design.md`'s "Placement vocabulary" decision):
/// `split` (a session id string) places the new pane next to that session's
/// pane instead of opening a new tab; `activate` defaults to `false` when
/// absent (the control plane never steals focus unless asked); `prompt` is
/// only accepted for `new-agent` (`allow_prompt`) -- the composite
/// create-with-prompt decision doesn't apply to `new-terminal`, which has no
/// analogous "first message" to send.
fn create_session_invocation(
    kind: PaneKind,
    args: &Value,
    allow_prompt: bool,
) -> Result<CommandInvocation, String> {
    let split_target = optional_session_id_arg(args, "split")?;
    let activate = activate_arg(args)?;
    let prompt = if allow_prompt {
        match args.get("prompt") {
            None | Some(Value::Null) => None,
            Some(Value::String(prompt)) => Some(prompt.clone()),
            Some(_) => return Err("`prompt` must be a string".to_string()),
        }
    } else {
        None
    };
    Ok(CommandInvocation::CreateSession {
        kind,
        split_target,
        activate,
        prompt,
    })
}

fn session_id_arg(args: &Value, key: &str) -> Result<SessionId, String> {
    let raw = string_arg(args, key)?;
    raw.parse::<Uuid>()
        .map(SessionId::from_uuid)
        .map_err(|_| format!("`{key}` must be a valid session id"))
}

/// Same as [`session_id_arg`] but `key`'s absence (or an explicit `null`) is
/// `Ok(None)` rather than an error -- for `CreateSession::split_target`,
/// which is genuinely optional (omitted means "open a new tab").
fn optional_session_id_arg(args: &Value, key: &str) -> Result<Option<SessionId>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => raw
            .parse::<Uuid>()
            .map(SessionId::from_uuid)
            .map(Some)
            .map_err(|_| format!("`{key}` must be a valid session id")),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

/// `CreateSession::activate`/`AttachSession::activate`'s wire
/// representation: an absent or `null` `activate` defaults to `false` --
/// `docs/cli-control-plane-design.md`'s "activate rides on creating/
/// attaching operations" decision (the control plane never dives unless
/// asked).
fn activate_arg(args: &Value) -> Result<bool, String> {
    match args.get("activate") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(activate)) => Ok(*activate),
        Some(_) => Err("`activate` must be a boolean".to_string()),
    }
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
/// `execute_command`; so does a [`CommandInvocation::CreateSession`] whose
/// `split_target` doesn't resolve to any pane right now (see
/// [`reject_unresolvable_split_target`]) -- the one case where "did the
/// operation actually do anything" needs to reach the wire as an explicit
/// error rather than a blanket `Ok`, since `execute_command`'s own return
/// type (`()`) gives every other caller no way to observe that afterward.
/// Every other successful dispatch always answers `Ok` regardless of
/// whether the underlying operation changed anything.
pub(crate) fn dispatch_invoke(invoke: &Invoke, command_state: &CommandActionState) -> EnvelopeBody {
    match invocation_from_external(&invoke.command, &invoke.args) {
        Ok(invocation) => match reject_unresolvable_split_target(&invocation, command_state) {
            Ok(()) => {
                execute_command(invocation, command_state.clone());
                EnvelopeBody::Ok
            }
            Err(message) => error_body(message),
        },
        Err(message) => error_body(message),
    }
}

/// `CommandInvocation::CreateSession`'s `split_target`, if set, must
/// currently be hosted by some pane -- checked here, before `execute_command`
/// runs, per the design doc's "Placement vocabulary" decision ("サーバ側:
/// セッション ID → ペイン解決...対象セッションがどのペインにも無い場合はエラ
/// ーを返す"). `Workspace::split_session_with_new_session` performs the same
/// check again when it actually runs (returning `None` rather than
/// panicking), so a target that detaches between this check and
/// `execute_command`'s call is still handled safely -- this function's job
/// is only to turn the common case into a wire-level error instead of a
/// silent no-op.
fn reject_unresolvable_split_target(
    invocation: &CommandInvocation,
    command_state: &CommandActionState,
) -> Result<(), String> {
    let CommandInvocation::CreateSession {
        split_target: Some(session_id),
        ..
    } = invocation
    else {
        return Ok(());
    };
    let hosted = command_state
        .workspace()
        .with_untracked(|workspace| workspace.session_is_referenced(*session_id));
    if hosted {
        Ok(())
    } else {
        Err(format!(
            "no pane currently hosts session `{}` to split next to",
            session_id.as_uuid()
        ))
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
    fn new_terminal_with_no_args_opens_a_new_tab_unactivated() {
        assert_eq!(
            invocation_from_external("new-terminal", &serde_json::json!({})),
            Ok(CommandInvocation::CreateSession {
                kind: PaneKind::Terminal,
                split_target: None,
                activate: false,
                prompt: None,
            })
        );
    }

    #[test]
    fn new_agent_without_a_prompt_opens_a_new_tab_unactivated() {
        assert_eq!(
            invocation_from_external("new-agent", &serde_json::json!({})),
            Ok(CommandInvocation::CreateSession {
                kind: PaneKind::Agent,
                split_target: None,
                activate: false,
                prompt: None,
            })
        );
        assert_eq!(
            invocation_from_external("new-agent", &serde_json::json!({ "prompt": null })),
            Ok(CommandInvocation::CreateSession {
                kind: PaneKind::Agent,
                split_target: None,
                activate: false,
                prompt: None,
            })
        );
    }

    #[test]
    fn new_agent_with_a_string_prompt_carries_it_on_create_session() {
        assert_eq!(
            invocation_from_external("new-agent", &serde_json::json!({ "prompt": "fix the bug" })),
            Ok(CommandInvocation::CreateSession {
                kind: PaneKind::Agent,
                split_target: None,
                activate: false,
                prompt: Some("fix the bug".to_string()),
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
    fn new_terminal_and_new_agent_accept_an_explicit_split_target() {
        let session_id = SessionId::new();
        assert_eq!(
            invocation_from_external(
                "new-terminal",
                &serde_json::json!({ "split": session_id.as_uuid().to_string() }),
            ),
            Ok(CommandInvocation::CreateSession {
                kind: PaneKind::Terminal,
                split_target: Some(session_id),
                activate: false,
                prompt: None,
            })
        );
        assert_eq!(
            invocation_from_external(
                "new-agent",
                &serde_json::json!({ "split": session_id.as_uuid().to_string() }),
            ),
            Ok(CommandInvocation::CreateSession {
                kind: PaneKind::Agent,
                split_target: Some(session_id),
                activate: false,
                prompt: None,
            })
        );
    }

    #[test]
    fn split_must_be_a_valid_session_id_when_given() {
        assert!(invocation_from_external(
            "new-terminal",
            &serde_json::json!({ "split": "not-a-uuid" }),
        )
        .is_err());
    }

    #[test]
    fn activate_true_opts_into_diving_and_defaults_false() {
        let session_id = SessionId::new();
        assert_eq!(
            invocation_from_external("new-terminal", &serde_json::json!({ "activate": true }),),
            Ok(CommandInvocation::CreateSession {
                kind: PaneKind::Terminal,
                split_target: None,
                activate: true,
                prompt: None,
            })
        );
        assert_eq!(
            invocation_from_external(
                "attach",
                &serde_json::json!({ "session_id": session_id.as_uuid().to_string() }),
            ),
            Ok(CommandInvocation::AttachSession {
                session_id,
                activate: false,
            })
        );
    }

    #[test]
    fn activate_must_be_a_boolean_when_given() {
        assert!(invocation_from_external(
            "new-terminal",
            &serde_json::json!({ "activate": "yes" }),
        )
        .is_err());
    }

    #[test]
    fn attach_requires_a_valid_session_id_and_defaults_activate_false() {
        let session_id = SessionId::new();
        assert_eq!(
            invocation_from_external(
                "attach",
                &serde_json::json!({
                    "session_id": session_id.as_uuid().to_string(),
                    "activate": true,
                }),
            ),
            Ok(CommandInvocation::AttachSession {
                session_id,
                activate: true,
            })
        );
        assert!(invocation_from_external("attach", &serde_json::json!({})).is_err());
        assert!(invocation_from_external(
            "attach",
            &serde_json::json!({ "session_id": "not-a-uuid" }),
        )
        .is_err());
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
    fn reload_config_maps_to_the_simple_command() {
        assert_eq!(
            invocation_from_external("reload-config", &serde_json::json!({})),
            Ok(CommandInvocation::Simple(CommandId::ReloadConfig))
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
            "reload-config",
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

    // --- CreateSession's activate/split wiring through dispatch_invoke -----

    #[test]
    fn dispatch_invoke_new_terminal_without_active_does_not_switch_the_active_session() {
        let state = test_state();
        let original_active_session = state
            .workspace()
            .with_untracked(|ws| ws.active_session_id());

        let body = dispatch_invoke(
            &Invoke {
                command: "new-terminal".to_string(),
                args: serde_json::json!({}),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Ok));
        assert_eq!(state.workspace().with_untracked(|ws| ws.tab_count()), 2);
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.active_session_id()),
            original_active_session,
            "activate defaults to false: the owner's active session must not change"
        );
    }

    #[test]
    fn dispatch_invoke_new_terminal_with_active_true_switches_to_the_new_session() {
        let state = test_state();
        let original_active_session = state
            .workspace()
            .with_untracked(|ws| ws.active_session_id());

        let body = dispatch_invoke(
            &Invoke {
                command: "new-terminal".to_string(),
                args: serde_json::json!({ "activate": true }),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Ok));
        assert_ne!(
            state
                .workspace()
                .with_untracked(|ws| ws.active_session_id()),
            original_active_session,
            "activate: true must switch to the new session"
        );
    }

    #[test]
    fn dispatch_invoke_rejects_a_split_target_with_no_hosting_pane() {
        let state = test_state();
        let unresolvable = SessionId::new();

        let body = dispatch_invoke(
            &Invoke {
                command: "new-terminal".to_string(),
                args: serde_json::json!({ "split": unresolvable.as_uuid().to_string() }),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Error(_)));
        assert_eq!(
            state.workspace().with_untracked(|ws| ws.tab_count()),
            1,
            "a rejected split target must not create anything"
        );
    }

    #[test]
    fn dispatch_invoke_accepts_a_split_target_hosted_by_a_pane() {
        let state = test_state();
        let target_session = state
            .workspace()
            .with_untracked(|ws| ws.active_session_id())
            .expect("mvp() starts with an active session");

        let body = dispatch_invoke(
            &Invoke {
                command: "new-terminal".to_string(),
                args: serde_json::json!({ "split": target_session.as_uuid().to_string() }),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Ok));
        assert_eq!(
            state.workspace().with_untracked(|ws| ws.tab_count()),
            1,
            "a resolvable split target must split in place, not open a new tab"
        );
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.visible_panes().len()),
            2
        );
    }

    #[test]
    fn dispatch_invoke_attach_reattaches_a_detached_session_without_activating_by_default() {
        let state = test_state();
        let detached_session = state
            .workspace()
            .with_untracked(|ws| ws.active_session_id())
            .expect("mvp() starts with an active session");
        state.workspace().update(|ws| {
            ws.split_active(PaneKind::Terminal, Some(SessionId::new()));
            ws.close_visible_pane(0);
        });
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.detached_session_count()),
            1
        );

        let body = dispatch_invoke(
            &Invoke {
                command: "attach".to_string(),
                args: serde_json::json!({ "session_id": detached_session.as_uuid().to_string() }),
            },
            &state,
        );

        assert!(matches!(body, EnvelopeBody::Ok));
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.detached_session_count()),
            0
        );
    }
}
