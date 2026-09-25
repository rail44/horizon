//! The GPUI shell's control plane: the transport (socket, listener,
//! per-connection handling) is `horizon_control::host`, with the client
//! side (`horizon <subcommand>`) in `crates/horizon-cli`. This module is
//! the UI-thread bridge over it -- `ChannelExecutor` hands each
//! `ControlRequest` to the GPUI event loop via a channel -- plus a
//! dispatcher over the shell's `execute()`/model. The external vocabulary
//! here mirrors every landed subsystem, including `reload-agent-runtime`
//! (the agentd drain/respawn/resume sequence,
//! `WorkspaceShell::reload_agent_runtime`).

mod invoke;

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use futures::StreamExt;
use gpui::*;
use horizon_control::contract::{EnvelopeBody, Invoke, Query, SessionEntry, Sessions, State};
use horizon_control::host::executor::{error_body, ControlExecutor, ControlRequest};
use horizon_control::host::listener;
use horizon_workspace::commands::{core_commands, CommandId};

use crate::workspace::WorkspaceShell;

const EXECUTE_TIMEOUT: Duration = Duration::from_secs(5);

/// The accept-thread wait for a *deferred* invoke. The reply arrives from a
/// background task after a daemon round-trip whose own budget is
/// `SYNC_REPLY_TIMEOUT` (60 s) plus the runtime's internal `OP_TIMEOUT`
/// (30 s), so the ordinary 5 s would report a spurious UI-thread timeout
/// while the switch actually lands.
const DEFERRED_EXECUTE_TIMEOUT: Duration = Duration::from_secs(70);

fn ok_body() -> EnvelopeBody {
    EnvelopeBody::Ok { session_id: None }
}

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
        let (timeout, timeout_message) = match &request {
            ControlRequest::Invoke(invoke) if invoke.command == "set-model" => (
                DEFERRED_EXECUTE_TIMEOUT,
                "timed out waiting for the agent runtime to answer the model switch",
            ),
            _ => (
                EXECUTE_TIMEOUT,
                "timed out waiting for the UI thread to answer",
            ),
        };
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
            .recv_timeout(timeout)
            .unwrap_or_else(|_| error_body(timeout_message))
    }
}

/// Binds `socket_path` and pumps accepted requests onto the shell
/// entity (external ops need a `Window` for reconcile/focus, hence the
/// window handle). Best-effort like the Floem shell's
/// `control_plane::start`: a bind failure (including another Horizon
/// instance already listening) logs and leaves external control
/// unavailable.
pub(crate) fn start(
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
    let mut async_rx = crate::runtime::event_stream(requests);
    cx.spawn(async move |cx| {
        while let Some(pending) = async_rx.next().await {
            let shell = shell.clone();
            // `None` means the invoke handed its reply to a background task
            // (only `set-model` does) -- sending here would race that task
            // into the single reply slot and block.
            let body = window
                .update(cx, |_, window, cx| {
                    shell
                        .update(cx, |shell, cx| match &pending.request {
                            ControlRequest::Invoke(invoke) => dispatch_invoke_or_defer(
                                shell,
                                invoke,
                                window,
                                cx,
                                pending.reply.clone(),
                            ),
                            ControlRequest::Query(query) => Some(dispatch_query(shell, query, cx)),
                        })
                        .unwrap_or_else(|_| Some(error_body("the workspace shell is gone")))
                })
                .unwrap_or_else(|_| Some(error_body("the window is gone")));
            if let Some(body) = body {
                let _ = pending.reply.send(body);
            }
        }
    })
    .detach();
}

/// Parse before executing; `None` transfers the reply to the model-switch task.
fn dispatch_invoke_or_defer(
    shell: &mut WorkspaceShell,
    invoke: &Invoke,
    window: &mut Window,
    cx: &mut Context<WorkspaceShell>,
    reply: Sender<EnvelopeBody>,
) -> Option<EnvelopeBody> {
    invoke::parse(invoke)
        .and_then(|command| dispatch_invoke(shell, command, window, cx, reply))
        .unwrap_or_else(|message| Some(error_body(message)))
}

fn dispatch_invoke(
    shell: &mut WorkspaceShell,
    command: invoke::Command,
    window: &mut Window,
    cx: &mut Context<WorkspaceShell>,
    reply: Sender<EnvelopeBody>,
) -> Result<Option<EnvelopeBody>, String> {
    use invoke::Command;
    match command {
        Command::NewSession {
            kind,
            role_id,
            split,
            issuer,
            activate,
            prompt,
            isolate,
        } => {
            let session_id = shell.control_plane_new_session(
                kind, role_id, split, issuer, activate, prompt, isolate, window, cx,
            )?;
            return Ok(Some(EnvelopeBody::Ok {
                session_id: Some(session_id),
            }));
        }
        Command::Preview {
            path,
            name,
            split,
            activate,
        } => {
            shell.control_plane_open_preview(path, name, split, activate, window, cx)?;
        }
        Command::Attach {
            session_id,
            activate,
        } => {
            shell.control_plane_attach_session(session_id, activate, window, cx)?;
        }
        Command::Terminate(session_id) => shell.control_plane_terminate(session_id, window, cx)?,
        Command::TerminateAllDetached => shell.control_plane_terminate_all_detached(window, cx),
        Command::Execute(id) => shell.execute_control_plane(id, window, cx),
        Command::Approve {
            session_id,
            identity,
        } => shell.control_plane_approve(session_id, identity, cx)?,
        Command::Deny {
            session_id,
            identity,
            reason,
        } => shell.control_plane_deny(session_id, identity, reason, cx)?,
        Command::CancelTurn(session_id) => shell.control_plane_cancel(session_id, cx)?,
        Command::ContinueTurn(session_id) => shell.control_plane_continue_turn(session_id, cx)?,
        Command::Send { session_id, text } => shell.control_plane_send(session_id, text, cx)?,
        Command::SetModel {
            session_id,
            provider,
            model,
        } => {
            return Ok(shell.control_plane_set_model(session_id, provider, model, reply, cx));
        }
    }
    Ok(Some(ok_body()))
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
                    kind: summary.kind.label().to_string(),
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
