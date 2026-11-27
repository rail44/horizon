//! `board_session` at the agent/board composition boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use horizon_agent::contract::{Event, SessionId, ToolCallRequest};
use horizon_agent::tools::BoardHost;
use horizon_agent_host::session::AgentHostState;
use horizon_board::Store;
use serde_json::Value;

pub(super) struct AgentHostBoardHost {
    store: Store,
    root: PathBuf,
    state: Arc<AgentHostState>,
}

impl AgentHostBoardHost {
    pub(super) fn new(
        places: &super::BoardPlaces,
        root: &Path,
        state: Arc<AgentHostState>,
    ) -> Option<Self> {
        let store = places.store(root).ok()?;
        Some(Self {
            store,
            root: root.to_path_buf(),
            state,
        })
    }
}

impl BoardHost for AgentHostBoardHost {
    fn operate(
        &self,
        session: SessionId,
        request: &ToolCallRequest,
    ) -> Result<(Value, Vec<Event>), String> {
        let runtime = runtime()?;
        match request.tool_id.as_str() {
            "board_session" => runtime.block_on(super::operate(
                self.state.clone(),
                &self.store,
                &self.root,
                session,
                request,
            )),
            _ => Err("Unknown board operation".into()),
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(render)
}
fn render(error: impl std::fmt::Display) -> String {
    error.to_string()
}
