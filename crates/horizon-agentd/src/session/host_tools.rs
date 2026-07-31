//! The agent (child) side of the host-tool channel: the round trip that
//! blocks a session thread while Horizon answers a `host_tool_request`.

use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract;
use horizon_agent::tools::HostTools;
use horizon_agent::wire::HostToolRequest;

use super::state::AgentdState;

/// How long a session thread waits for Horizon to answer a `host_tool_*`
/// round trip before giving up. Generous but finite: a client that never
/// answers must not hang a session forever.
const HOST_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Sends a host-tool request through the current connection's
/// `HubHello::host_tools` bridge, if a connection is live. Returns whether
/// the send was actually accepted, for the one caller
/// ([`AgentdHostTools::execute_auto`]) that needs to fail fast rather
/// than wait out its full timeout when nothing is listening.
fn send_host_tool_request(state: &AgentdState, request: HostToolRequest) -> bool {
    match state.host_tools_outgoing.lock().unwrap().as_ref() {
        Some(tx) => tx.send(request).is_ok(),
        None => false,
    }
}

/// The agent (child) side of the host-tool channel (guardrail 4 in
/// `docs/agent-runtime-split-design.md`): sends a `host_tool_request` over
/// the current connection (if any -- see [`send_host_tool_request`];
/// connection-global, exactly as the JSONL envelope's receiver treated it) and blocks this
/// session's dedicated thread on the matching `host_tool_response`. Only
/// `workspace.snapshot` is ever routed here today (the same tool id
/// Horizon's own `agent::host_tools::WorkspaceHostTools` answers
/// in-process) -- everything else falls through to `None`, letting
/// `execute_agent_tool` try the crate's own `tools::fs` auto tools next.
pub(super) struct AgentdHostTools {
    pub(super) state: Arc<AgentdState>,
}

impl HostTools for AgentdHostTools {
    fn execute_auto(&self, tool_id: &str, input: &serde_json::Value) -> Option<serde_json::Value> {
        if tool_id != "workspace.snapshot" {
            return None;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        self.state
            .pending_host_tool_requests
            .lock()
            .unwrap()
            .insert(request_id.clone(), reply_tx);

        let request = HostToolRequest {
            request_id: contract::RequestId(request_id.clone()),
            tool_id: tool_id.to_string(),
            input: input.clone().into(),
        };
        if !send_host_tool_request(&self.state, request) {
            self.state
                .pending_host_tool_requests
                .lock()
                .unwrap()
                .remove(&request_id);
            return None;
        }

        let response = reply_rx.recv_timeout(HOST_TOOL_TIMEOUT).ok();
        self.state
            .pending_host_tool_requests
            .lock()
            .unwrap()
            .remove(&request_id);
        response.map(|response| response.output.0)
    }
}
