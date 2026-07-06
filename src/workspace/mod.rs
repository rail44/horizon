mod input;
mod layout;
mod mode;
mod mode_input;
mod operations;
mod query;
mod session;
mod types;
pub(crate) mod view;

pub(crate) use input::{
    active_agent, active_agent_draft, active_terminal_sender, active_text_input_pane,
    handle_active_pane_key, handle_active_pane_key_release, handle_agent_banner_key,
    request_active_pane_focus, trace_ime, visible_terminal_sender, AgentDrafts, BannerKeyAction,
    PaneFocusRequests, MAX_VISIBLE_PANES,
};
pub(crate) use mode::Direction;
pub(crate) use mode_input::{
    agent_escape_requests_workspace_mode, handle_workspace_mode_key, ModeAction,
};
pub(crate) use types::{PaneKind, PaneSummary, SessionKind, Workspace};

#[cfg(test)]
use types::{SessionSummary, TabSummary};

#[cfg(test)]
mod tests;
