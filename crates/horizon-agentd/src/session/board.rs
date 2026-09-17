//! Board data and task-session tools at the agent/board composition boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use horizon_agent::contract::{Event, SessionId, ToolCallRequest};
use horizon_agent::tools::BoardHost;
use horizon_board::{Position, Store};
use serde_json::Value;

pub(super) struct AgentdBoardHost {
    store: Store,
    root: PathBuf,
    state: Arc<super::AgentdState>,
}

impl AgentdBoardHost {
    pub(super) fn new(root: &Path, state: Arc<super::AgentdState>) -> Option<Self> {
        let store = Store::from_dir(root).ok()?;
        Some(Self {
            store,
            root: root.to_path_buf(),
            state,
        })
    }

    async fn update(&self, input: &Value) -> Result<Value, String> {
        let action = string(input, "action")?;
        if action == "add" {
            let item = self
                .store
                .add(
                    string(input, "title")?,
                    input.get("body").and_then(Value::as_str).unwrap_or(""),
                    optional_id(input, "parent")?,
                    position(input)?,
                )
                .await
                .map_err(render)?;
            return serde_json::to_value(item).map_err(render);
        }
        let id = id(input, "id")?;
        match action {
            "edit" => {
                self.store
                    .edit(
                        id,
                        optional_string(input, "title")?,
                        optional_string(input, "body")?,
                    )
                    .await
            }
            "parent" => {
                self.store
                    .set_parent(id, optional_id(input, "parent")?, position(input)?)
                    .await
            }
            "dependencies" => {
                let dependencies = serde_json::from_value::<Vec<u64>>(
                    input
                        .get("depends_on")
                        .cloned()
                        .ok_or("Missing depends_on")?,
                )
                .map_err(render)?;
                self.store.set_dependencies(id, dependencies).await
            }
            "move" => self.store.move_item(id, position(input)?).await.map(|_| ()),
            "status" => self.store.set_status(id, string(input, "status")?).await,
            "close" => {
                self.store
                    .set_closed(
                        id,
                        input
                            .get("is_closed")
                            .and_then(Value::as_bool)
                            .ok_or("Missing is_closed boolean")?,
                        optional_string(input, "status")?.as_deref(),
                    )
                    .await
            }
            _ => return Err(format!("Unknown board update action: {action}")),
        }
        .map_err(render)?;
        serde_json::to_value(self.store.show(id).map_err(render)?).map_err(render)
    }
}

impl BoardHost for AgentdBoardHost {
    fn list(&self, status_filter: Option<&str>) -> Result<Value, String> {
        self.state.register_board(self.root.clone());
        serde_json::to_value(self.store.list(status_filter, true).map_err(render)?.items)
            .map_err(render)
    }
    fn show(&self, id: u64) -> Result<Value, String> {
        self.state.register_board(self.root.clone());
        serde_json::to_value(self.store.show(id).map_err(render)?).map_err(render)
    }
    fn comment(&self, id: u64, author: &str, text: &str) -> Result<(), String> {
        self.state.register_board(self.root.clone());
        runtime()?
            .block_on(self.store.comment(id, author, text))
            .map_err(render)
    }
    fn operate(
        &self,
        session: SessionId,
        request: &ToolCallRequest,
    ) -> Result<(Value, Vec<Event>), String> {
        self.state.register_board(self.root.clone());
        let runtime = runtime()?;
        match request.tool_id.as_str() {
            "board.update" => runtime
                .block_on(self.update(&request.input))
                .map(|value| (value, Vec::new())),
            "board.session" => runtime.block_on(crate::board_flow::operate(
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

pub(crate) fn string<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Missing or empty {key}"))
}
pub(crate) fn id(input: &Value, key: &str) -> Result<u64, String> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| format!("Missing positive {key}"))
}
fn optional_id(input: &Value, key: &str) -> Result<Option<u64>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        _ => id(input, key).map(Some),
    }
}
fn optional_string(input: &Value, key: &str) -> Result<Option<String>, String> {
    match input.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|s| Some(s.into()))
            .ok_or_else(|| format!("{key} must be text")),
    }
}
fn position(input: &Value) -> Result<Position, String> {
    match input
        .get("position")
        .and_then(Value::as_str)
        .unwrap_or("last")
    {
        "first" => Ok(Position::Top),
        "last" => Ok(Position::Bottom),
        "before" => Ok(Position::Before(id(input, "relative_to")?)),
        "after" => Ok(Position::After(id(input, "relative_to")?)),
        other => Err(format!("Unknown position: {other}")),
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

pub(super) fn board_host_for(
    root: Option<&Path>,
    state: Arc<super::AgentdState>,
) -> Option<Arc<dyn BoardHost>> {
    Some(Arc::new(AgentdBoardHost::new(root?, state)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn relative_reorder_requires_an_existing_target_argument() {
        assert!(position(&json!({"position":"before"})).is_err());
        assert_eq!(
            position(&json!({"position":"after","relative_to":7})).unwrap(),
            Position::After(7)
        );
    }
    #[test]
    fn malformed_parent_is_not_silently_cleared() {
        assert!(optional_id(&json!({"parent":"3"}), "parent").is_err());
        assert_eq!(
            optional_id(&json!({"parent":null}), "parent").unwrap(),
            None
        );
    }
}
