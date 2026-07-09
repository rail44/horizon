use std::path::PathBuf;

use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::control_surface::PaletteStage;
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{AgentDrafts, PaneFocusRequests, Workspace};

use super::runtime::{
    resolve_new_session_cwd, spawn_session, wire_focus_reporting, SessionRuntimeState,
};

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
    /// Which palette stage is showing -- the normal Commands catalog, or
    /// the second-stage view chooser `CommandId::SplitRight`/`CommandId::
    /// SplitDown`/`CommandId::NewTab` open (`docs/roadmap.md`'s
    /// "Placement-first session creation"). See
    /// `control_surface::{open_palette, open_view_chooser}`.
    pub(super) palette_stage: RwSignal<PaletteStage>,
    pub(super) palette_focus_request: RwSignal<u64>,
    pub(super) pane_focus_requests: PaneFocusRequests,
    /// Whether Horizon's own window currently holds OS-level focus --
    /// updated from floem's `WindowGotFocus`/`WindowLostFocus`
    /// (`app::input::AppInput::handle_window_focus`/
    /// `handle_window_lost_focus`) and composed with the active visible
    /// pane by `app::runtime::wire_focus_reporting` to decide when the
    /// active terminal session gets a `CSI I`/`CSI O` focus report
    /// (`docs/tasks/backlog.md` item 5). Starts `true`: a freshly launched
    /// window is focused by default on every desktop this targets.
    pub(super) window_focused: RwSignal<bool>,
    pub(super) agent_drafts: AgentDrafts,
    /// The session manager modal's own signals (`docs/plans/application-ui/
    /// 01-session-manager.md`) -- mirrors `palette_open`/`palette_selection`/
    /// `palette_focus_request` above, bundled into a
    /// `control_surface::SessionManagerHandle` for `CommandActionState` (see
    /// `app::context::session_manager_handle`).
    pub(super) session_manager_open: RwSignal<bool>,
    pub(super) session_manager_selection: RwSignal<usize>,
    /// The session identity `session_manager_selection` currently resolves
    /// to -- see `control_surface::SessionManagerHandle::selected_id`'s doc
    /// comment for why this exists alongside a plain index.
    pub(super) session_manager_selected_id: RwSignal<Option<SessionId>>,
    pub(super) session_manager_pending_terminate: RwSignal<Option<SessionId>>,
    pub(super) session_manager_focus_request: RwSignal<u64>,
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
    /// Bumped when a session's successful `config.write` is observed
    /// (`agent::agentd_runtime::fold_agent_session_events`); `app::view`
    /// answers a bump by executing the `Reload Config` command. Same
    /// request-counter shape as `pane_focus_requests`.
    pub(super) config_reload_requests: RwSignal<u64>,
}

impl AppState {
    pub(super) fn new() -> Self {
        let workspace = RwSignal::new(Workspace::mvp());
        let frames = RwSignal::new(Frames::default());
        let sessions = RwSignal::new(Registry::default());
        let agent_state_status = RwSignal::new(None::<String>);
        let config_reload_requests = RwSignal::new(0_u64);

        // Non-blocking startup connect (`docs/tasks/backlog.md` item 14(c)):
        // `agentd_connection` starts `None` and the window must map
        // regardless of how long -- or whether -- this ever resolves, so
        // the connect/handshake/`session_list` sequence (step 4's "on
        // connect: hello -> session_list -> session_load for every
        // session") runs entirely on a background thread; a
        // `create_effect` callback applies the outcome once it arrives.
        // See `agent::agentd_runtime::connect_agentd_at_startup_async`'s
        // own doc for why this used to block here (a busy `horizon-agentd`
        // -- it serves one connection at a time -- could stall this call
        // indefinitely, along with the window's first frame).
        let agentd_connection: RwSignal<Option<AgentdConnection>> = RwSignal::new(None);
        crate::agent::agentd_runtime::connect_agentd_at_startup_async(
            workspace,
            frames,
            sessions,
            agentd_connection,
            agent_state_status,
            config_reload_requests,
        );

        let window_focused = RwSignal::new(true);
        wire_focus_reporting(workspace, sessions, window_focused);

        let state = Self {
            workspace,
            frames,
            sessions,
            agentd_connection,
            ime_composing: RwSignal::new(false),
            ime_preedit: RwSignal::new(None::<String>),
            ime_cursor_area: RwSignal::new((Point::new(12.0, 64.0), Size::new(8.0, 18.0))),
            palette_open: RwSignal::new(false),
            palette_query: RwSignal::new(String::new()),
            palette_selection: RwSignal::new(0_usize),
            palette_stage: RwSignal::new(PaletteStage::Commands),
            palette_focus_request: RwSignal::new(0_u64),
            pane_focus_requests: PaneFocusRequests::new(),
            window_focused,
            agent_drafts: AgentDrafts::new(),
            session_manager_open: RwSignal::new(false),
            session_manager_selection: RwSignal::new(0_usize),
            session_manager_selected_id: RwSignal::new(None),
            session_manager_pending_terminate: RwSignal::new(None),
            session_manager_focus_request: RwSignal::new(0_u64),
            agent_state_status,
            terminal_dump: std::env::var_os("HORIZON_TERMINAL_DUMP").map(PathBuf::from),
            clipboard_dump: std::env::var_os("HORIZON_CLIPBOARD_DUMP").map(PathBuf::from),
            status_dump: std::env::var_os("HORIZON_STATUS_DUMP").map(PathBuf::from),
            config_reload_requests,
        };
        state.start_control_plane();
        state
    }

    /// Starts Horizon's CLI control-plane listener
    /// (`docs/cli-control-plane-design.md`) against this state's command-
    /// action surface -- the exact same `CommandActionState` the palette and
    /// keybindings already dispatch through, so an external client's
    /// `Invoke` runs through the identical `execute_command` path ("the
    /// command model is the core; surfaces are replaceable"). Called once,
    /// at the tail of `new()` -- deliberately before `app_view`'s
    /// `spawn_initial_sessions()` runs, so the listener (and the
    /// `HORIZON_SOCKET` value every terminal/agentd spawn injects) is
    /// already up before the first child process that might read it exists.
    /// Not run by [`Self::for_test`]: unit tests never need a real bound
    /// socket.
    fn start_control_plane(&self) {
        crate::control_plane::start(super::context::command_action_state(self));
    }

    pub(super) fn spawn_initial_sessions(&self) {
        let runtime = self.session_runtime_state();
        // No spawn-source pane exists yet at startup, so every initial
        // session falls back to Horizon's own launch cwd
        // (`resolve_new_session_cwd`'s "no source" branch) -- unchanged
        // from the behavior before terminal-cwd sourcing existed.
        let cwd = resolve_new_session_cwd(None, self.workspace, self.sessions);
        for session in self.workspace.with(|ws| ws.session_summaries()) {
            if session.attached {
                spawn_session(session.kind.into(), None, session.id, cwd.clone(), &runtime);
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
            self.config_reload_requests,
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
            palette_stage: RwSignal::new(PaletteStage::Commands),
            palette_focus_request: RwSignal::new(0_u64),
            pane_focus_requests: PaneFocusRequests::new(),
            window_focused: RwSignal::new(true),
            agent_drafts: AgentDrafts::new(),
            session_manager_open: RwSignal::new(false),
            session_manager_selection: RwSignal::new(0_usize),
            session_manager_selected_id: RwSignal::new(None),
            session_manager_pending_terminate: RwSignal::new(None),
            session_manager_focus_request: RwSignal::new(0_u64),
            agent_state_status: RwSignal::new(None::<String>),
            terminal_dump: None,
            clipboard_dump: None,
            status_dump: None,
            config_reload_requests: RwSignal::new(0_u64),
        }
    }
}
