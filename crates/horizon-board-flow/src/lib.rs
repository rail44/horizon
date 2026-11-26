//! Board-to-session integration, composed into the agent host as an
//! [`Extension`]. Policy stays in skills; this adapter delivers the board
//! tool's requests and their answers using durable logs on both sides.

mod host;
mod operations;
pub(crate) mod roles;
mod sink;

use horizon_agent::contract::SessionId;
use horizon_agent::packages::PackagesBuilder;
use horizon_agent::tools::BoardHost;
use horizon_agent_host::extension::Extension;
use horizon_agent_host::outbox::ReplySink;
use horizon_agent_host::session::AgentdState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use operations::operate;

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

/// Where the board is kept: the data home its stores are under, and the
/// socket of the log daemon that writes them.
#[derive(Clone, Debug)]
pub struct BoardPlaces {
    pub data_home: PathBuf,
    pub logd_socket: PathBuf,
}

impl BoardPlaces {
    /// The store of the project `dir` is in.
    pub(crate) fn store(&self, dir: &Path) -> Result<horizon_board::Store, String> {
        horizon_board::Store::from_dir_in(dir, &self.data_home, self.logd_socket.clone())
            .map_err(|error| error.to_string())
    }
}

/// The board, composed into the host: the board's tools, its task and
/// reviewer roles, and the sink that posts answers to its threads.
pub struct BoardFlow {
    places: BoardPlaces,
}

impl BoardFlow {
    pub fn new(places: BoardPlaces) -> Self {
        Self { places }
    }
}

impl Extension for BoardFlow {
    fn packages(&self, packages: PackagesBuilder) -> PackagesBuilder {
        roles::add(packages)
    }

    fn reply_sinks(&self) -> Vec<Arc<dyn ReplySink>> {
        vec![Arc::new(sink::BoardSink::new(self.places.clone()))]
    }

    fn board_host(&self, state: &Arc<AgentdState>, root: &Path) -> Option<Arc<dyn BoardHost>> {
        Some(Arc::new(host::AgentdBoardHost::new(
            &self.places,
            root,
            state.clone(),
        )?))
    }
}
