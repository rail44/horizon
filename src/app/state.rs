use std::path::PathBuf;

use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::agent::config::AgentConfig;
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
    pub(super) agent_config: AgentConfig,
    /// `Some` only when `[agent].agentd` is on and the startup connection
    /// succeeded (see `agent::agentd_client::connect_agentd_at_startup`) --
    /// `None` (including the default, flag-off case) means every agent
    /// session spawns fully in-process, unchanged from before step 3.
    pub(super) agentd_connection: Option<AgentdConnection>,
    pub(super) terminal_dump: Option<PathBuf>,
    pub(super) clipboard_dump: Option<PathBuf>,
    pub(super) status_dump: Option<PathBuf>,
}

impl AppState {
    pub(super) fn new() -> Self {
        // Built before the rest of the struct so `connect_agentd_at_startup`
        // (which wires its host-tool responder against this same signal)
        // can be given it -- a struct literal's fields can't reference each
        // other, and `spawn_initial_sessions` (called right after `new`
        // returns) needs `agentd_connection` already populated in case an
        // initial pane is an agent session.
        let workspace = RwSignal::new(Workspace::mvp());
        let agentd_connection = crate::agent::agentd_client::connect_agentd_at_startup(workspace);
        Self {
            workspace,
            agentd_connection,
            frames: RwSignal::new(Frames::default()),
            sessions: RwSignal::new(Registry::default()),
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
            agent_config: crate::agent::load_agent_config(),
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
            self.agent_config.clone(),
            self.terminal_dump.clone(),
            self.clipboard_dump.clone(),
            self.agentd_connection.clone(),
        )
    }
}
