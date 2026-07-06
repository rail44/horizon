mod agent;
mod terminal;

use std::path::PathBuf;

use floem::prelude::*;
use floem::reactive::create_effect;

use horizon_agent::roles::RoleId;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::session::{Frames, Registry, SessionId};
use crate::terminal::TerminalCommand;
use crate::workspace::{PaneKind, Workspace};

use agent::spawn_agent_session;
use terminal::spawn_terminal_session;

#[derive(Clone)]
pub(crate) struct SessionRuntimeState {
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    agent_state_status: RwSignal<Option<String>>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
    agentd_connection: RwSignal<Option<AgentdConnection>>,
    config_reload_requests: RwSignal<u64>,
}

impl SessionRuntimeState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        workspace: RwSignal<Workspace>,
        frames: RwSignal<Frames>,
        sessions: RwSignal<Registry>,
        agent_state_status: RwSignal<Option<String>>,
        terminal_dump: Option<PathBuf>,
        clipboard_dump: Option<PathBuf>,
        agentd_connection: RwSignal<Option<AgentdConnection>>,
        config_reload_requests: RwSignal<u64>,
    ) -> Self {
        Self {
            workspace,
            frames,
            sessions,
            agent_state_status,
            terminal_dump,
            clipboard_dump,
            agentd_connection,
            config_reload_requests,
        }
    }

    pub(crate) fn workspace(&self) -> RwSignal<Workspace> {
        self.workspace
    }

    pub(crate) fn frames(&self) -> RwSignal<Frames> {
        self.frames
    }

    pub(crate) fn sessions(&self) -> RwSignal<Registry> {
        self.sessions
    }

    pub(crate) fn agent_state_status(&self) -> RwSignal<Option<String>> {
        self.agent_state_status
    }

    pub(crate) fn agentd_connection(&self) -> RwSignal<Option<AgentdConnection>> {
        self.agentd_connection
    }

    /// Bumped by `agent::agentd_runtime`'s `config.write` observation
    /// (`fold_agent_session_events`); the app view answers a bump by
    /// executing the `Reload Config` command -- see `app::view`.
    pub(crate) fn config_reload_requests(&self) -> RwSignal<u64> {
        self.config_reload_requests
    }
}

/// Flushes any runtime state that buffers writes in memory before the app
/// exits normally. A no-op since step 4: Horizon no longer owns any
/// buffered writer itself -- the agent event log moved entirely into
/// `horizon-agentd` in step 3, and step 4 retired the in-process agent
/// runtime that used to open a copy of it here. Kept (rather than removed)
/// so `app::shutdown`/`main.rs`'s `AppEvent::WillTerminate` wiring doesn't
/// need to change; terminal sessions have no buffered-write concern of
/// their own either.
pub(crate) fn shutdown() {}

/// `role_id` selects a role-tagged agent session (`horizon_agent::roles`);
/// it only means something for `PaneKind::Agent` -- a terminal spawn
/// ignores it, and every caller but the `New Configuration Agent` command
/// passes `None` today.
pub(crate) fn spawn_session(
    kind: PaneKind,
    role_id: Option<RoleId>,
    session_id: SessionId,
    state: &SessionRuntimeState,
) {
    match kind {
        PaneKind::Terminal => spawn_terminal_session(
            session_id,
            state.frames,
            state.sessions,
            state.terminal_dump.clone(),
            state.clipboard_dump.clone(),
        ),
        PaneKind::Agent => spawn_agent_session(
            session_id,
            role_id,
            state.frames,
            state.sessions,
            state.agent_state_status,
            state.agentd_connection,
            state.config_reload_requests,
        ),
    }
}

/// Wires Horizon's own focus signals onto the currently-active terminal
/// session as `TerminalCommand::Focus` (`docs/tasks/backlog.md` item 5) --
/// the actual gate on whether that ever becomes a `CSI I`/`CSI O` byte on
/// the wire lives in `TerminalCore::focus_input` (only an app that
/// negotiated mode 1004 sees anything). Called once at startup
/// (`AppState::new`); the effect re-runs on every change to `window_focused`
/// or to the workspace (any mutation re-reads `active_visible_index`/
/// `visible_terminal_session_id`, matching how every other `workspace.with`
/// derived value in this codebase reacts), always diffing against the
/// previously notified session so a transition sends focus-out to the
/// session that just lost it and focus-in to the one that gained it --
/// never both to the same one, and never anything at all for an unchanged
/// pair.
///
/// Window focus composes with pane focus the way kitty/ghostty do: losing
/// OS-level window focus reports focus-out for whichever terminal is
/// active even though nothing pane-internal changed, and regaining window
/// focus reports focus-in again for whichever terminal is still active.
/// `window_focused` is the one piece of that composition owned here --
/// `app::input::AppInput::handle_window_focus`/`handle_window_lost_focus`
/// set it from floem's `WindowGotFocus`/`WindowLostFocus` events.
pub(crate) fn wire_focus_reporting(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    window_focused: RwSignal<bool>,
) {
    create_effect(move |previous: Option<Option<SessionId>>| {
        let focused_session = if window_focused.get() {
            workspace.with(|ws| ws.visible_terminal_session_id(ws.active_visible_index()))
        } else {
            None
        };

        if let Some(previous) = previous {
            if previous != focused_session {
                if let Some(session_id) = previous {
                    send_focus(sessions, session_id, false);
                }
                if let Some(session_id) = focused_session {
                    send_focus(sessions, session_id, true);
                }
            }
        }

        focused_session
    });
}

fn send_focus(sessions: RwSignal<Registry>, session_id: SessionId, focused: bool) {
    if let Some(tx) = sessions.with_untracked(|registry| registry.terminal_sender(session_id)) {
        let _ = tx.send(TerminalCommand::Focus(focused));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register_terminal(
        sessions: RwSignal<Registry>,
        session_id: SessionId,
    ) -> crossbeam_channel::Receiver<TerminalCommand> {
        let (tx, rx) = crossbeam_channel::unbounded();
        sessions.update(|registry| registry.insert_terminal(session_id, tx));
        rx
    }

    fn expect_focus(rx: &crossbeam_channel::Receiver<TerminalCommand>) -> bool {
        match rx.try_recv() {
            Ok(TerminalCommand::Focus(focused)) => focused,
            other => panic!("expected a Focus command, got {other:?}"),
        }
    }

    #[test]
    fn switching_the_active_pane_sends_focus_out_then_focus_in() {
        let workspace = RwSignal::new(Workspace::mvp());
        let sessions = RwSignal::new(Registry::default());
        let window_focused = RwSignal::new(true);

        let first_session = workspace
            .with_untracked(|ws| ws.visible_terminal_session_id(0))
            .expect("mvp() starts with a terminal in pane 0");
        let first_rx = register_terminal(sessions, first_session);

        let mut split = None;
        workspace.update(|ws| split = ws.split_active_with_new_session());
        let (_, second_session) = split.expect("terminal split");
        let second_rx = register_terminal(sessions, second_session);

        wire_focus_reporting(workspace, sessions, window_focused);

        // The split above already made pane 1 (the new terminal) active,
        // so creating the effect must not itself fire anything (no
        // "previous" state exists on the very first run) -- nothing to
        // assert on `first_rx` besides "no message queued for it yet".
        assert!(first_rx.try_recv().is_err());
        assert!(second_rx.try_recv().is_err());

        // Move focus back to the first pane: the second (now inactive)
        // session gets focus-out, the first gets focus-in.
        workspace.update(|ws| {
            ws.activate_visible_pane(0);
        });

        assert!(!expect_focus(&second_rx));
        assert!(expect_focus(&first_rx));
    }

    #[test]
    fn window_losing_focus_reports_focus_out_even_though_the_pane_did_not_change() {
        let workspace = RwSignal::new(Workspace::mvp());
        let sessions = RwSignal::new(Registry::default());
        let window_focused = RwSignal::new(true);

        let session_id = workspace
            .with_untracked(|ws| ws.visible_terminal_session_id(0))
            .expect("mvp() starts with a terminal in pane 0");
        let rx = register_terminal(sessions, session_id);

        wire_focus_reporting(workspace, sessions, window_focused);
        assert!(rx.try_recv().is_err(), "the initial run must send nothing");

        window_focused.set(false);
        assert!(
            !expect_focus(&rx),
            "losing window focus must report focus-out for the still-active pane"
        );

        window_focused.set(true);
        assert!(
            expect_focus(&rx),
            "regaining window focus must report focus-in again for the same pane"
        );
    }
}
