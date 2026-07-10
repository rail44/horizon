//! The Floem shell's workspace layer. The domain model — tabs, panes,
//! the layout tree, session attachments, operations/queries, mode
//! state, spatial navigation — lives in `crates/horizon-workspace`
//! (shared with `shell-gpui/`, see docs/gpui-migration-design.md);
//! this module keeps the shell-specific halves (pane input routing and
//! views) and re-exports the model so existing `crate::workspace::*`
//! and `super::*` paths keep working unchanged.

mod input;
mod mode_input;
pub(crate) mod view;

pub(crate) use horizon_workspace::{layout, mode, types};

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
