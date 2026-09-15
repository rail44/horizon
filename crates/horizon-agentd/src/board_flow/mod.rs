//! Board-to-session integration. Policy stays in skills; this adapter delivers
//! identified inputs and settled outcomes using durable logs on both sides.

mod dispatch;
mod log;
mod operations;
pub(crate) mod roles;
#[cfg(test)]
mod tests;

use crate::session::AgentdState;
use horizon_agent::contract::SessionId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) use operations::operate;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ReplyAddress {
    Board { root: PathBuf, task: u64 },
    Session { id: String },
}
impl ReplyAddress {
    fn board(root: &Path, task: u64) -> Result<String, String> {
        let root = crate::worktree::project_root(root).ok_or("Board root is unavailable")?;
        serde_json::to_string(&Self::Board { root, task }).map_err(|e| e.to_string())
    }
    fn session(id: SessionId) -> String {
        serde_json::to_string(&Self::Session {
            id: id.as_uuid().to_string(),
        })
        .expect("session address serializes")
    }
}

pub(crate) fn spawn(state: Arc<AgentdState>, root: Option<PathBuf>) {
    if let Some(root) = root {
        state.register_board(root);
    }
    tokio::spawn(async move {
        state.wait_until_resume_ready().await;
        let path = state
            .agent_config
            .lock()
            .unwrap()
            .persistence
            .event_log_path
            .clone();
        let mut runner = dispatch::Runner::new(state, path);
        let mut last_error = None;
        loop {
            match runner.tick().await {
                Ok(()) => last_error = None,
                Err(error) => {
                    if last_error.as_ref() != Some(&error) {
                        eprintln!("horizon-agentd: board delivery: {error}");
                    }
                    last_error = Some(error);
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
}
