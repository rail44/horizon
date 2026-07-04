use floem::prelude::*;

use crate::agent::contract::{Command, ToolCallId};
use crate::agent::tools::{
    resolve_approval, unregister_session_runtime, ApprovalDecision, ApprovalOutcome,
};
use crate::app::commands::{command_enabled, CommandId};
use crate::control_surface::command_state;
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{
    request_active_pane_focus, PaneFocusRequests, PaneKind, SessionKind, Workspace,
};

use super::runtime::{spawn_session, SessionRuntimeState};

/// Reason recorded for a tool-call denial that doesn't carry an explicit
/// user-supplied reason — shared by the palette's auto-resolved
/// `DenyToolCall` and pane.rs's deny button so the literal isn't duplicated.
pub(crate) const DEFAULT_DENY_REASON: &str = "Denied by user";

#[derive(Clone)]
pub(crate) struct CommandActionState {
    pub(crate) runtime: SessionRuntimeState,
    pub(crate) pane_focus_requests: PaneFocusRequests,
}

impl CommandActionState {
    pub(crate) fn workspace(&self) -> RwSignal<Workspace> {
        self.runtime.workspace()
    }

    pub(crate) fn frames(&self) -> RwSignal<Frames> {
        self.runtime.frames()
    }

    pub(crate) fn sessions(&self) -> RwSignal<Registry> {
        self.runtime.sessions()
    }
}

/// A command ready to run. `Simple` is a catalog command with no inherent
/// target — used by the palette, which resolves a target for it on the fly
/// (see `find_pending_agent_approval`/`find_agent_turn_in_flight` below).
/// The other three variants carry an exact session id and are used by
/// direct UI bindings (e.g. a pane's approve/deny/cancel controls) that
/// already know which session they mean, so they skip target resolution
/// entirely — this is what lets an approval on a *detached* session (no
/// pane showing it) resolve at all.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommandInvocation {
    Simple(CommandId),
    ApproveToolCall {
        session_id: SessionId,
        call_id: ToolCallId,
    },
    DenyToolCall {
        session_id: SessionId,
        call_id: ToolCallId,
        reason: Option<String>,
    },
    CancelAgentTurn {
        session_id: SessionId,
    },
}

pub(crate) fn execute_command(invocation: CommandInvocation, state: CommandActionState) {
    match invocation {
        CommandInvocation::Simple(command_id) => execute_simple_command(command_id, state),
        CommandInvocation::ApproveToolCall {
            session_id,
            call_id,
        } => resolve_and_send_approval(state, session_id, call_id, ApprovalDecision::Approve),
        CommandInvocation::DenyToolCall {
            session_id,
            call_id,
            reason,
        } => resolve_and_send_approval(
            state,
            session_id,
            call_id,
            ApprovalDecision::Deny { reason },
        ),
        CommandInvocation::CancelAgentTurn { session_id } => cancel_agent_turn(state, session_id),
    }
}

fn execute_simple_command(command_id: CommandId, state: CommandActionState) {
    let workspace = state.workspace();
    let frames = state.frames();
    let snapshot = workspace.with_untracked(|ws| frames.with_untracked(|fr| command_state(ws, fr)));
    if !command_enabled(command_id, snapshot) {
        return;
    }

    match command_id {
        CommandId::NewTerminal => open_tab(state, PaneKind::Terminal),
        CommandId::NewAgent => {
            open_tab(state, PaneKind::Agent);
        }
        CommandId::SplitActivePane => {
            split_active_pane(state);
        }
        CommandId::FocusNextPane => {
            workspace.update(Workspace::focus_next);
            request_active_pane_focus(workspace, state.pane_focus_requests);
        }
        CommandId::CloseActivePane => {
            workspace.update(|ws| {
                ws.close_active_pane();
            });
        }
        CommandId::CloseActiveTab => {
            workspace.update(|ws| {
                ws.close_active_tab();
            });
        }
        CommandId::TerminateActiveSession => {
            terminate_active_session(workspace, state.frames(), state.sessions());
        }
        CommandId::ApproveToolCall => {
            let target = workspace.with_untracked(|ws| {
                frames.with_untracked(|fr| find_pending_agent_approval(ws, fr))
            });
            if let Some((session_id, call_id)) = target {
                resolve_and_send_approval(state, session_id, call_id, ApprovalDecision::Approve);
            }
        }
        CommandId::DenyToolCall => {
            let target = workspace.with_untracked(|ws| {
                frames.with_untracked(|fr| find_pending_agent_approval(ws, fr))
            });
            if let Some((session_id, call_id)) = target {
                resolve_and_send_approval(
                    state,
                    session_id,
                    call_id,
                    ApprovalDecision::Deny {
                        reason: Some(DEFAULT_DENY_REASON.to_string()),
                    },
                );
            }
        }
        CommandId::CancelAgentTurn => {
            let target = workspace
                .with_untracked(|ws| frames.with_untracked(|fr| find_agent_turn_in_flight(ws, fr)));
            if let Some(session_id) = target {
                cancel_agent_turn(state, session_id);
            }
        }
    }
}

fn open_tab(state: CommandActionState, kind: PaneKind) {
    let workspace = state.workspace();
    let mut session_id = None;
    workspace.update(|ws| {
        session_id = Some(ws.open_tab_with_new_session(kind));
    });
    let session_id = session_id.expect("new session");
    spawn_session(kind, session_id, &state.runtime);
    request_active_pane_focus(workspace, state.pane_focus_requests);
}

fn split_active_pane(state: CommandActionState) {
    let workspace = state.workspace();
    let mut split = None;
    workspace.update(|ws| {
        split = ws.split_active_with_new_session();
    });

    let Some((kind, session_id)) = split else {
        return;
    };
    spawn_session(kind, session_id, &state.runtime);
    request_active_pane_focus(workspace, state.pane_focus_requests);
}

fn terminate_active_session(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    let mut terminated = None;
    workspace.update(|ws| {
        terminated = ws.terminate_active_session();
    });

    let Some(session_id) = terminated else {
        return;
    };
    sessions.update(|registry| {
        registry.shutdown_terminal(session_id);
        registry.shutdown_agent(session_id);
    });
    // No-op for terminal sessions; for agent sessions this drops the
    // per-session tool state so a stale approval click can no longer
    // execute against a terminated session.
    unregister_session_runtime(session_id);
    frames.update(|frames| frames.remove_session(session_id));
}

/// Resolves a user's approve/deny decision for `session_id`'s pending tool
/// call and sends the resulting command to the provider, keyed purely by
/// session id via `Registry::agent_sender` — no pane or workspace lookup —
/// so this resolves identically whether or not any pane currently shows the
/// session.
///
/// `fs.write`/`fs.edit`/`bash` are executed by Horizon itself on approval —
/// see `agent::tools::resolve_approval` — so this also folds the execution
/// (or, for `bash`, the running-state) result into the session's frame
/// before sending. Every other approval-gated tool (e.g.
/// `mock.approval_required`) falls back to forwarding `ApproveToolCall`/
/// `DenyToolCall` to the provider unchanged.
fn resolve_and_send_approval(
    state: CommandActionState,
    session_id: SessionId,
    call_id: ToolCallId,
    decision: ApprovalDecision,
) {
    let frames = state.frames();
    let sessions = state.sessions();
    let frame = frames.with_untracked(|frames| frames.agent_frame(session_id));
    let command = match resolve_approval(&frame, session_id, call_id, decision) {
        ApprovalOutcome::Executed { frame, command } => {
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
            command
        }
        // `bash` on approve: the running-state frame is ready to publish,
        // but the tool is executing off the UI thread and hasn't produced a
        // result yet. Nothing to send to the provider here — the eventual
        // `Command::ToolCallResult` is sent later by the effect
        // `app/runtime/agent.rs::spawn_agent_session` sets up for it.
        ApprovalOutcome::Started { frame } => {
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
            return;
        }
        ApprovalOutcome::Forward(command) => command,
        // Duplicate click on a call that already resolved (double-approve,
        // or approve racing an earlier result) — nothing to run or send.
        ApprovalOutcome::AlreadyResolved => return,
    };
    if let Some(tx) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = tx.send(command);
    }
}

fn cancel_agent_turn(state: CommandActionState, session_id: SessionId) {
    if let Some(tx) = state
        .sessions()
        .with_untracked(|registry| registry.agent_sender(session_id))
    {
        let _ = tx.send(Command::Cancel { request_id: None });
    }
}

/// Scans every agent session the workspace knows about (attached or
/// detached) for one with a pending tool-call approval, returning the
/// first found. Used both by `Simple(CommandId::ApproveToolCall |
/// DenyToolCall)`'s auto-target resolution above and by the palette's
/// enabled-state check (`control_surface::items::command_state`).
///
/// Listing one palette entry per pending session (proper two-step target
/// selection) is future work per `docs/ux-principles.md`'s Command Palette
/// Direction. Taking the first match is the smaller-to-build option and is
/// sufficient to prove the frozen-detach case, since in practice there is
/// normally at most one pending approval at a time.
pub(crate) fn find_pending_agent_approval(
    workspace: &Workspace,
    frames: &Frames,
) -> Option<(SessionId, ToolCallId)> {
    workspace
        .session_summaries()
        .into_iter()
        .filter(|session| session.kind == SessionKind::Agent)
        .find_map(|session| {
            frames
                .agent_frame(session.id)
                .pending_approval_call_id()
                .map(|call_id| (session.id, call_id))
        })
}

/// Same idea as [`find_pending_agent_approval`] but for `CancelAgentTurn`:
/// the first agent session (attached or detached) with a turn in flight.
pub(crate) fn find_agent_turn_in_flight(
    workspace: &Workspace,
    frames: &Frames,
) -> Option<SessionId> {
    workspace
        .session_summaries()
        .into_iter()
        .filter(|session| session.kind == SessionKind::Agent)
        .find(|session| frames.agent_frame(session.id).is_turn_in_flight())
        .map(|session| session.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{AgentConfig, AgentPersistenceConfig, RigAgentConfig};
    use crate::agent::contract::{ApprovalRequest, SessionHandle, SessionState};
    use crate::agent::frame::{AgentFrame, AgentFrameItem};
    use crate::workspace::PaneKind;

    fn test_agent_config() -> AgentConfig {
        AgentConfig {
            rig: RigAgentConfig {
                openai_enabled: false,
                model: "test".to_string(),
            },
            persistence: AgentPersistenceConfig {
                event_log_path: std::env::temp_dir().join(format!(
                    "horizon-command-actions-test-{}.jsonl",
                    uuid::Uuid::new_v4()
                )),
                duckdb_path: None,
            },
        }
    }

    /// Builds a workspace with an agent session that has a pending approval
    /// but is detached — no pane in the workspace references it — plus a
    /// registered channel standing in for its running session, mirroring
    /// `control_surface::items`'s `palette_items_include_detached_sessions`
    /// detach recipe and `session::registry`'s `shutdown_removes_agent_session`
    /// channel setup.
    fn detached_pending_approval_fixture() -> (
        CommandActionState,
        SessionId,
        ToolCallId,
        crossbeam_channel::Receiver<Command>,
    ) {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.split_active(PaneKind::Agent, Some(session_id));
        workspace.close_visible_pane(1);
        assert!(
            workspace
                .detached_session_summaries()
                .iter()
                .any(|session| session.id == session_id),
            "session must be detached (no pane references it) before approving"
        );

        let call_id = ToolCallId("call-1".to_string());
        let mut frames = Frames::default();
        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: Some(SessionState::WaitingForApproval),
                items: vec![AgentFrameItem::ApprovalRequested(ApprovalRequest {
                    call_id: call_id.clone(),
                    reason: "needs approval".to_string(),
                })],
            },
        );

        let (tx, rx) = crossbeam_channel::unbounded();
        let (_events_tx, events_rx) = crossbeam_channel::unbounded();
        let mut sessions = Registry::default();
        sessions.insert_agent(session_id, SessionHandle::new(tx, events_rx));

        let runtime = SessionRuntimeState::new(
            RwSignal::new(workspace),
            RwSignal::new(frames),
            RwSignal::new(sessions),
            RwSignal::new(None),
            test_agent_config(),
            None,
            None,
        );
        let state = CommandActionState {
            runtime,
            pane_focus_requests: std::array::from_fn(|_| RwSignal::new(0_u64)),
        };

        (state, session_id, call_id, rx)
    }

    #[test]
    fn approve_tool_call_resolves_for_session_with_no_pane_attached() {
        let (state, session_id, call_id, rx) = detached_pending_approval_fixture();

        // The session stays detached throughout: no pane is ever touched to
        // resolve this approval, only the session id.
        assert!(state
            .workspace()
            .with_untracked(|ws| ws.detached_session_summaries())
            .iter()
            .any(|session| session.id == session_id));

        execute_command(
            CommandInvocation::ApproveToolCall {
                session_id,
                call_id: call_id.clone(),
            },
            state,
        );

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::ApproveToolCall { call_id: received }) if received == call_id
        ));
    }

    #[test]
    fn simple_approve_tool_call_auto_resolves_detached_session() {
        let (state, _session_id, call_id, rx) = detached_pending_approval_fixture();

        execute_command(CommandInvocation::Simple(CommandId::ApproveToolCall), state);

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::ApproveToolCall { call_id: received }) if received == call_id
        ));
    }
}
