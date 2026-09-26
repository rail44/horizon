use super::bounded::{Object, Text};
use crate::contract::{FoldedLogRange, MemoryDigest, MemoryField, MemoryFieldUpdate, MemoryOp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum FieldEdit {
    Set {
        content: Text,
    },
    Append {
        content: Text,
    },
    Clear {
        /// Ignored for clear.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LogRange {
    pub from_seq: u64,
    pub to_seq: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoUpdate {
    pub reason: Text,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryUpdate {
    /// The overarching goal this session serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Object<FieldEdit>>,
    /// Decisions and their rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Object<FieldEdit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<Object<FieldEdit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_progress: Option<Object<FieldEdit>>,
    /// Blocked or unresolved work, and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck: Option<Object<FieldEdit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<Object<FieldEdit>>,
    /// Relevant files, paths and symbols.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related: Option<Object<FieldEdit>>,
    /// Raw event-log sequence range condensed by this update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded_log_range: Option<Object<LogRange>>,
    /// Instead of field edits, explain why no update is needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_update: Option<Object<NoUpdate>>,
}

impl MemoryUpdate {
    pub(crate) fn digest(&self) -> Result<MemoryDigest, String> {
        let fields = [
            (MemoryField::Goal, &self.goal),
            (MemoryField::Decisions, &self.decisions),
            (MemoryField::Completed, &self.completed),
            (MemoryField::InProgress, &self.in_progress),
            (MemoryField::Stuck, &self.stuck),
            (MemoryField::NextStep, &self.next_step),
            (MemoryField::Related, &self.related),
        ];
        let updates: Vec<_> = fields
            .into_iter()
            .filter_map(|(field, edit)| {
                let (op, content) = match &**edit.as_ref()? {
                    FieldEdit::Set { content } => (MemoryOp::Set, content.trim().to_string()),
                    FieldEdit::Append { content } => (MemoryOp::Append, content.trim().to_string()),
                    FieldEdit::Clear { .. } => (MemoryOp::Clear, String::new()),
                };
                Some(MemoryFieldUpdate { field, op, content })
            })
            .collect();
        if let Some(no_update) = &self.no_update {
            if !updates.is_empty() || self.folded_log_range.is_some() {
                return Err(
                    "cannot combine no_update with field operations or folded_log_range".into(),
                );
            }
            return Ok(MemoryDigest {
                updates,
                folded_log_range: None,
                no_update_reason: Some(no_update.reason.trim().to_string()),
            });
        }
        if updates.is_empty() {
            return Err(
                "provide at least one field operation or declare no_update with a reason".into(),
            );
        }
        if self
            .folded_log_range
            .as_ref()
            .is_some_and(|range| range.from_seq > range.to_seq)
        {
            return Err("folded_log_range.from_seq must not exceed to_seq".into());
        }
        Ok(MemoryDigest {
            updates,
            folded_log_range: self.folded_log_range.as_ref().map(|range| FoldedLogRange {
                from_seq: range.from_seq,
                to_seq: range.to_seq,
            }),
            no_update_reason: None,
        })
    }
}
