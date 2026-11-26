//! A host-side web request that needs a domain the session does not have
//! becomes a new approval, before any contact is made.

use crate::session::approval::begin_reissued_approval;
use crate::session::state::AgentHostState;
use horizon_agent::contract::{ApprovalKind, Command, SessionId, ToolCallRequest};
use horizon_agent::live::LiveState;
use std::sync::Arc;

pub(super) fn fold_domain_grant_required(
    state: &Arc<AgentHostState>,
    live_state: &LiveState,
    commands_tx: &crossbeam_channel::Sender<Command>,
    session_id: SessionId,
    original_request: ToolCallRequest,
    domains: Vec<String>,
) {
    let domain_list = domains.join(", ");
    let reason = format!(
        "`{}` needs to contact {domain_list}, but no request was sent to that domain. Allow {} \
         for this session and retry from the original URL?",
        original_request.tool_id,
        if domains.len() == 1 { "it" } else { "them" }
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request,
        ApprovalKind::DomainGrant { domains },
        reason,
        commands_tx,
    );
}
