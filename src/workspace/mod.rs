mod input;
mod layout;
mod operations;
mod query;
mod session;
mod types;
pub(crate) mod view;

pub(crate) use input::{
    active_agent, active_agent_draft, active_terminal_sender, active_text_input_pane,
    handle_active_pane_key, request_active_pane_focus, trace_ime, visible_agent_sender,
    visible_terminal_sender, AgentDrafts, PaneFocusRequests, MAX_VISIBLE_PANES,
};
pub(crate) use types::{PaneKind, PaneSummary, SessionKind, Workspace};

#[cfg(test)]
use types::{SessionSummary, TabSummary};

#[cfg(test)]
mod tests;
