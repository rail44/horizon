use std::path::PathBuf;

use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::control_surface::ControlMode;
use crate::session::{Frames, Registry};
use crate::workspace::{AgentDrafts, PaneFocusRequests, Workspace, MAX_VISIBLE_PANES};

use super::runtime::{spawn_session, SessionRuntimeState};

#[derive(Clone)]
pub(super) struct AppState {
    pub(super) workspace: RwSignal<Workspace>,
    pub(super) frames: RwSignal<Frames>,
    pub(super) sessions: RwSignal<Registry>,
    pub(super) ime_composing: RwSignal<bool>,
    pub(super) ime_preedit: RwSignal<Option<String>>,
    pub(super) ime_cursor_area: RwSignal<(Point, Size)>,
    pub(super) palette_open: RwSignal<bool>,
    pub(super) palette_query: RwSignal<String>,
    pub(super) palette_selection: RwSignal<usize>,
    pub(super) palette_focus_request: RwSignal<u64>,
    pub(super) pane_focus_requests: PaneFocusRequests,
    pub(super) agent_drafts: AgentDrafts,
    pub(super) control_mode: RwSignal<ControlMode>,
    pub(super) overview_selection: RwSignal<usize>,
    pub(super) agent_state_status: RwSignal<Option<String>>,
    /// `Some` once Horizon has a live connection to `horizon-agentd` -- the
    /// *only* place agent sessions run (step 4 retired the in-process
    /// fallback; see `docs/agent-runtime-split-design.md`). `None` at
    /// startup means the initial connect failed (a status message is
    /// already latched into `agent_state_status` when that happens); a
    /// `Reload Agent Runtime` command (`agent::agentd_runtime::
    /// reload_agent_runtime`) is what can set this back to `Some` again,
    /// which is why it's a signal rather than a plain field.
    pub(super) agentd_connection: RwSignal<Option<AgentdConnection>>,
    pub(super) terminal_dump: Option<PathBuf>,
    pub(super) clipboard_dump: Option<PathBuf>,
    pub(super) status_dump: Option<PathBuf>,
}

impl AppState {
    pub(super) fn new() -> Self {
        let workspace = RwSignal::new(Workspace::mvp());
        let frames = RwSignal::new(Frames::default());
        let sessions = RwSignal::new(Registry::default());
        let agent_state_status = RwSignal::new(None::<String>);

        // Step 4's "on connect: hello -> session_list -> session_load for
        // every session" -- a fresh Horizon process has no panes yet, so
        // every session agentd already hosts (resumed from its own log,
        // possibly from a previous Horizon run entirely) surfaces as a
        // detached session. A failed connect leaves `agentd_connection`
        // `None` and latches an actionable status instead of silently
        // limping along -- there is no in-process fallback left to fall
        // back to.
        let agentd_connection = match crate::agent::agentd_client::connect_agentd_at_startup(
            workspace,
            agent_state_status,
        ) {
            Ok(connection) => {
                crate::agent::agentd_runtime::reconnect_all_sessions(
                    &connection,
                    workspace,
                    frames,
                    sessions,
                );
                Some(connection)
            }
            Err(error) => {
                eprintln!("horizon: could not connect to horizon-agentd ({error})");
                agent_state_status.set(Some(format!(
                    "Agent runtime unavailable ({error}) -- use \"Reload Agent Runtime\" to retry"
                )));
                None
            }
        };

        Self {
            workspace,
            frames,
            sessions,
            agentd_connection: RwSignal::new(agentd_connection),
            ime_composing: RwSignal::new(false),
            ime_preedit: RwSignal::new(None::<String>),
            ime_cursor_area: RwSignal::new((Point::new(12.0, 64.0), Size::new(8.0, 18.0))),
            palette_open: RwSignal::new(false),
            palette_query: RwSignal::new(String::new()),
            palette_selection: RwSignal::new(0_usize),
            palette_focus_request: RwSignal::new(0_u64),
            pane_focus_requests: [(); MAX_VISIBLE_PANES].map(|_| RwSignal::new(0_u64)),
            agent_drafts: [(); MAX_VISIBLE_PANES].map(|_| RwSignal::new(String::new())),
            control_mode: RwSignal::new(ControlMode::Commands),
            overview_selection: RwSignal::new(0_usize),
            agent_state_status,
            terminal_dump: std::env::var_os("HORIZON_TERMINAL_DUMP").map(PathBuf::from),
            clipboard_dump: std::env::var_os("HORIZON_CLIPBOARD_DUMP").map(PathBuf::from),
            status_dump: std::env::var_os("HORIZON_STATUS_DUMP").map(PathBuf::from),
        }
    }

    pub(super) fn spawn_initial_sessions(&self) {
        let runtime = self.session_runtime_state();
        for session in self.workspace.with(|ws| ws.session_summaries()) {
            if session.attached {
                spawn_session(session.kind.into(), session.id, &runtime);
            }
        }
    }

    pub(super) fn session_runtime_state(&self) -> SessionRuntimeState {
        SessionRuntimeState::new(
            self.workspace,
            self.frames,
            self.sessions,
            self.agent_state_status,
            self.terminal_dump.clone(),
            self.clipboard_dump.clone(),
            self.agentd_connection,
        )
    }

    /// A state with no live `horizon-agentd` connection and no spawned
    /// sessions -- for tests that only need the signals `AppInput` reads
    /// and writes (`workspace`, `ime_composing`, `ime_preedit`, ...)
    /// without `new()`'s real connect attempt. Mirrors
    /// `AgentdConnection::for_test`'s rationale.
    #[cfg(test)]
    pub(super) fn for_test() -> Self {
        Self {
            workspace: RwSignal::new(Workspace::mvp()),
            frames: RwSignal::new(Frames::default()),
            sessions: RwSignal::new(Registry::default()),
            agentd_connection: RwSignal::new(None),
            ime_composing: RwSignal::new(false),
            ime_preedit: RwSignal::new(None::<String>),
            ime_cursor_area: RwSignal::new((Point::new(12.0, 64.0), Size::new(8.0, 18.0))),
            palette_open: RwSignal::new(false),
            palette_query: RwSignal::new(String::new()),
            palette_selection: RwSignal::new(0_usize),
            palette_focus_request: RwSignal::new(0_u64),
            pane_focus_requests: [(); MAX_VISIBLE_PANES].map(|_| RwSignal::new(0_u64)),
            agent_drafts: [(); MAX_VISIBLE_PANES].map(|_| RwSignal::new(String::new())),
            control_mode: RwSignal::new(ControlMode::Commands),
            overview_selection: RwSignal::new(0_usize),
            agent_state_status: RwSignal::new(None::<String>),
            terminal_dump: None,
            clipboard_dump: None,
            status_dump: None,
        }
    }
}
