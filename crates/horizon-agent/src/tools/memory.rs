//! `memory.update` tool dispatch and the standing-agent memory document
//! (`docs/standing-agent-memory-design.md`).
//!
//! A standing-role session (one whose `RoleDefinition::standing` is true)
//! maintains a **memory document** — a structured summary of the project's
//! state that carries across turns, so a long-lived agent (the keeper) does
//! not start from zero each spawn. The agent edits this document one field at
//! a time via the typed `memory.update` tool; the harness persists each edit
//! as an `Event::MemoryDigest` and reconstructs the current document by
//! replaying them (`memory_document_from_events`).
//!
//! **Incremental, not regenerative.** Each turn's edit is a set of per-field
//! operations (`Set`/`Append`/`Clear`); the document is never carried as a
//! whole, only as the diffs that built it. Full-document regeneration
//! collapses under iteration — ACE 66.7→57.1%, codex#14589 13.7→6.9%
//! (`docs/research/standing-agent-memory-evidence-2026-08-15.md` §2-1) — so
//! the schema structurally forbids it: you specify the fields you changed and
//! what to do with each, never the whole document.
//!
//! **No host trait.** Unlike `board.comment` (which writes to an external
//! store via `BoardHost`), `memory.update` has no side effect beyond the event
//! the session loop emits. The handler here is pure: it validates the input
//! and returns a confirmation. The session loop
//! (`providers::rig::session::state`, `Command::ToolCallResult` arm) parses the
//! same input into a `MemoryDigest`, applies it to the session's `MemoryState`,
//! and emits the event — so the state mutation and the persistence happen in
//! one place (the loop), not split across a host boundary.

use serde_json::{json, Value};

use crate::contract::{
    Event, FoldedLogRange, MemoryDigest, MemoryField, MemoryFieldUpdate, MemoryOp,
};
use crate::tools::error_output;

/// The model-visible tool id.
pub(crate) const TOOL_ID: &str = "memory.update";

/// The seven fields of the Tier 2 template, as display labels for rendering
/// and for the tool schema's field descriptions.
const FIELD_LABELS: &[(MemoryField, &str, &str)] = &[
    (
        MemoryField::Goal,
        "goal",
        "The overarching goal this session is serving.",
    ),
    (
        MemoryField::Decisions,
        "decisions",
        "Decisions made, with rationale.",
    ),
    (MemoryField::Completed, "completed", "Work that is done."),
    (
        MemoryField::InProgress,
        "in_progress",
        "Work currently underway.",
    ),
    (
        MemoryField::Stuck,
        "stuck",
        "What is blocked or unresolved, and why.",
    ),
    (
        MemoryField::NextStep,
        "next_step",
        "The single next action to take.",
    ),
    (
        MemoryField::Related,
        "related",
        "Files, symbols, and paths relevant to the current work.",
    ),
];

fn field_label(field: MemoryField) -> &'static str {
    FIELD_LABELS
        .iter()
        .find(|(f, _, _)| *f == field)
        .map(|(_, label, _)| *label)
        .unwrap_or("unknown")
}

// ---------------------------------------------------------------------------
// MemoryDocument — the reconstructed current state
// ---------------------------------------------------------------------------

/// The standing agent's memory document: seven free-text fields, each
/// maintained incrementally. Reconstructed by replaying `MemoryDigest` events
/// (`memory_document_from_events`); the session loop holds the live copy in
/// `MemoryState` and the provider-view projection renders it via `render`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MemoryDocument {
    pub goal: String,
    pub decisions: String,
    pub completed: String,
    pub in_progress: String,
    pub stuck: String,
    pub next_step: String,
    pub related: String,
}

impl MemoryDocument {
    /// Whether every field is empty — the projection skips prepending the
    /// document when this is true (a session that has never updated its
    /// memory sends its full history unchanged).
    pub fn is_empty(&self) -> bool {
        self.goal.is_empty()
            && self.decisions.is_empty()
            && self.completed.is_empty()
            && self.in_progress.is_empty()
            && self.stuck.is_empty()
            && self.next_step.is_empty()
            && self.related.is_empty()
    }

    fn field_mut(&mut self, field: MemoryField) -> &mut String {
        match field {
            MemoryField::Goal => &mut self.goal,
            MemoryField::Decisions => &mut self.decisions,
            MemoryField::Completed => &mut self.completed,
            MemoryField::InProgress => &mut self.in_progress,
            MemoryField::Stuck => &mut self.stuck,
            MemoryField::NextStep => &mut self.next_step,
            MemoryField::Related => &mut self.related,
        }
    }

    /// Applies one digest's field operations to this document. A `Skipped`
    /// digest (no updates, reason set) or a no-op leaves the document
    /// unchanged — only `Updated` digests with field operations mutate it.
    pub fn apply(&mut self, digest: &MemoryDigest) {
        for update in &digest.updates {
            let field = self.field_mut(update.field);
            match update.op {
                MemoryOp::Set => *field = update.content.clone(),
                MemoryOp::Append => {
                    if !field.is_empty() && !field.ends_with('\n') {
                        field.push('\n');
                    }
                    field.push_str(&update.content);
                }
                MemoryOp::Clear => field.clear(),
            }
        }
    }

    /// Renders the document as a single text block for the provider-view
    /// projection. Empty fields are omitted; if all fields are empty this
    /// returns an empty string.
    pub fn render(&self) -> String {
        let pairs: [(MemoryField, &str); 7] = [
            (MemoryField::Goal, &self.goal),
            (MemoryField::Decisions, &self.decisions),
            (MemoryField::Completed, &self.completed),
            (MemoryField::InProgress, &self.in_progress),
            (MemoryField::Stuck, &self.stuck),
            (MemoryField::NextStep, &self.next_step),
            (MemoryField::Related, &self.related),
        ];
        let mut sections = Vec::new();
        for (field, content) in &pairs {
            if content.is_empty() {
                continue;
            }
            sections.push(format!("## {}\n{}", field_label(*field), content));
        }
        if sections.is_empty() {
            return String::new();
        }
        format!(
            "# Current memory document (standing agent state)\n\n{}",
            sections.join("\n\n")
        )
    }
}

/// Replays every persisted `MemoryDigest` event's field operations to
/// reconstruct the current memory document — the resume/replay counterpart to
/// the live `MemoryState`, mirroring `clearing::cleared_call_ids_from_events`.
/// Called at session spawn to seed `MemoryState` from the event log.
pub fn memory_document_from_events(events: &[Event]) -> MemoryDocument {
    let mut document = MemoryDocument::default();
    for event in events {
        if let Event::MemoryDigest(digest) = event {
            document.apply(digest);
        }
    }
    document
}

// ---------------------------------------------------------------------------
// Tool input parsing
// ---------------------------------------------------------------------------

/// Parses a `memory.update` tool-call input into a `MemoryDigest`, validating
/// that it is well-formed: exactly one of "field operations" or "no update"
/// is present, `Set`/`Append` operations carry content, and field/op values
/// are recognized. Used both by the tool handler (to validate and confirm)
/// and by the session loop (to emit the event) — the input is the same
/// `ToolCallDescriptor::args` in both places, so the parse is idempotent.
pub(crate) fn parse_update(input: &Value) -> Result<MemoryDigest, String> {
    let obj = input.as_object().ok_or("input must be a JSON object")?;

    let no_update = obj.get("no_update");
    let field_keys: Vec<&str> = FIELD_LABELS.iter().map(|(_, key, _)| *key).collect();
    let present_fields: Vec<&str> = field_keys
        .iter()
        .copied()
        .filter(|key| obj.contains_key(*key))
        .collect();

    if let Some(no_update_val) = no_update {
        if !present_fields.is_empty() {
            return Err(format!(
                "cannot combine `no_update` with field operations ({})",
                present_fields.join(", ")
            ));
        }
        let reason = no_update_val
            .get("reason")
            .and_then(Value::as_str)
            .ok_or("`no_update.reason` must be a non-empty string")?
            .trim();
        if reason.is_empty() {
            return Err("`no_update.reason` must be a non-empty string".to_string());
        }
        return Ok(MemoryDigest {
            updates: Vec::new(),
            folded_log_range: None,
            no_update_reason: Some(reason.to_string()),
        });
    }

    if present_fields.is_empty() {
        return Err(
            "provide at least one field operation (goal/decisions/completed/in_progress/\
             stuck/next_step/related) or declare `no_update` with a reason"
                .to_string(),
        );
    }

    let mut updates = Vec::new();
    for key in &present_fields {
        let (field, _, _) = FIELD_LABELS
            .iter()
            .find(|(_, k, _)| k == key)
            .copied()
            .expect("present_fields are all valid keys");
        let op_obj = obj
            .get(*key)
            .and_then(Value::as_object)
            .ok_or(format!("`{key}` must be an object with `op` and `content`"))?;
        let op_str = op_obj
            .get("op")
            .and_then(Value::as_str)
            .ok_or(format!("`{key}.op` must be one of: set, append, clear"))?;
        let op = match op_str {
            "set" => MemoryOp::Set,
            "append" => MemoryOp::Append,
            "clear" => MemoryOp::Clear,
            other => {
                return Err(format!(
                    "`{key}.op` must be set/append/clear, got `{other}`"
                ))
            }
        };
        let content = match op {
            MemoryOp::Clear => String::new(),
            MemoryOp::Set | MemoryOp::Append => {
                let content = op_obj
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or(format!(
                        "`{key}.content` must be a string when op is {op_str:?}"
                    ))?
                    .trim();
                if content.is_empty() {
                    return Err(format!(
                        "`{key}.content` must be non-empty when op is {op_str:?}"
                    ));
                }
                content.to_string()
            }
        };
        updates.push(MemoryFieldUpdate { field, op, content });
    }

    let folded_log_range = obj
        .get("folded_log_range")
        .and_then(|v| v.as_object())
        .and_then(|o| {
            let from = o.get("from_seq")?.as_u64()?;
            let to = o.get("to_seq")?.as_u64()?;
            Some(FoldedLogRange {
                from_seq: from,
                to_seq: to,
            })
        });

    Ok(MemoryDigest {
        updates,
        folded_log_range,
        no_update_reason: None,
    })
}

/// Executes the `memory.update` auto-allowed tool: validates the input and
/// returns a confirmation. Returns `None` for any other tool id, so the caller
/// can try elsewhere — same contract as `tools::knowledge::execute_auto`.
///
/// **No side effect here.** The session loop owns the state mutation and event
/// emission (see the module doc): this handler only validates and confirms, so
/// the model sees whether its edit was well-formed before the loop applies it.
pub(crate) fn execute_auto(tool_id: &str, input: &Value) -> Option<Value> {
    if tool_id != TOOL_ID {
        return None;
    }
    match parse_update(input) {
        Ok(digest) => {
            if let Some(reason) = &digest.no_update_reason {
                Some(json!({
                    "ok": true,
                    "no_update": true,
                    "reason": reason,
                }))
            } else {
                let fields: Vec<&str> = digest
                    .updates
                    .iter()
                    .map(|u| field_label(u.field))
                    .collect();
                let mut result = json!({
                    "ok": true,
                    "fields_updated": fields,
                });
                if let Some(range) = digest.folded_log_range {
                    result["folded_log_range"] = json!({
                        "from_seq": range.from_seq,
                        "to_seq": range.to_seq,
                    });
                }
                Some(result)
            }
        }
        Err(message) => Some(error_output(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: &str, content: &str) -> Value {
        json!({ "op": op, "content": content })
    }

    // -- parse_update / schema validation ----------------------------------

    #[test]
    fn parse_set_and_append() {
        let input = json!({
            "goal": op("set", "Ship the memory feature"),
            "decisions": op("append", "Incremental updates only"),
        });
        let digest = parse_update(&input).expect("valid input");
        assert_eq!(digest.updates.len(), 2);
        assert_eq!(digest.updates[0].field, MemoryField::Goal);
        assert_eq!(digest.updates[0].op, MemoryOp::Set);
        assert_eq!(digest.updates[1].field, MemoryField::Decisions);
        assert_eq!(digest.updates[1].op, MemoryOp::Append);
        assert!(digest.no_update_reason.is_none());
    }

    #[test]
    fn parse_clear_ignores_content() {
        let input = json!({ "stuck": { "op": "clear" } });
        let digest = parse_update(&input).expect("valid input");
        assert_eq!(digest.updates.len(), 1);
        assert_eq!(digest.updates[0].op, MemoryOp::Clear);
        assert!(digest.updates[0].content.is_empty());
    }

    #[test]
    fn parse_no_update_with_reason() {
        let input = json!({ "no_update": { "reason": "Nothing changed this turn." } });
        let digest = parse_update(&input).expect("valid input");
        assert!(digest.updates.is_empty());
        assert_eq!(
            digest.no_update_reason.as_deref(),
            Some("Nothing changed this turn.")
        );
    }

    #[test]
    fn parse_folded_log_range() {
        let input = json!({
            "goal": op("set", "x"),
            "folded_log_range": { "from_seq": 10, "to_seq": 47 },
        });
        let digest = parse_update(&input).expect("valid input");
        assert_eq!(
            digest.folded_log_range,
            Some(FoldedLogRange {
                from_seq: 10,
                to_seq: 47
            })
        );
    }

    #[test]
    fn parse_rejects_no_fields_and_no_skip() {
        let input = json!({});
        assert!(parse_update(&input).is_err());
    }

    #[test]
    fn parse_rejects_combining_fields_with_no_update() {
        let input = json!({
            "goal": op("set", "x"),
            "no_update": { "reason": "..." },
        });
        assert!(parse_update(&input).is_err());
    }

    #[test]
    fn parse_rejects_empty_no_update_reason() {
        let input = json!({ "no_update": { "reason": "  " } });
        assert!(parse_update(&input).is_err());
    }

    #[test]
    fn parse_rejects_set_with_empty_content() {
        let input = json!({ "goal": op("set", "  ") });
        assert!(parse_update(&input).is_err());
    }

    #[test]
    fn parse_rejects_unknown_op() {
        let input = json!({ "goal": { "op": "replace", "content": "x" } });
        assert!(parse_update(&input).is_err());
    }

    // -- fold (incremental apply) -----------------------------------------

    #[test]
    fn document_starts_empty() {
        let doc = MemoryDocument::default();
        assert!(doc.is_empty());
        assert!(doc.render().is_empty());
    }

    #[test]
    fn apply_set_replaces_field() {
        let mut doc = MemoryDocument::default();
        doc.apply(&parse_update(&json!({ "goal": op("set", "first") })).unwrap());
        doc.apply(&parse_update(&json!({ "goal": op("set", "second") })).unwrap());
        assert_eq!(doc.goal, "second");
    }

    #[test]
    fn apply_append_adds_to_field() {
        let mut doc = MemoryDocument::default();
        doc.apply(&parse_update(&json!({ "decisions": op("set", "decision A") })).unwrap());
        doc.apply(&parse_update(&json!({ "decisions": op("append", "decision B") })).unwrap());
        assert_eq!(doc.decisions, "decision A\ndecision B");
    }

    #[test]
    fn apply_clear_empties_field() {
        let mut doc = MemoryDocument::default();
        doc.apply(&parse_update(&json!({ "goal": op("set", "some goal") })).unwrap());
        doc.apply(&parse_update(&json!({ "goal": op("clear", "") })).unwrap());
        assert!(doc.goal.is_empty());
        assert!(doc.is_empty());
    }

    #[test]
    fn apply_no_update_leaves_document_unchanged() {
        let mut doc = MemoryDocument::default();
        doc.apply(&parse_update(&json!({ "goal": op("set", "set goal") })).unwrap());
        doc.apply(&parse_update(&json!({ "no_update": { "reason": "nothing" } })).unwrap());
        assert_eq!(doc.goal, "set goal");
    }

    #[test]
    fn memory_document_from_events_replays_in_order() {
        let events = vec![
            Event::MemoryDigest(parse_update(&json!({ "goal": op("set", "v1") })).unwrap()),
            Event::MemoryDigest(parse_update(&json!({ "goal": op("append", "v2") })).unwrap()),
            Event::MemoryDigest(
                parse_update(&json!({ "no_update": { "reason": "skip" } })).unwrap(),
            ),
            Event::MemoryDigest(parse_update(&json!({ "stuck": op("clear", "") })).unwrap()),
        ];
        let doc = memory_document_from_events(&events);
        assert_eq!(doc.goal, "v1\nv2");
    }

    // -- render ------------------------------------------------------------

    #[test]
    fn render_omits_empty_fields() {
        let doc = MemoryDocument {
            goal: "the goal".to_string(),
            stuck: "blocked".to_string(),
            ..Default::default()
        };
        let rendered = doc.render();
        assert!(rendered.contains("## goal\nthe goal"));
        assert!(rendered.contains("## stuck\nblocked"));
        assert!(!rendered.contains("decisions"));
        assert!(!rendered.contains("completed"));
    }

    // -- execute_auto ------------------------------------------------------

    #[test]
    fn execute_auto_returns_none_for_other_tools() {
        assert!(execute_auto("fs.read", &json!({})).is_none());
    }

    #[test]
    fn execute_auto_confirms_updated_fields() {
        let result = execute_auto(
            TOOL_ID,
            &json!({ "goal": op("set", "x"), "decisions": op("append", "y") }),
        )
        .expect("handled");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["fields_updated"], json!(["goal", "decisions"]));
    }

    #[test]
    fn execute_auto_confirms_no_update() {
        let result = execute_auto(
            TOOL_ID,
            &json!({ "no_update": { "reason": "nothing changed" } }),
        )
        .expect("handled");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["no_update"], json!(true));
    }

    #[test]
    fn execute_auto_returns_error_for_invalid_input() {
        let result = execute_auto(TOOL_ID, &json!({})).expect("handled");
        assert_eq!(result["is_error"], json!(true));
    }
}
