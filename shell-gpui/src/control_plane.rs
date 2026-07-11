//! The GPUI shell's control plane: the transport (socket, listener,
//! per-connection handling) is `horizon_control::host`, shared with the
//! Floem shell; this module is the GPUI-side UI-thread bridge (the
//! `ChannelExecutor` counterpart of the Floem shell's
//! `control_plane::bridge`) plus a dispatcher over the shell's
//! `execute()`/model. The external vocabulary here is the subset whose
//! subsystems have landed — only `reload-agent-runtime` (the agentd
//! drain/respawn sequence) still returns an error, pending M5.

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use futures::StreamExt;
use gpui::*;
use horizon_control::contract::{EnvelopeBody, Invoke, Query, SessionEntry, Sessions, State};
use horizon_control::host::executor::{error_body, ControlExecutor, ControlRequest};
use horizon_control::host::listener;
use horizon_workspace::commands::{core_commands, CommandId};
use horizon_workspace::{PaneKind, SessionId, SplitAxis};

use crate::workspace::WorkspaceShell;

const EXECUTE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct PendingRequest {
    request: ControlRequest,
    reply: Sender<EnvelopeBody>,
}

struct ChannelExecutor {
    sender: Sender<PendingRequest>,
}

impl ControlExecutor for ChannelExecutor {
    fn execute(&self, request: ControlRequest) -> EnvelopeBody {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        if self
            .sender
            .send(PendingRequest {
                request,
                reply: reply_tx,
            })
            .is_err()
        {
            return error_body("control plane UI bridge is no longer running");
        }
        reply_rx
            .recv_timeout(EXECUTE_TIMEOUT)
            .unwrap_or_else(|_| error_body("timed out waiting for the UI thread to answer"))
    }
}

/// Binds `socket_path` and pumps accepted requests onto the shell
/// entity (external ops need a `Window` for reconcile/focus, hence the
/// window handle). Best-effort like the Floem shell's
/// `control_plane::start`: a bind failure (including another Horizon
/// instance already listening) logs and leaves external control
/// unavailable.
pub fn start(
    shell: WeakEntity<WorkspaceShell>,
    window: AnyWindowHandle,
    socket_path: std::path::PathBuf,
    cx: &mut App,
) {
    let (sender, requests) = crossbeam_channel::unbounded::<PendingRequest>();
    wire(requests, shell, window, cx);
    listener::spawn(socket_path, ChannelExecutor { sender });
}

fn wire(
    requests: Receiver<PendingRequest>,
    shell: WeakEntity<WorkspaceShell>,
    window: AnyWindowHandle,
    cx: &mut App,
) {
    let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
    std::thread::spawn(move || {
        while let Ok(pending) = requests.recv() {
            if async_tx.unbounded_send(pending).is_err() {
                return;
            }
        }
    });
    cx.spawn(async move |cx| {
        while let Some(pending) = async_rx.next().await {
            let shell = shell.clone();
            let body = window
                .update(cx, |_, window, cx| {
                    shell
                        .update(cx, |shell, cx| match &pending.request {
                            ControlRequest::Invoke(invoke) => {
                                dispatch_invoke(shell, invoke, window, cx)
                            }
                            ControlRequest::Query(query) => dispatch_query(shell, query, cx),
                        })
                        .unwrap_or_else(|_| error_body("the workspace shell is gone"))
                })
                .unwrap_or_else(|_| error_body("the window is gone"));
            let _ = pending.reply.send(body);
        }
    })
    .detach();
}

fn dispatch_invoke(
    shell: &mut WorkspaceShell,
    invoke: &Invoke,
    window: &mut Window,
    cx: &mut Context<WorkspaceShell>,
) -> EnvelopeBody {
    let args = &invoke.args;
    match invoke.command.as_str() {
        "new-terminal" | "new-agent" | "new-config-agent" => {
            let kind = if invoke.command == "new-terminal" {
                PaneKind::Terminal
            } else {
                PaneKind::Agent
            };
            let role_id = if invoke.command == "new-config-agent" {
                Some(config_agent_role_id())
            } else {
                None
            };
            let split = match optional_session_id_arg(args, "split") {
                Ok(split) => split,
                Err(message) => return error_body(message),
            };
            let activate = match activate_arg(args) {
                Ok(activate) => activate,
                Err(message) => return error_body(message),
            };
            let prompt = match args.get("prompt") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(prompt)) if kind == PaneKind::Agent => {
                    Some(prompt.clone())
                }
                Some(serde_json::Value::String(_)) => {
                    return error_body("`prompt` is only accepted for agent sessions")
                }
                Some(_) => return error_body("`prompt` must be a string"),
            };
            match shell.external_new_session(kind, role_id, split, activate, prompt, window, cx) {
                Ok(()) => EnvelopeBody::Ok,
                Err(message) => error_body(message),
            }
        }
        "attach" => {
            let session_id = match session_id_arg(args, "session_id") {
                Ok(id) => id,
                Err(message) => return error_body(message),
            };
            let activate = match activate_arg(args) {
                Ok(activate) => activate,
                Err(message) => return error_body(message),
            };
            match shell.external_attach(session_id, activate, window, cx) {
                Ok(()) => EnvelopeBody::Ok,
                Err(message) => error_body(message),
            }
        }
        "terminate-session" => {
            let session_id = match session_id_arg(args, "session_id") {
                Ok(id) => id,
                Err(message) => return error_body(message),
            };
            match shell.external_terminate(session_id, window, cx) {
                Ok(()) => EnvelopeBody::Ok,
                Err(message) => error_body(message),
            }
        }
        "terminate-all-detached" => {
            shell.external_terminate_all_detached(window, cx);
            EnvelopeBody::Ok
        }
        "reload-config" => {
            shell.execute_external(CommandId::ReloadConfig, window, cx);
            EnvelopeBody::Ok
        }
        "approve" => {
            let session_id = match session_id_arg(args, "session_id") {
                Ok(id) => id,
                Err(message) => return error_body(message),
            };
            let call_id = match call_id_arg(args, "call_id") {
                Ok(id) => id,
                Err(message) => return error_body(message),
            };
            match shell.external_approve(session_id, call_id, cx) {
                Ok(()) => EnvelopeBody::Ok,
                Err(message) => error_body(message),
            }
        }
        "deny" => {
            let session_id = match session_id_arg(args, "session_id") {
                Ok(id) => id,
                Err(message) => return error_body(message),
            };
            let call_id = match call_id_arg(args, "call_id") {
                Ok(id) => id,
                Err(message) => return error_body(message),
            };
            match shell.external_deny(session_id, call_id, cx) {
                Ok(()) => EnvelopeBody::Ok,
                Err(message) => error_body(message),
            }
        }
        "cancel-turn" => {
            let session_id = match session_id_arg(args, "session_id") {
                Ok(id) => id,
                Err(message) => return error_body(message),
            };
            match shell.external_cancel(session_id, cx) {
                Ok(()) => EnvelopeBody::Ok,
                Err(message) => error_body(message),
            }
        }
        // Still pending: the agentd drain/respawn sequence.
        other @ "reload-agent-runtime" => {
            error_body(format!("`{other}` is not available in this shell yet"))
        }
        other => error_body(format!("unknown external command `{other}`")),
    }
}

fn dispatch_query(
    shell: &WorkspaceShell,
    query: &Query,
    cx: &mut Context<WorkspaceShell>,
) -> EnvelopeBody {
    match query.what.as_str() {
        "sessions" => EnvelopeBody::Sessions(Sessions {
            sessions: shell
                .session_summaries()
                .into_iter()
                .map(|summary| SessionEntry {
                    session_id: summary.id.as_uuid().to_string(),
                    kind: format!("{:?}", summary.kind).to_ascii_lowercase(),
                    attached: summary.attached,
                    title: summary.title,
                })
                .collect(),
        }),
        "state" => {
            let state = shell.command_state_with(cx);
            EnvelopeBody::State(State {
                tab_count: state.tab_count,
                visible_pane_count: state.visible_pane_count,
                has_active_session: state.has_active_session,
                detached_session_count: state.detached_session_count,
                has_pending_approval: state.has_pending_approval,
                has_turn_in_flight: state.has_turn_in_flight,
                destructive_commands: destructive_commands(),
            })
        }
        other => error_body(format!("unknown query `{other}`")),
    }
}

/// The stable external names of every destructive command that is
/// currently enabled-relevant, mirroring the Floem shell's
/// `external_destructive_commands` (the CLI prompts before these).
fn destructive_commands() -> Vec<String> {
    core_commands()
        .into_iter()
        .filter(|spec| spec.destructive)
        .filter_map(|spec| match spec.id {
            CommandId::TerminateActiveSession => None, // no external name
            CommandId::TerminateAllDetachedSessions => Some("terminate-all-detached".to_string()),
            _ => None,
        })
        .chain(std::iter::once("terminate-session".to_string()))
        .collect()
}

fn session_id_arg(args: &serde_json::Value, key: &str) -> Result<SessionId, String> {
    match args.get(key) {
        Some(serde_json::Value::String(raw)) => raw
            .parse::<uuid::Uuid>()
            .map(SessionId::from_uuid)
            .map_err(|_| format!("`{key}` must be a UUID string")),
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("`{key}` is required")),
    }
}

fn optional_session_id_arg(
    args: &serde_json::Value,
    key: &str,
) -> Result<Option<(SessionId, SplitAxis)>, String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) => raw
            .parse::<uuid::Uuid>()
            .map(|uuid| Some((SessionId::from_uuid(uuid), SplitAxis::Horizontal)))
            .map_err(|_| format!("`{key}` must be a UUID string")),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn activate_arg(args: &serde_json::Value) -> Result<bool, String> {
    match args.get("activate") {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(activate)) => Ok(*activate),
        Some(_) => Err("`activate` must be a boolean".to_string()),
    }
}

fn call_id_arg(
    args: &serde_json::Value,
    key: &str,
) -> Result<horizon_agent::contract::ToolCallId, String> {
    match args.get(key) {
        Some(serde_json::Value::String(raw)) => {
            Ok(horizon_agent::contract::ToolCallId(raw.clone()))
        }
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("`{key}` is required")),
    }
}

/// `new-config-agent`'s fixed role id, mirroring the Floem shell's
/// `command_actions::config_agent_role_id`.
fn config_agent_role_id() -> horizon_agent::roles::RoleId {
    horizon_agent::roles::RoleId(horizon_agent::roles::CONFIG_ROLE.id.to_string())
}
