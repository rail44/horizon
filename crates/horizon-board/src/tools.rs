//! The board's tools for an agent, as JSON in and out over a [`Store`]:
//! what `horizon mcp board` serves (`docs/agent-sessions-on-demand-design.md`).
//! Each returns the tool's result, or a message for the model.

use serde_json::{Value, json};

use crate::{Position, Store};

/// One tool: its name, title, description and argument schema, and
/// whether it only reads.
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub read_only: bool,
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "board_read",
            title: "Read Board",
            description: "Read the task board. If `id` is given, show that item with its full \
                comment thread; otherwise list all items in rank order, optionally filtered by \
                status. Items include their hierarchy, priority, dependencies, completion, \
                associated sessions, and conversation.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Show this item with its comments. If omitted, lists all items.",
                    },
                    "status": {
                        "type": "string",
                        "description": "Filter the list by status (e.g. proposed, ready, in-progress, review, done, blocked). Ignored when `id` is given.",
                    },
                }
            }),
            read_only: true,
        },
        Tool {
            name: "board_comment",
            title: "Add Board Comment",
            description: "Add a comment to a board item. Its author is the session this \
                server serves, which you cannot set. Comments are append-only; the board's \
                event log is the audit trail.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "text"],
                "properties": {
                    "id": {"type": "integer", "minimum": 1, "description": "Item id to comment on."},
                    "text": {"type": "string", "description": "Comment text (markdown)."},
                }
            }),
            read_only: false,
        },
        Tool {
            name: "board_update",
            title: "Update Board Task",
            description: "Create or edit ordinary tasks, set parent/dependencies, reorder among \
                siblings, or record project-defined status and closure. For move, provide \
                position and relative_to for before/after. Use close with is_closed=true for \
                finished or withdrawn work, or false to reopen; optional status is updated \
                atomically. Status text alone never changes is_closed.",
            input_schema: json!({"type":"object", "additionalProperties":false, "required":["action"],
            "properties": {
                "action":{"type":"string","enum":["add","edit","parent","dependencies","move","status","close"],"description":"close sets is_closed explicitly (true to close, false to reopen); an optional status is updated in the same transaction. status alone never changes is_closed."},
                "id":{"type":"integer","minimum":1},
                "title":{"type":"string"},"body":{"type":"string"},
                "parent":{"type":["integer","null"]},
                "depends_on":{"type":"array","items":{"type":"integer","minimum":1}},
                "position":{"type":"string","enum":["first","last","before","after"]},
                "relative_to":{"type":"integer","minimum":1},
                "status":{"type":"string"},"is_closed":{"type":"boolean"}
            }}),
            read_only: false,
        },
    ]
}

/// Runs `name` with `input` on `store`; a comment is written as `author`.
pub async fn call(store: &Store, author: &str, name: &str, input: &Value) -> Result<Value, String> {
    match name {
        "board_read" => read(store, input),
        "board_comment" => {
            let id = id(input, "id")?;
            let text = input
                .get("text")
                .and_then(Value::as_str)
                .ok_or("board_comment requires a `text` string argument")?;
            store.comment(id, author, text).await.map_err(render)?;
            Ok(json!({ "ok": true }))
        }
        "board_update" => update(store, input).await,
        _ => Err(format!("Unknown board tool: {name}")),
    }
}

fn read(store: &Store, input: &Value) -> Result<Value, String> {
    match input.get("id").and_then(Value::as_u64) {
        Some(id) => serde_json::to_value(store.show(id).map_err(render)?).map_err(render),
        None => {
            let status = input.get("status").and_then(Value::as_str);
            serde_json::to_value(store.list(status, true).map_err(render)?.items).map_err(render)
        }
    }
}

async fn update(store: &Store, input: &Value) -> Result<Value, String> {
    let action = string(input, "action")?;
    if action == "add" {
        let item = store
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
            store
                .edit(
                    id,
                    optional_string(input, "title")?,
                    optional_string(input, "body")?,
                )
                .await
        }
        "parent" => {
            store
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
            store.set_dependencies(id, dependencies).await
        }
        "move" => store.move_item(id, position(input)?).await.map(|_| ()),
        "status" => store.set_status(id, string(input, "status")?).await,
        "close" => {
            store
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
    serde_json::to_value(store.show(id).map_err(render)?).map_err(render)
}

fn string<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Missing or empty {key}"))
}

fn id(input: &Value, key: &str) -> Result<u64, String> {
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

fn render(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn reading_an_empty_board_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("events.jsonl"));
        assert_eq!(
            call(&store, "agent", "board_read", &json!({})).await,
            Ok(json!([]))
        );
        assert_eq!(
            call(&store, "agent", "board_read", &json!({"id": 3})).await,
            Ok(Value::Null)
        );
    }
}
