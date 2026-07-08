mod input;
mod layout;
mod mode;
mod mode_input;
mod nav;
mod operations;
mod query;
mod session;
mod types;
pub(crate) mod view;

pub(crate) use input::{
    active_agent, active_agent_draft, active_terminal_sender, active_text_input_pane,
    handle_active_pane_key, handle_active_pane_key_release, handle_agent_approval_key,
    insert_agent_draft_text, pane_terminal_sender, request_active_pane_focus, trace_ime,
    AgentDraft, AgentDrafts, ApprovalKeyAction, PaneFocusRequests,
};
pub(crate) use mode::Direction;
pub(crate) use mode_input::{
    agent_escape_requests_workspace_mode, handle_workspace_mode_key, ModeAction,
};
pub(crate) use types::{PaneId, PaneKind, SessionKind, SplitAxis, Workspace};

#[cfg(test)]
use types::{PaneSummary, SessionSummary, TabSummary};

#[cfg(test)]
mod tests;
