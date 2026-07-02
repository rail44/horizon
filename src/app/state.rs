use std::path::PathBuf;

use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;

use crate::agent_config::AgentConfig;
use crate::app::runtime::{spawn_session, SessionRuntimeState};
use crate::control_surface::ControlMode;
use crate::session::{Frames, Registry};
use crate::workspace::{AgentDrafts, PaneFocusRequests, Workspace, MAX_VISIBLE_PANES};

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
    pub(super) terminal_dump: Option<PathBuf>,
    pub(super) clipboard_dump: Option<PathBuf>,
    pub(super) status_dump: Option<PathBuf>,
}

impl AppState {
    pub(super) fn new() -> Self {
        Self {
            workspace: RwSignal::new(Workspace::mvp()),
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
            agent_config: AgentConfig::from_env(),
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
        SessionRuntimeState {
            workspace: self.workspace,
            frames: self.frames,
            sessions: self.sessions,
            agent_state_status: self.agent_state_status,
            agent_config: self.agent_config.clone(),
            terminal_dump: self.terminal_dump.clone(),
            clipboard_dump: self.clipboard_dump.clone(),
        }
    }
}
