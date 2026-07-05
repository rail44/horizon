use floem::prelude::*;

use crate::agent::agentd_runtime::AgentdConnection;
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

    pub(crate) fn agent_state_status(&self) -> RwSignal<Option<String>> {
        self.runtime.agent_state_status()
    }

    pub(crate) fn agentd_connection(&self) -> RwSignal<Option<AgentdConnection>> {
        self.runtime.agentd_connection()
    }
}

/// A command ready to run. `Simple` is a catalog command with no inherent
/// target — used by the palette, which resolves a target for it on the fly
/// (see `find_pending_agent_approval`/`find_agent_turn_in_flight` below).
/// Every other variant carries an explicit target and is used by direct UI
/// bindings (a pane's approve/deny/cancel controls, a pane/tab close
/// button, a tab chip click, a palette/overview row) that already know
/// which pane/tab/session they mean, so they skip target resolution
/// entirely — this is what lets, e.g., an approval or a terminate on a
/// *detached* session (no pane showing it) resolve at all.
///
/// `ClosePane`/`CloseTab`/`ActivateTab`/`ActivatePane` target a visible
/// index rather than a stable id: the workspace model only tracks
/// `PaneId`/`TabId` internally (see `workspace::types::id`), and every
/// `Workspace` method backing these operations already takes a visible
/// index (`close_visible_pane`, `close_tab_index`, `activate_tab_index`,
/// `activate_pane_index`), so there is no stable id available to prefer at
/// the call sites this enum serves today. `AttachSession` and
/// `TerminateSession` target a `SessionId` instead, since that's stable
/// across attach/detach and is what the workspace already keys sessions by.
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
    /// Close a specific visible pane (pane header's × button), whether or
    /// not it's the active pane.
    ClosePane {
        index: usize,
    },
    /// Close a specific tab (tab chip's × button), whether or not it's the
    /// active tab.
    CloseTab {
        index: usize,
    },
    /// Activate a specific tab (tab chip click, palette tab row, overview
    /// tab row).
    ActivateTab {
        index: usize,
    },
    /// Activate a specific pane within a specific tab (overview pane row).
    ActivatePane {
        tab_index: usize,
        pane_index: usize,
    },
    /// Attach a detached session to a new split in the active tab (palette
    /// or overview detached-session row).
    AttachSession {
        session_id: SessionId,
    },
    /// Terminate a session by id, whether or not it's the active session or
    /// attached to any pane — reuses the same registry/frame cleanup as
    /// `Simple(CommandId::TerminateActiveSession)`.
    TerminateSession {
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
        CommandInvocation::ClosePane { index } => {
            state.workspace().update(|ws| {
                ws.close_visible_pane(index);
            });
        }
        CommandInvocation::CloseTab { index } => {
            state.workspace().update(|ws| {
                ws.close_tab_index(index);
            });
        }
        CommandInvocation::ActivateTab { index } => {
            let workspace = state.workspace();
            workspace.update(|ws| {
                ws.activate_tab_index(index);
            });
            request_active_pane_focus(workspace, state.pane_focus_requests);
        }
        CommandInvocation::ActivatePane {
            tab_index,
            pane_index,
        } => {
            let workspace = state.workspace();
            workspace.update(|ws| {
                ws.activate_pane_index(tab_index, pane_index);
            });
            request_active_pane_focus(workspace, state.pane_focus_requests);
        }
        CommandInvocation::AttachSession { session_id } => {
            let workspace = state.workspace();
            workspace.update(|ws| {
                ws.attach_existing_session_to_split(session_id);
            });
            request_active_pane_focus(workspace, state.pane_focus_requests);
        }
        CommandInvocation::TerminateSession { session_id } => {
            terminate_session_by_id(
                state.workspace(),
                state.frames(),
                state.sessions(),
                session_id,
            );
        }
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
        CommandId::TerminateAllDetachedSessions => {
            terminate_all_detached_sessions(workspace, state.frames(), state.sessions());
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
        CommandId::ReloadAgentRuntime => reload_agent_runtime(state),
    }
}

/// `docs/agent-runtime-split-design.md` step 4's `Reload Agent Runtime`
/// command: drain -> wait for exit -> spawn the (possibly rebuilt) binary ->
/// reconnect -> `session_load` every session -- all implemented in
/// `agent::agentd_runtime::reload_agent_runtime`, which this just gathers
/// the right signals to call. Progress and the eventual result surface
/// through `agent_state_status`, the same status the status bar already
/// renders.
fn reload_agent_runtime(state: CommandActionState) {
    let current = state.agentd_connection().get_untracked();
    crate::agent::agentd_runtime::reload_agent_runtime(
        current,
        state.workspace(),
        state.frames(),
        state.sessions(),
        state.agentd_connection(),
        state.agent_state_status(),
    );
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
    cleanup_terminated_session(session_id, frames, sessions);
}

/// Same effect as `terminate_active_session` but targets an explicit
/// session id via `Workspace::terminate_session` rather than
/// `Workspace::terminate_active_session` — this is what lets
/// `CommandInvocation::TerminateSession` end a *detached* session (no pane
/// referencing it, so it isn't reachable through the workspace's notion of
/// "active") without first reattaching it.
fn terminate_session_by_id(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    session_id: SessionId,
) {
    let mut terminated = false;
    workspace.update(|ws| {
        terminated = ws.terminate_session(session_id);
    });

    if !terminated {
        return;
    }
    cleanup_terminated_session(session_id, frames, sessions);
}

/// `Simple(CommandId::TerminateAllDetachedSessions)`'s bulk cleanup: snapshots
/// every currently-detached session id, then runs each through
/// `terminate_session_by_id` — the exact same per-session machinery
/// (`Workspace::terminate_session` + `cleanup_terminated_session`'s registry
/// shutdown, runtime unregistration, and frame removal) that a single
/// `CommandInvocation::TerminateSession` uses. The id list is snapshotted up
/// front rather than recomputed each iteration, so a session detaching or
/// attaching mid-loop (there is no way for one to today, since this all runs
/// synchronously on the UI thread, but the snapshot makes it robust and
/// matches the read-then-act shape `terminate_active_session` already uses)
/// can't change which sessions this call targets. Attached sessions never
/// appear in the snapshot, so they're left untouched.
fn terminate_all_detached_sessions(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    let detached_ids: Vec<SessionId> = workspace
        .with_untracked(|ws| ws.detached_session_summaries())
        .into_iter()
        .map(|session| session.id)
        .collect();

    for session_id in detached_ids {
        terminate_session_by_id(workspace, frames, sessions, session_id);
    }
}

/// Registry/frame cleanup shared by both terminate paths above.
fn cleanup_terminated_session(
    session_id: SessionId,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    sessions.update(|registry| {
        registry.shutdown_terminal(session_id);
        registry.shutdown_agent(session_id);
    });
    // No-op for terminal sessions; for agent sessions this drops the
    // per-session tool state so a stale approval click can no longer
    // execute against a terminated session.
    unregister_session_runtime(session_id.into());
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
    let command = match resolve_approval(&frame, session_id.into(), call_id, decision) {
        ApprovalOutcome::Executed { frame, command, .. } => {
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
            command
        }
        // `bash` on approve: the running-state frame is ready to publish,
        // but the tool is executing off the UI thread and hasn't produced a
        // result yet. Nothing to send to the provider here — the eventual
        // `Command::ToolCallResult` is sent later by the effect
        // `app/runtime/agent.rs::spawn_agent_session` sets up for it.
        ApprovalOutcome::Started { frame, .. } => {
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
    use crate::agent::contract::{ApprovalRequest, SessionHandle, SessionState};
    use crate::agent::frame::{AgentFrame, AgentFrameItem};
    use crate::terminal::TerminalCommand;
    use crate::workspace::PaneKind;

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
            None,
            None,
            RwSignal::new(None),
        );
        let state = CommandActionState {
            runtime,
            pane_focus_requests: std::array::from_fn(|_| RwSignal::new(0_u64)),
        };

        (state, session_id, call_id, rx)
    }

    /// A minimal `CommandActionState` wrapping the given workspace, with
    /// empty frames/sessions — for tests of the targeted-index/session
    /// invocations below that don't need a running session behind them.
    fn test_command_action_state(workspace: Workspace) -> CommandActionState {
        let runtime = SessionRuntimeState::new(
            RwSignal::new(workspace),
            RwSignal::new(Frames::default()),
            RwSignal::new(Registry::default()),
            RwSignal::new(None),
            None,
            None,
            RwSignal::new(None),
        );
        CommandActionState {
            runtime,
            pane_focus_requests: std::array::from_fn(|_| RwSignal::new(0_u64)),
        }
    }

    /// One attached terminal session plus two detached sessions (a terminal
    /// and an agent) — the mixed fixture
    /// `terminate_all_detached_sessions_ends_only_the_detached_sessions`
    /// below needs to prove the bulk command leaves the attached session
    /// alone. Registry entries mirror `detached_pending_approval_fixture`'s
    /// channel setup so each session's cleanup (or lack of it) is
    /// observable.
    fn mixed_attached_and_detached_fixture() -> (
        CommandActionState,
        SessionId,
        SessionId,
        SessionId,
        crossbeam_channel::Receiver<TerminalCommand>,
        crossbeam_channel::Receiver<TerminalCommand>,
        crossbeam_channel::Receiver<Command>,
    ) {
        let mut workspace = Workspace::mvp();
        let attached_session = workspace
            .active_terminal_session_id()
            .expect("mvp() starts with an active terminal session");

        let detached_terminal = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(detached_terminal));
        workspace.close_visible_pane(1);

        let detached_agent = SessionId::new();
        workspace.split_active(PaneKind::Agent, Some(detached_agent));
        workspace.close_visible_pane(1);

        assert_eq!(
            workspace.detached_session_count(),
            2,
            "fixture must start with exactly the two detached sessions under test"
        );

        let mut sessions = Registry::default();
        let (attached_tx, attached_rx) = crossbeam_channel::unbounded();
        sessions.insert_terminal(attached_session, attached_tx);
        let (detached_terminal_tx, detached_terminal_rx) = crossbeam_channel::unbounded();
        sessions.insert_terminal(detached_terminal, detached_terminal_tx);
        let (detached_agent_tx, detached_agent_rx) = crossbeam_channel::unbounded();
        let (_events_tx, events_rx) = crossbeam_channel::unbounded();
        sessions.insert_agent(
            detached_agent,
            SessionHandle::new(detached_agent_tx, events_rx),
        );

        let runtime = SessionRuntimeState::new(
            RwSignal::new(workspace),
            RwSignal::new(Frames::default()),
            RwSignal::new(sessions),
            RwSignal::new(None),
            None,
            None,
            RwSignal::new(None),
        );
        let state = CommandActionState {
            runtime,
            pane_focus_requests: std::array::from_fn(|_| RwSignal::new(0_u64)),
        };

        (
            state,
            attached_session,
            detached_terminal,
            detached_agent,
            attached_rx,
            detached_terminal_rx,
            detached_agent_rx,
        )
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

    #[test]
    fn close_pane_invocation_closes_the_targeted_pane() {
        let mut workspace = Workspace::mvp();
        let second_session = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(second_session));
        let state = test_command_action_state(workspace);

        execute_command(CommandInvocation::ClosePane { index: 1 }, state.clone());

        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.visible_panes().len()),
            1
        );
        assert!(state
            .workspace()
            .with_untracked(|ws| ws.detached_session_summaries())
            .iter()
            .any(|session| session.id == second_session));
    }

    #[test]
    fn close_tab_invocation_closes_the_targeted_tab() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab(PaneKind::Agent, None);
        let state = test_command_action_state(workspace);
        assert_eq!(state.workspace().with_untracked(|ws| ws.tab_count()), 2);

        execute_command(CommandInvocation::CloseTab { index: 0 }, state.clone());

        assert_eq!(state.workspace().with_untracked(|ws| ws.tab_count()), 1);
    }

    #[test]
    fn activate_tab_invocation_switches_the_active_tab() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab(PaneKind::Agent, None);
        let state = test_command_action_state(workspace);
        assert_eq!(
            state.workspace().with_untracked(|ws| ws.active_tab_index()),
            1
        );

        execute_command(CommandInvocation::ActivateTab { index: 0 }, state.clone());

        assert_eq!(
            state.workspace().with_untracked(|ws| ws.active_tab_index()),
            0
        );
    }

    #[test]
    fn activate_pane_invocation_switches_tab_and_pane() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.open_tab(PaneKind::Agent, None);
        let state = test_command_action_state(workspace);
        assert_eq!(
            state.workspace().with_untracked(|ws| ws.active_tab_index()),
            1
        );

        execute_command(
            CommandInvocation::ActivatePane {
                tab_index: 0,
                pane_index: 0,
            },
            state.clone(),
        );

        assert_eq!(
            state.workspace().with_untracked(|ws| ws.active_tab_index()),
            0
        );
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.active_visible_index()),
            0
        );
    }

    #[test]
    fn attach_session_invocation_reattaches_a_detached_session() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(session_id));
        workspace.close_visible_pane(1);
        let state = test_command_action_state(workspace);
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.detached_session_count()),
            1
        );

        execute_command(
            CommandInvocation::AttachSession { session_id },
            state.clone(),
        );

        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.detached_session_count()),
            0
        );
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.visible_panes().len()),
            2
        );
    }

    #[test]
    fn terminate_session_ends_a_detached_session() {
        let (state, session_id, _call_id, rx) = detached_pending_approval_fixture();
        let workspace = state.workspace();
        let sessions = state.sessions();

        execute_command(CommandInvocation::TerminateSession { session_id }, state);

        assert!(!workspace
            .with_untracked(|ws| ws.session_summaries())
            .iter()
            .any(|session| session.id == session_id));
        assert!(sessions
            .with_untracked(|registry| registry.agent_sender(session_id))
            .is_none());
        assert!(matches!(rx.try_recv(), Ok(Command::Shutdown)));
    }

    #[test]
    fn terminate_all_detached_sessions_ends_only_the_detached_sessions() {
        let (
            state,
            attached_session,
            detached_terminal,
            detached_agent,
            attached_rx,
            detached_terminal_rx,
            detached_agent_rx,
        ) = mixed_attached_and_detached_fixture();
        let workspace = state.workspace();
        let sessions = state.sessions();

        execute_command(
            CommandInvocation::Simple(CommandId::TerminateAllDetachedSessions),
            state,
        );

        // Both detached sessions are gone from the workspace, their
        // registry senders are gone, and each received Shutdown.
        let summaries = workspace.with_untracked(|ws| ws.session_summaries());
        assert!(!summaries
            .iter()
            .any(|session| session.id == detached_terminal));
        assert!(!summaries.iter().any(|session| session.id == detached_agent));
        assert!(sessions
            .with_untracked(|registry| registry.terminal_sender(detached_terminal))
            .is_none());
        assert!(sessions
            .with_untracked(|registry| registry.agent_sender(detached_agent))
            .is_none());
        assert!(matches!(
            detached_terminal_rx.try_recv(),
            Ok(TerminalCommand::Shutdown)
        ));
        assert!(matches!(
            detached_agent_rx.try_recv(),
            Ok(Command::Shutdown)
        ));

        // The attached session survives untouched: still in the workspace
        // and attached, its registry sender still live, no Shutdown sent.
        assert!(summaries
            .iter()
            .any(|session| session.id == attached_session && session.attached));
        assert!(sessions
            .with_untracked(|registry| registry.terminal_sender(attached_session))
            .is_some());
        assert!(attached_rx.try_recv().is_err());
    }

    #[test]
    fn terminate_all_detached_sessions_is_a_no_op_with_no_detached_sessions() {
        let workspace = Workspace::mvp();
        let active_session = workspace
            .active_terminal_session_id()
            .expect("mvp() starts with an active terminal session");
        let state = test_command_action_state(workspace);

        execute_command(
            CommandInvocation::Simple(CommandId::TerminateAllDetachedSessions),
            state.clone(),
        );

        assert_eq!(state.workspace().with_untracked(|ws| ws.session_count()), 1);
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.active_terminal_session_id()),
            Some(active_session)
        );
    }
}
