//! Board-to-session integration, composed into the agent host as an
//! [`Extension`]. Policy stays in skills; this adapter delivers identified
//! inputs and settled outcomes using durable logs on both sides.

mod dispatch;
mod host;
mod operations;
pub(crate) mod roles;

use horizon_agent::contract::SessionId;
use horizon_agent::packages::PackagesBuilder;
use horizon_agent::tools::BoardHost;
use horizon_agent_host::extension::Extension;
use horizon_agent_host::outbox::{ReplySink, TriggerSource};
use horizon_agent_host::session::AgentdState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) use operations::{operate, organizer_session};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ReplyAddress {
    Board {
        root: PathBuf,
        task: u64,
    },
    Session {
        id: String,
    },
    /// Return review details to the requester, then route that task's
    /// continuation answer to its board conversation.
    ReviewResult {
        id: String,
        root: PathBuf,
        task: u64,
    },
}
impl ReplyAddress {
    fn board(root: &Path, task: u64) -> Result<String, String> {
        let root =
            horizon_agent_host::worktree::project_root(root).ok_or("Board root is unavailable")?;
        serde_json::to_string(&Self::Board { root, task }).map_err(|e| e.to_string())
    }
    fn session(id: SessionId) -> String {
        serde_json::to_string(&Self::Session {
            id: id.as_uuid().to_string(),
        })
        .expect("session address serializes")
    }
    fn review_result(root: &Path, task: u64, id: SessionId) -> Result<String, String> {
        let root =
            horizon_agent_host::worktree::project_root(root).ok_or("Board root is unavailable")?;
        serde_json::to_string(&Self::ReviewResult {
            id: id.as_uuid().to_string(),
            root,
            task,
        })
        .map_err(|e| e.to_string())
    }
}

/// The project roots whose boards are watched: registered by a board-role
/// session's records, by the board tool, and by the client.
#[derive(Default)]
pub(crate) struct Projects(Mutex<HashSet<PathBuf>>);

impl Projects {
    pub(crate) fn register(&self, root: PathBuf) {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(root);
    }

    pub(crate) fn list(&self) -> Vec<PathBuf> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Registers the project `root` is in.
    fn register_project_of(&self, root: &Path) -> Result<PathBuf, String> {
        let root = horizon_agent_host::worktree::project_root(root)
            .ok_or("Board root is not a Git repository")?;
        self.register(root.clone());
        Ok(root)
    }
}

/// The board, composed into the host.
pub struct BoardFlow {
    projects: Arc<Projects>,
}

impl BoardFlow {
    /// With `start_dir`'s board (the host's start directory) watched from
    /// the start.
    pub fn new(start_dir: Option<PathBuf>) -> Self {
        let projects = Arc::new(Projects::default());
        if let Some(root) = start_dir {
            projects.register(root);
        }
        Self { projects }
    }
}

impl Extension for BoardFlow {
    fn packages(&self, packages: PackagesBuilder) -> PackagesBuilder {
        roles::add(packages)
    }

    fn trigger_sources(&self) -> Vec<Box<dyn TriggerSource>> {
        vec![Box::new(dispatch::BoardSource::new(self.projects.clone()))]
    }

    fn reply_sinks(&self) -> Vec<Arc<dyn ReplySink>> {
        vec![Arc::new(dispatch::BoardSink)]
    }

    fn board_host(&self, state: &Arc<AgentdState>, root: &Path) -> Option<Arc<dyn BoardHost>> {
        Some(Arc::new(host::AgentdBoardHost::new(
            root,
            state.clone(),
            self.projects.clone(),
        )?))
    }

    fn serves(&self, method: &str) -> bool {
        method == ENSURE_BOARD_ORGANIZER
    }

    fn request(
        &self,
        state: &Arc<AgentdState>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match method {
            ENSURE_BOARD_ORGANIZER => {
                let request: horizon_acp::EnsureBoardOrganizerRequest =
                    serde_json::from_value(params).map_err(|error| error.to_string())?;
                self.projects.register_project_of(&request.workspace_root)?;
                let session_id = organizer_session(state, &request.workspace_root)?;
                to_value(horizon_acp::EnsureBoardOrganizerResponse { session_id })
            }
            _ => Err(format!("{method} is not served")),
        }
    }
}

const ENSURE_BOARD_ORGANIZER: &str = "_horizon/ensure_board_organizer";

fn to_value(response: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(response).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_agent_host::session::test_support::state_with_rig_config;

    #[test]
    fn the_board_method_keeps_its_wire_name() {
        use agent_client_protocol::JsonRpcMessage;
        let root = PathBuf::from("/");
        assert_eq!(
            horizon_acp::EnsureBoardOrganizerRequest {
                workspace_root: root
            }
            .method(),
            ENSURE_BOARD_ORGANIZER
        );
    }

    #[test]
    fn an_organizer_outside_a_repository_is_refused() {
        let flow = BoardFlow::new(None);
        let dir = tempfile::tempdir().unwrap();
        let state = state_with_rig_config(false, "test");
        let refused = flow.request(
            &state,
            ENSURE_BOARD_ORGANIZER,
            serde_json::json!({ "workspaceRoot": dir.path() }),
        );
        assert!(refused.is_err(), "{refused:?}");
        assert!(flow.projects.list().is_empty());
    }
}
