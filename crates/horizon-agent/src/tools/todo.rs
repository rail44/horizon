//! `todo.write`: a self-notetaking plan tool (`docs/agent-todo-tool-
//! design.md`). Replaces the *whole* list on every call (decision 1 —
//! no incremental add/update/remove ops); this module only validates the
//! input and reports terse counts back. The UI's Plan panel derives the
//! session's current list itself, from the raw event log
//! (`src/agent/turns.rs`'s `latest_todo_list`, decision 3) — this tool
//! never stores the list anywhere, and its result deliberately does not
//! echo `items` back (the UI reads the *request* input, not the result).

use serde_json::{json, Value};

use crate::tools::state::ToolSessionState;

/// Item cap (decision 2): generous for any real plan, cheap to enforce.
const MAX_ITEMS: usize = 50;
/// Per-item text cap (decision 2): a checklist label, not a scratchpad.
const MAX_TEXT_CHARS: usize = 200;

/// Executes `todo.write` if `tool_id` names it. `_tool_state` is unused —
/// unlike `fs`/`config`, this tool carries no session-scoped state (no
/// paths, no staleness gate) — but kept in the signature for the same
/// dispatch shape every other auto-allow module uses
/// (`tools::execution::execute_auto_tool`'s `.or_else` chain).
pub(crate) fn execute_auto(
    _tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    if tool_id != "todo.write" {
        return None;
    }
    Some(execute(input))
}

fn execute(input: &Value) -> Value {
    let Some(items) = input.get("items").and_then(Value::as_array) else {
        return error_output("todo.write requires an `items` array argument");
    };
    if items.len() > MAX_ITEMS {
        return error_output(format!(
            "todo.write accepts at most {MAX_ITEMS} items, got {}",
            items.len()
        ));
    }

    let mut pending = 0usize;
    let mut in_progress = 0usize;
    let mut done = 0usize;
    for (index, item) in items.iter().enumerate() {
        let Some(text) = item.get("text").and_then(Value::as_str) else {
            return error_output(format!("item {index} is missing a `text` string"));
        };
        if text.trim().is_empty() {
            return error_output(format!("item {index}'s `text` must not be empty"));
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return error_output(format!(
                "item {index}'s `text` exceeds {MAX_TEXT_CHARS} characters"
            ));
        }
        match item.get("status").and_then(Value::as_str) {
            Some("pending") => pending += 1,
            Some("in_progress") => in_progress += 1,
            Some("done") => done += 1,
            Some(other) => {
                return error_output(format!(
                    "item {index} has an invalid `status` \"{other}\" (expected pending, \
                     in_progress, or done)"
                ))
            }
            None => return error_output(format!("item {index} is missing a `status` string")),
        }
    }

    json!({
        "total": items.len(),
        "pending": pending,
        "in_progress": in_progress,
        "done": done,
    })
}

fn error_output(message: impl Into<String>) -> Value {
    json!({ "is_error": true, "message": message.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_error(output: &Value) -> bool {
        output.get("is_error").and_then(Value::as_bool) == Some(true)
    }

    #[test]
    fn accepts_a_well_formed_list_and_reports_terse_counts() {
        let output = execute(&json!({
            "items": [
                { "text": "Read the design doc", "status": "done" },
                { "text": "Implement the tool", "status": "in_progress" },
                { "text": "Write tests", "status": "pending" },
            ]
        }));

        assert!(!is_error(&output));
        assert_eq!(output["total"], 3);
        assert_eq!(output["done"], 1);
        assert_eq!(output["in_progress"], 1);
        assert_eq!(output["pending"], 1);
        // The result never echoes `items` back (decision 3).
        assert!(output.get("items").is_none());
    }

    #[test]
    fn accepts_an_empty_list() {
        let output = execute(&json!({ "items": [] }));
        assert!(!is_error(&output));
        assert_eq!(output["total"], 0);
    }

    #[test]
    fn rejects_a_missing_items_field() {
        let output = execute(&json!({}));
        assert!(is_error(&output));
        assert!(output["message"].as_str().unwrap().contains("items"));
    }

    #[test]
    fn rejects_more_than_the_item_cap() {
        let items: Vec<Value> = (0..MAX_ITEMS + 1)
            .map(|index| json!({ "text": format!("step {index}"), "status": "pending" }))
            .collect();
        let output = execute(&json!({ "items": items }));
        assert!(is_error(&output));
        assert!(output["message"].as_str().unwrap().contains("50"));
    }

    #[test]
    fn rejects_empty_text() {
        let output = execute(&json!({ "items": [{ "text": "  ", "status": "pending" }] }));
        assert!(is_error(&output));
        assert!(output["message"].as_str().unwrap().contains("text"));
    }

    #[test]
    fn rejects_text_over_the_char_cap() {
        let text = "x".repeat(MAX_TEXT_CHARS + 1);
        let output = execute(&json!({ "items": [{ "text": text, "status": "pending" }] }));
        assert!(is_error(&output));
        assert!(output["message"]
            .as_str()
            .unwrap()
            .contains("200 characters"));
    }

    #[test]
    fn rejects_an_invalid_status() {
        let output = execute(&json!({ "items": [{ "text": "step", "status": "blocked" }] }));
        assert!(is_error(&output));
        assert!(output["message"].as_str().unwrap().contains("status"));
    }

    #[test]
    fn rejects_a_missing_status() {
        let output = execute(&json!({ "items": [{ "text": "step" }] }));
        assert!(is_error(&output));
        assert!(output["message"].as_str().unwrap().contains("status"));
    }

    #[test]
    fn execute_auto_returns_none_for_other_tool_ids() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        assert!(execute_auto(&tool_state, "fs.read", &json!({})).is_none());
    }

    #[test]
    fn execute_auto_dispatches_todo_write() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let output = execute_auto(
            &tool_state,
            "todo.write",
            &json!({ "items": [{ "text": "step", "status": "pending" }] }),
        )
        .expect("todo.write is auto-executed");
        assert!(!is_error(&output));
        assert_eq!(output["total"], 1);
    }
}
