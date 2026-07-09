use floem::prelude::*;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::agent::contract::{Command, ToolCallId};
use crate::agent::tools::{
    resolve_approval, unregister_session_runtime, ApprovalDecision, ApprovalOutcome,
};
use crate::app::commands::{command_enabled, CommandId};
use crate::control_surface::{
    command_state, open_session_manager, open_view_chooser, OpenPaletteState, Placement,
    SessionManagerHandle,
};
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{
    request_active_pane_focus, Direction, PaneFocusRequests, PaneId, PaneKind, SessionKind,
    SplitAxis, Workspace,
};

use super::runtime::{resolve_new_session_cwd, spawn_session, SessionRuntimeState};

/// Reason recorded for a tool-call denial that doesn't carry an explicit
/// user-supplied reason — shared by the palette's auto-resolved
/// `DenyToolCall` and pane.rs's deny button so the literal isn't duplicated.
pub(crate) const DEFAULT_DENY_REASON: &str = "Denied by user";

#[derive(Clone)]
pub(crate) struct CommandActionState {
    pub(crate) runtime: SessionRuntimeState,
    pub(crate) pane_focus_requests: PaneFocusRequests,
    /// The session manager modal's own signals -- carried here (rather than
    /// threaded to call sites the way `OpenPaletteState` is) so
    /// `CommandId::OpenSessionManager` opens the modal identically whether
    /// it's invoked from the palette or a `[keybindings]` chord; see
    /// `control_surface::SessionManagerHandle`'s doc comment.
    pub(crate) session_manager: SessionManagerHandle,
    /// The command palette's own signals -- carried here (mirroring
    /// `session_manager` above) so `CommandId::SplitRight`/`CommandId::
    /// SplitDown`/`CommandId::NewTab` can open the second-stage view chooser
    /// (`docs/roadmap.md`'s "Placement-first session creation") identically
    /// whether invoked from a palette row or a `[keybindings]` chord -- see
    /// `control_surface::open_view_chooser`.
    pub(crate) palette: OpenPaletteState,
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

    /// A minimal `CommandActionState` wrapping `workspace`, with empty
    /// frames/sessions and no live `horizon-agentd` connection -- exposed
    /// crate-wide (unlike this module's own private `test_command_action_
    /// state`) so other modules' tests (e.g. `control_surface::actions`'s
    /// `execute_palette_selection` coverage) can build one without reaching
    /// into `app::runtime`, which is private outside `app`.
    #[cfg(test)]
    pub(crate) fn for_test(workspace: Workspace) -> Self {
        let runtime = SessionRuntimeState::new(
            RwSignal::new(workspace),
            RwSignal::new(Frames::default()),
            RwSignal::new(Registry::default()),
            RwSignal::new(None),
            None,
            None,
            RwSignal::new(None),
            RwSignal::new(0_u64),
        );
        Self {
            runtime,
            pane_focus_requests: PaneFocusRequests::new(),
            session_manager: SessionManagerHandle::for_test(),
            palette: OpenPaletteState::for_test(),
        }
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
/// `CloseTab`/`ActivateTab`/`ActivatePane` target a visible index rather
/// than a stable id: the `Workspace` methods backing them
/// (`close_tab_index`, `activate_tab_index`, `activate_pane_index`) already
/// take one, and workspace mode's own cursor is itself a flat visible
/// index (`docs/recursive-layout-design.md`'s slice 3 is what would move
/// that to real geometry), so there's no stable id to prefer at those call
/// sites yet. `ClosePane` targets a `PaneId` instead: `workspace::view::
/// pane`'s recursive renderer (slice 2 of the same doc) builds each pane's
/// view directly off its `PaneId`, so its close button already has one to
/// hand. `AttachSession`/`TerminateSession` target a `SessionId`, since
/// that's stable across attach/detach and is what the workspace already
/// keys sessions by.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommandInvocation {
    Simple(CommandId),
    /// Creates a new `kind` session -- the one operation every session-
    /// creating surface converges on (`docs/roadmap.md`'s "Placement-first
    /// session creation"): a control-plane `new-terminal`/`new-agent`
    /// invocation (`docs/cli-control-plane-design.md`'s "Placement
    /// vocabulary" and "activate rides on creating/attaching operations"
    /// decisions), and the human palette/keybinding path via `CommandId::
    /// SplitRight`/`CommandId::SplitDown`/`CommandId::NewTab`'s second-stage
    /// view chooser (`control_surface::actions::execute_palette_selection`'s
    /// `PaletteRow::Chooser` branch), which always sets `activate: true`
    /// (a human creation always dives). `split_target`, when set, bundles
    /// the target session with the axis to split on
    /// (`docs/recursive-layout-design.md`'s slice 3 -- `SplitRight`/
    /// `SplitDown` resolve to `Horizontal`/`Vertical`, the control plane
    /// always resolves to `Horizontal` since it has no vertical vocabulary
    /// yet) and places the new pane as a split next to the pane currently
    /// hosting that session (in whichever tab that is, not necessarily the
    /// active one) instead of opening a new tab; `activate` controls whether
    /// focus/the active tab and pane follow the new session at all (the
    /// control plane defaults this to `false`, the CLI's `--active` opts in
    /// -- see `app::external_commands::invocation_from_external`). `prompt`,
    /// if set, is the composite create-with-prompt decision's payload: sent
    /// as the new session's first `Command::UserMessage` once spawned (see
    /// [`execute_command`]'s handling of this variant and
    /// [`send_initial_user_message`] for why
    /// there is no readiness wait between the two steps) -- this subsumes
    /// the older, narrower `NewAgentWithPrompt` variant, which had no room
    /// for `split_target`/`activate`.
    CreateSession {
        kind: PaneKind,
        /// Role tag for an agent session (`horizon_agent::roles`) -- the
        /// control plane's `new-config-agent` sets this; `new-terminal`/
        /// `new-agent` leave it `None`. Meaningless (and always `None`) for
        /// a terminal `kind`.
        role_id: Option<horizon_agent::roles::RoleId>,
        split_target: Option<(SessionId, SplitAxis)>,
        activate: bool,
        prompt: Option<String>,
    },
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
    /// Close a specific pane (pane header's × button), whether or not it's
    /// the active pane. Targets a stable `PaneId` (unlike `CloseTab`/
    /// `ActivateTab`/`ActivatePane` below, which still target a visible
    /// index) -- `workspace::view::pane`'s recursive renderer builds each
    /// pane's view directly off its `PaneId`
    /// (`docs/recursive-layout-design.md`'s slice 2), so that's what its
    /// close button already has to hand.
    ClosePane {
        pane_id: PaneId,
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
    /// or overview detached-session row, or the control plane's `attach`
    /// external command). `activate` is always `true` from human surfaces
    /// (attaching dives, per `docs/workspace-mode-design.md`'s Amended
    /// second-round decision 1: "`AttachSession` joins this bucket"); the
    /// control plane defaults it to `false`, with `--active` opting in --
    /// same rationale as `CreateSession::activate`.
    AttachSession {
        session_id: SessionId,
        activate: bool,
    },
    /// Terminate a session by id, whether or not it's the active session or
    /// attached to any pane — reuses the same registry/frame cleanup as
    /// `Simple(CommandId::TerminateActiveSession)`.
    TerminateSession {
        session_id: SessionId,
    },
    /// Enters workspace mode (`docs/workspace-mode-design.md`) — the
    /// terminal/agent-pane entry chord (`app::keymap::
    /// is_workspace_mode_enter_key`) and an agent pane's bare-`Esc`
    /// fallback (`workspace::agent_escape_requests_workspace_mode`) both
    /// dispatch here. No catalog `CommandId`/palette row: entering the mode
    /// isn't something worth listing as a searchable operation, matching
    /// the precedent the other targeted variants above already set.
    EnterWorkspaceMode,
    /// Moves the workspace-mode cursor one step — the interpreted result of
    /// an `hjkl` key while the mode is active (see `workspace::mode_input`).
    MoveWorkspaceCursor {
        direction: Direction,
    },
    /// `Enter` while in workspace mode: focus follows the cursor, then the
    /// mode ends.
    CommitWorkspaceMode,
    /// `Esc` while in workspace mode: cancels back to the untouched focus.
    CancelWorkspaceMode,
}

pub(crate) fn execute_command(invocation: CommandInvocation, state: CommandActionState) {
    match invocation {
        CommandInvocation::Simple(command_id) => execute_simple_command(command_id, state),
        CommandInvocation::CreateSession {
            kind,
            role_id,
            split_target,
            activate,
            prompt,
        } => {
            let sessions = state.sessions();
            if let Some(session_id) = create_session(state, kind, role_id, split_target, activate) {
                if let Some(prompt) = prompt {
                    send_initial_user_message(sessions, session_id, prompt);
                }
            }
        }
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
        CommandInvocation::ClosePane { pane_id } => {
            state.workspace().update(|ws| {
                ws.close_pane(pane_id);
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
            request_active_pane_focus(workspace, state.pane_focus_requests.clone());
        }
        CommandInvocation::ActivatePane {
            tab_index,
            pane_index,
        } => {
            let workspace = state.workspace();
            workspace.update(|ws| {
                ws.activate_pane_index(tab_index, pane_index);
            });
            request_active_pane_focus(workspace, state.pane_focus_requests.clone());
        }
        CommandInvocation::AttachSession {
            session_id,
            activate,
        } => {
            let workspace = state.workspace();
            workspace.update(|ws| {
                ws.attach_existing_session_to_split_activated(session_id, activate);
            });
            if activate {
                request_active_pane_focus(workspace, state.pane_focus_requests.clone());
            }
        }
        CommandInvocation::TerminateSession { session_id } => {
            terminate_session_by_id(
                state.workspace(),
                state.frames(),
                state.sessions(),
                session_id,
            );
        }
        CommandInvocation::EnterWorkspaceMode => {
            state.workspace().update(Workspace::enter_workspace_mode);
        }
        CommandInvocation::MoveWorkspaceCursor { direction } => {
            state.workspace().update(|ws| ws.move_cursor(direction));
        }
        CommandInvocation::CommitWorkspaceMode => {
            let workspace = state.workspace();
            workspace.update(Workspace::commit_workspace_mode);
            request_active_pane_focus(workspace, state.pane_focus_requests.clone());
        }
        CommandInvocation::CancelWorkspaceMode => {
            state.workspace().update(Workspace::cancel_workspace_mode);
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
        CommandId::SplitRight => {
            open_view_chooser(state.palette, Placement::Split(SplitAxis::Horizontal));
        }
        CommandId::SplitDown => {
            open_view_chooser(state.palette, Placement::Split(SplitAxis::Vertical));
        }
        CommandId::NewTab => {
            open_view_chooser(state.palette, Placement::NewTab);
        }
        CommandId::FocusNextPane => {
            workspace.update(Workspace::focus_next);
            request_active_pane_focus(workspace, state.pane_focus_requests.clone());
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
        CommandId::OpenSessionManager => open_session_manager(state.session_manager),
        CommandId::ReloadConfig => reload_config(state),
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
        state.runtime.config_reload_requests(),
    );
}

/// The one place the GUI names the `config` role
/// (`horizon_agent::roles::CONFIG_ROLE`) -- `New Configuration Agent`'s
/// palette arm and the control plane's `new-config-agent` both spawn
/// through it, so the role id literal isn't scattered.
pub(crate) fn config_agent_role_id() -> horizon_agent::roles::RoleId {
    horizon_agent::roles::RoleId(horizon_agent::roles::CONFIG_ROLE.id.to_string())
}

/// `Reload Config` (`CommandId::ReloadConfig`): re-reads Horizon's single
/// config file fresh from disk (`crate::config::reload`, bypassing
/// `config::load`'s startup-only cache) and applies its `[theme]` (chrome
/// roles, `[theme.ansi]`, and the terminal colors both derive) and
/// `[keybindings]` live -- see `docs/plans/agent-foundation/
/// 03-roles-and-config-agent.md`'s "前提機能: config の実行時リロード".
///
/// Every other config section (`[agent]`/`[provider]`/`[terminal]`/`[ui]`)
/// is deliberately NOT reapplied here: those are read once, at session-spawn
/// or app-startup time, into small owned structs with no swappable process
/// state behind them (`agent::config`, `terminal::config::TerminalConfig`,
/// `ui::fonts`, ...) -- making *those* live too is future work, out of scope
/// for this command, so the surfaced status says so explicitly rather than
/// implying a full reload happened.
///
/// A parse or read failure leaves the currently applied theme/keymap
/// untouched -- never resets to defaults over a typo -- and surfaces the
/// error instead; see `config::reload_from_path`'s doc comment for why a
/// missing file is a different (successful) case from a malformed one.
fn reload_config(state: CommandActionState) {
    match crate::config::reload() {
        Ok(config) => {
            crate::ui::theme::apply_reload(&config.theme);
            crate::app::keymap::Keymap::reload(&config.keybindings);
            state.agent_state_status().set(Some(
                "Config reloaded -- theme & keybindings applied (other sections apply at \
                 restart / to new sessions)"
                    .to_string(),
            ));
        }
        Err(error) => {
            state.agent_state_status().set(Some(format!(
                "Config reload failed: {error} -- keeping current theme/keybindings"
            )));
        }
    }
}

/// `CommandInvocation::CreateSession`'s worker -- see that variant's doc
/// comment. Spawns the session and requests focus, placement- and
/// activation-aware: `None` `split_target` opens a new tab; `Some((target,
/// axis))` places the new pane as a split, on `axis`, next to that session's
/// pane in whichever tab currently hosts it (any tab, not just the active
/// one). `activate: false` skips both the workspace-mode exit and the
/// focus-follow request, so a control-
/// plane-driven creation never disturbs the owner's focus or an in-progress
/// workspace-mode session -- see `Workspace::exit_workspace_mode`'s doc
/// comment for why that call is otherwise tied to "creating operations
/// dive". Returns `None` (spawning nothing) only when `split_target` fails
/// to resolve to any pane -- already rejected earlier by
/// `external_commands::dispatch_invoke`'s pre-check, so this is defense in
/// depth, not the primary error path.
///
/// The spawn-source session, for `runtime::resolve_new_session_cwd`
/// (`docs/session-relationship-design.md` decision 3, "start where I'm
/// looking"), is `split_target`'s session when explicit (a placement-first
/// split derives from the pane it splits) or the *currently* active session
/// otherwise (a plain new tab still derives from whatever pane the caller
/// was looking at) -- read before `workspace.update` below mutates which
/// pane is active.
fn create_session(
    state: CommandActionState,
    kind: PaneKind,
    role_id: Option<horizon_agent::roles::RoleId>,
    split_target: Option<(SessionId, SplitAxis)>,
    activate: bool,
) -> Option<SessionId> {
    let workspace = state.workspace();
    let source_session_id = split_target
        .map(|(target, _axis)| target)
        .or_else(|| workspace.with_untracked(|ws| ws.active_session_id()));
    let cwd = resolve_new_session_cwd(source_session_id, workspace, state.runtime.sessions());
    let mut session_id = None;
    workspace.update(|ws| {
        session_id = match split_target {
            Some((target, axis)) => ws.split_session_with_new_session(target, kind, axis, activate),
            None => Some(ws.open_tab_with_new_session_activated(kind, activate)),
        };
        if activate {
            ws.exit_workspace_mode();
        }
    });
    let session_id = session_id?;
    spawn_session(kind, role_id, session_id, cwd, &state.runtime);
    if activate {
        request_active_pane_focus(workspace, state.pane_focus_requests.clone());
    }
    Some(session_id)
}

/// Sends `text` as `session_id`'s first `Command::UserMessage` --
/// `CommandInvocation::CreateSession`'s tail half, once `create_session` has
/// already spawned the session.
///
/// Safe to call immediately, with no readiness wait: `create_session` runs
/// `spawn_session` synchronously, and for an agent session that path
/// (`app::runtime::agent::spawn_agent_session` ->
/// `agent::agentd_runtime::fold_agent_session_events`) registers the
/// session's sender into `Registry` *before* returning -- there is no window
/// where `CreateSession`'s prompt-sending tail could observe a session id
/// with no sender yet.
/// This is a different race than the one agentd's own `session_new`
/// readiness gate guards against (`crates/horizon-agentd/src/main.rs`'s
/// `run_session_hosting_loop` comment on `Control::SessionNew`) -- that one
/// is about agentd's *own* persistence-writer startup, in a different
/// process, and is already handled entirely on agentd's side. A missing
/// sender here can therefore only mean agentd itself was unavailable when
/// the session was spawned, which `spawn_agent_session`'s "agent runtime
/// unavailable" error frame already covers -- so this is a silent no-op in
/// that case, exactly like `cancel_agent_turn`/`resolve_and_send_approval`
/// already are for the same "no sender" condition.
fn send_initial_user_message(sessions: RwSignal<Registry>, session_id: SessionId, text: String) {
    if let Some(tx) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = tx.send(Command::UserMessage { text });
    }
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
    // Fully untracked (not just the outer `agent_frame_untracked`): this
    // reads once to decide what to do next, not to render, so it must not
    // leave a live subscription behind if `resolve_and_send_approval` ever
    // runs from inside an active reactive scope (e.g. the CLI control-plane
    // bridge's request-pump effect -- see `agentd_runtime::
    // fold_agent_session_events`'s doc comment for that exact hazard).
    let frame = frames.with_untracked(|frames| frames.agent_frame_untracked(session_id));
    let command = match resolve_approval(&frame, session_id.into(), call_id, decision) {
        ApprovalOutcome::Executed { frame, command, .. } => {
            // `with_untracked`, not `update` -- see `agentd_runtime::
            // fold_agent_session_events`'s matching comment.
            frames.with_untracked(|frames| frames.update_agent_frame(session_id, frame));
            command
        }
        // `bash` on approve: the running-state frame is ready to publish,
        // but the tool is executing off the UI thread and hasn't produced a
        // result yet. Nothing to send to the provider here — the eventual
        // `Command::ToolCallResult` is sent later by the effect
        // `app/runtime/agent.rs::spawn_agent_session` sets up for it.
        ApprovalOutcome::Started { frame, .. } => {
            frames.with_untracked(|frames| frames.update_agent_frame(session_id, frame));
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
///
/// Deliberately reads through `Frames::agent_frame` (tracked, via each
/// visited session's own handle), not `agent_frame_untracked`, even though
/// the palette's caller is the only one of the two that actually needs
/// live reactivity here: the palette's `command_state` check must
/// re-derive when a pending approval appears/resolves, and this function is
/// shared with the one-shot command-dispatch caller above, which has no
/// live view to gate on anyway. A one-shot caller invoked from inside some
/// unrelated active effect could in principle pick up a spurious
/// subscription this way; splitting the two call sites onto separate
/// tracked/untracked helpers is future work if that ever proves to matter
/// in practice (see `docs/reactive-store-design.md`'s open questions).
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
        let frames = Frames::default();
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
            RwSignal::new(0_u64),
        );
        let state = CommandActionState {
            runtime,
            pane_focus_requests: PaneFocusRequests::new(),
            session_manager: SessionManagerHandle::for_test(),
            palette: OpenPaletteState::for_test(),
        };

        (state, session_id, call_id, rx)
    }

    /// A minimal `CommandActionState` wrapping the given workspace, with
    /// empty frames/sessions — for tests of the targeted-index/session
    /// invocations below that don't need a running session behind them.
    /// Thin wrapper over `CommandActionState::for_test` (kept as its own
    /// name in this file since every call site here predates that crate-
    /// wide helper).
    fn test_command_action_state(workspace: Workspace) -> CommandActionState {
        CommandActionState::for_test(workspace)
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
        sessions.insert_terminal(attached_session, attached_tx, None);
        let (detached_terminal_tx, detached_terminal_rx) = crossbeam_channel::unbounded();
        sessions.insert_terminal(detached_terminal, detached_terminal_tx, None);
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
            RwSignal::new(0_u64),
        );
        let state = CommandActionState {
            runtime,
            pane_focus_requests: PaneFocusRequests::new(),
            session_manager: SessionManagerHandle::for_test(),
            palette: OpenPaletteState::for_test(),
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
        let pane_id = workspace.split_active(PaneKind::Terminal, Some(second_session));
        let state = test_command_action_state(workspace);

        execute_command(CommandInvocation::ClosePane { pane_id }, state.clone());

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
            CommandInvocation::AttachSession {
                session_id,
                activate: true,
            },
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

    // --- CommandInvocation::CreateSession -----------------------------------

    #[test]
    fn send_initial_user_message_sends_the_prompt_to_the_sessions_channel() {
        let session_id = SessionId::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let (_events_tx, events_rx) = crossbeam_channel::unbounded();
        let mut sessions = Registry::default();
        sessions.insert_agent(session_id, SessionHandle::new(tx, events_rx));
        let sessions = RwSignal::new(sessions);

        send_initial_user_message(sessions, session_id, "hello agent".to_string());

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::UserMessage { text }) if text == "hello agent"
        ));
    }

    #[test]
    fn send_initial_user_message_is_a_silent_no_op_with_no_registered_sender() {
        let session_id = SessionId::new();
        let sessions = RwSignal::new(Registry::default());

        // Must not panic when no session is registered under this id.
        send_initial_user_message(sessions, session_id, "hello agent".to_string());
    }

    #[test]
    fn create_session_with_prompt_dives_when_activated() {
        let workspace = Workspace::mvp();
        let runtime = SessionRuntimeState::new(
            RwSignal::new(workspace),
            RwSignal::new(Frames::default()),
            RwSignal::new(Registry::default()),
            RwSignal::new(None),
            None,
            None,
            RwSignal::new(Some(AgentdConnection::for_test())),
            RwSignal::new(0_u64),
        );
        let state = CommandActionState {
            runtime,
            pane_focus_requests: PaneFocusRequests::new(),
            session_manager: SessionManagerHandle::for_test(),
            palette: OpenPaletteState::for_test(),
        };
        let workspace = state.workspace();
        let before_tab_count = workspace.with_untracked(|ws| ws.tab_count());

        execute_command(
            CommandInvocation::CreateSession {
                kind: PaneKind::Agent,
                role_id: None,
                split_target: None,
                activate: true,
                prompt: Some("hello agent".to_string()),
            },
            state.clone(),
        );

        assert_eq!(
            workspace.with_untracked(|ws| ws.tab_count()),
            before_tab_count + 1
        );
        let new_session_id = workspace
            .with_untracked(|ws| ws.active_session_id())
            .expect("the new agent tab should be active when activate is true");
        assert!(state
            .sessions()
            .with_untracked(|registry| registry.agent_sender(new_session_id))
            .is_some());
    }

    #[test]
    fn create_session_without_activate_creates_the_tab_but_does_not_switch_to_it() {
        let state = test_command_action_state(Workspace::mvp());
        let workspace = state.workspace();
        let before_active_tab = workspace.with_untracked(|ws| ws.active_tab_index());
        let before_tab_count = workspace.with_untracked(|ws| ws.tab_count());
        // `pane_focus_requests` entries are created lazily, normally by
        // `pane_view` mounting -- pre-register the active pane's own entry
        // here (standing in for a real mount) so the "must not bump" check
        // below actually observes something rather than comparing two
        // empty maps.
        let active_pane_id = workspace
            .with_untracked(|ws| ws.visible_pane_id(ws.active_visible_index()))
            .expect("mvp() has an active pane");
        let focus_request = state.pane_focus_requests.register(active_pane_id);
        let before_focus_request = focus_request.with_untracked(|count| *count);

        execute_command(
            CommandInvocation::CreateSession {
                kind: PaneKind::Terminal,
                role_id: None,
                split_target: None,
                activate: false,
                prompt: None,
            },
            state.clone(),
        );

        assert_eq!(
            workspace.with_untracked(|ws| ws.tab_count()),
            before_tab_count + 1,
            "a new tab is still created even when not activated"
        );
        assert_eq!(
            workspace.with_untracked(|ws| ws.active_tab_index()),
            before_active_tab,
            "activate: false must not switch the active tab"
        );
        assert_eq!(
            focus_request.with_untracked(|count| *count),
            before_focus_request,
            "activate: false must not request pane focus"
        );
    }

    #[test]
    fn create_session_with_split_target_places_the_pane_in_the_targets_tab_without_activating() {
        let mut workspace = Workspace::mvp();
        let target_session = SessionId::new();
        // tab 0: [initial session, target_session], target_session's pane
        // left active by `split_active` -- this fixture call always dives,
        // matching every other direct `split_active` call in this test
        // module.
        workspace.split_active(PaneKind::Terminal, Some(target_session));
        // tab 1, opened and switched to -- tab 0 (and target_session) is no
        // longer the active tab.
        workspace.open_tab(PaneKind::Terminal, Some(SessionId::new()));
        let state = test_command_action_state(workspace);
        let workspace = state.workspace();
        assert_eq!(workspace.with_untracked(|ws| ws.active_tab_index()), 1);

        execute_command(
            CommandInvocation::CreateSession {
                kind: PaneKind::Terminal,
                role_id: None,
                split_target: Some((target_session, SplitAxis::Horizontal)),
                activate: false,
                prompt: None,
            },
            state.clone(),
        );

        assert_eq!(
            workspace.with_untracked(|ws| ws.tab_count()),
            2,
            "splitting next to a session must not open a new tab"
        );
        assert_eq!(
            workspace.with_untracked(|ws| ws.active_tab_index()),
            1,
            "activate: false must not switch to the split target's tab"
        );
        let panes_in_target_tab = workspace.with_untracked(|ws| {
            ws.pane_summaries()
                .into_iter()
                .filter(|pane| pane.tab_index == 0)
                .collect::<Vec<_>>()
        });
        assert_eq!(panes_in_target_tab.len(), 3);
        assert!(
            panes_in_target_tab.iter().all(|pane| !pane.tab_active),
            "tab 0 must not report itself as the active tab"
        );
        assert!(
            panes_in_target_tab[1].active,
            "the pane hosting the split target must remain that tab's own active pane"
        );
        assert!(
            !panes_in_target_tab[2].active,
            "the newly created pane must not become active when activate is false"
        );
    }

    #[test]
    fn create_session_with_split_target_and_activate_switches_to_the_targets_tab_and_pane() {
        let mut workspace = Workspace::mvp();
        let target_session = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(target_session));
        workspace.open_tab(PaneKind::Terminal, Some(SessionId::new()));
        let state = test_command_action_state(workspace);
        let workspace = state.workspace();
        assert_eq!(workspace.with_untracked(|ws| ws.active_tab_index()), 1);

        execute_command(
            CommandInvocation::CreateSession {
                kind: PaneKind::Terminal,
                role_id: None,
                split_target: Some((target_session, SplitAxis::Horizontal)),
                activate: true,
                prompt: None,
            },
            state.clone(),
        );

        assert_eq!(
            workspace.with_untracked(|ws| ws.active_tab_index()),
            0,
            "activate: true must switch to the split target's tab"
        );
        let new_active_session = workspace
            .with_untracked(|ws| ws.active_session_id())
            .expect("the new pane should be active");
        assert_ne!(new_active_session, target_session);
    }

    #[test]
    fn create_session_with_an_unresolvable_split_target_spawns_nothing() {
        let state = test_command_action_state(Workspace::mvp());
        let workspace = state.workspace();
        let before_tab_count = workspace.with_untracked(|ws| ws.tab_count());
        let before_session_count = workspace.with_untracked(|ws| ws.session_count());

        execute_command(
            CommandInvocation::CreateSession {
                kind: PaneKind::Terminal,
                role_id: None,
                split_target: Some((SessionId::new(), SplitAxis::Horizontal)),
                activate: false,
                prompt: None,
            },
            state.clone(),
        );

        assert_eq!(
            workspace.with_untracked(|ws| ws.tab_count()),
            before_tab_count
        );
        assert_eq!(
            workspace.with_untracked(|ws| ws.session_count()),
            before_session_count
        );
    }

    // --- CommandId::SplitRight / CommandId::SplitDown / CommandId::NewTab:
    // open the view chooser ------------------------------------------------
    //
    // `docs/roadmap.md`'s "Placement-first session creation": these three
    // catalog commands never create a session directly -- they open the
    // palette's second-stage view chooser (`control_surface::
    // open_view_chooser`), tagged with which placement a chosen view will
    // use. Dispatched via `Simple`, exactly like a `[keybindings]` chord
    // would (`app::input::AppInput`'s fallback), so this also proves the
    // bound-key path opens the chooser directly -- no palette row needed
    // first.

    #[test]
    fn simple_split_right_opens_the_view_chooser_directly() {
        let state = test_command_action_state(Workspace::mvp());

        execute_command(
            CommandInvocation::Simple(CommandId::SplitRight),
            state.clone(),
        );

        assert!(state.palette.palette_open.get_untracked());
        assert_eq!(
            state.palette.palette_stage.get_untracked(),
            crate::control_surface::PaletteStage::ViewChooser {
                placement: crate::control_surface::Placement::Split(SplitAxis::Horizontal)
            }
        );
        assert_eq!(state.palette.palette_query.get_untracked(), "");
    }

    #[test]
    fn simple_split_down_opens_the_view_chooser_directly() {
        let state = test_command_action_state(Workspace::mvp());

        execute_command(
            CommandInvocation::Simple(CommandId::SplitDown),
            state.clone(),
        );

        assert!(state.palette.palette_open.get_untracked());
        assert_eq!(
            state.palette.palette_stage.get_untracked(),
            crate::control_surface::PaletteStage::ViewChooser {
                placement: crate::control_surface::Placement::Split(SplitAxis::Vertical)
            }
        );
        assert_eq!(state.palette.palette_query.get_untracked(), "");
    }

    #[test]
    fn simple_new_tab_opens_the_view_chooser_directly() {
        let state = test_command_action_state(Workspace::mvp());

        execute_command(CommandInvocation::Simple(CommandId::NewTab), state.clone());

        assert!(state.palette.palette_open.get_untracked());
        assert_eq!(
            state.palette.palette_stage.get_untracked(),
            crate::control_surface::PaletteStage::ViewChooser {
                placement: crate::control_surface::Placement::NewTab
            }
        );
    }

    // --- workspace-mode invocations ---------------------------------------

    #[test]
    fn enter_workspace_mode_invocation_activates_the_mode() {
        let state = test_command_action_state(Workspace::mvp());

        execute_command(CommandInvocation::EnterWorkspaceMode, state.clone());

        assert!(state
            .workspace()
            .with_untracked(|ws| ws.is_workspace_mode_active()));
    }

    #[test]
    fn move_workspace_cursor_invocation_moves_the_cursor_without_moving_focus() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        // `split_active` itself focuses the new (second) pane -- reset to
        // the first so this test starts from a known focus position.
        workspace.activate_visible_pane(0);
        let second_pane_id = workspace
            .visible_pane_id(1)
            .expect("split_active left a second visible pane");
        let state = test_command_action_state(workspace);
        execute_command(CommandInvocation::EnterWorkspaceMode, state.clone());

        execute_command(
            CommandInvocation::MoveWorkspaceCursor {
                direction: Direction::Right,
            },
            state.clone(),
        );

        assert_eq!(
            state.workspace().with_untracked(|ws| ws.cursor_pane_id()),
            Some(second_pane_id)
        );
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.active_visible_index()),
            0,
            "focus must not follow the cursor until a commit"
        );
    }

    #[test]
    fn commit_workspace_mode_invocation_moves_focus_to_the_cursor_and_refocuses_the_pane() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        let second_pane_id = workspace
            .visible_pane_id(1)
            .expect("split_active left a second visible pane");
        let state = test_command_action_state(workspace);
        // Pre-register the second pane's focus-request entry -- normally
        // created lazily by `pane_view` mounting -- so the "commit bumps
        // it" check below observes a real signal rather than `None`.
        let focus_request = state.pane_focus_requests.register(second_pane_id);
        execute_command(CommandInvocation::EnterWorkspaceMode, state.clone());
        execute_command(
            CommandInvocation::MoveWorkspaceCursor {
                direction: Direction::Right,
            },
            state.clone(),
        );
        let focus_request_before = focus_request.with_untracked(|count| *count);

        execute_command(CommandInvocation::CommitWorkspaceMode, state.clone());

        assert!(!state
            .workspace()
            .with_untracked(|ws| ws.is_workspace_mode_active()));
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.active_visible_index()),
            1
        );
        assert!(
            focus_request.with_untracked(|count| *count) > focus_request_before,
            "commit must request real focus for the pane the cursor landed on"
        );
    }

    #[test]
    fn cancel_workspace_mode_invocation_leaves_focus_untouched() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        let state = test_command_action_state(workspace);
        execute_command(CommandInvocation::EnterWorkspaceMode, state.clone());
        execute_command(
            CommandInvocation::MoveWorkspaceCursor {
                direction: Direction::Right,
            },
            state.clone(),
        );

        execute_command(CommandInvocation::CancelWorkspaceMode, state.clone());

        assert!(!state
            .workspace()
            .with_untracked(|ws| ws.is_workspace_mode_active()));
        assert_eq!(
            state
                .workspace()
                .with_untracked(|ws| ws.active_visible_index()),
            0
        );
    }

    // `create_session`'s "creating operations dive" call to
    // `Workspace::exit_workspace_mode` (see its doc comment) isn't exercised
    // here through `execute_command(CreateSession { activate: true, .. })`
    // with a real agent/terminal kind: that path spawns a real PTY/shell
    // process via `runtime::spawn_session`, which is too heavy (and
    // environment-dependent) for a unit test. `Workspace::
    // exit_workspace_mode`'s own no-op-when-inactive/clears-when-active
    // behavior is covered directly in `workspace::mode`'s tests instead.
}
