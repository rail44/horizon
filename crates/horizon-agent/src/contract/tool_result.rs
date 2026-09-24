//! Tool result envelope and the persisted meaning of an abandoned retry.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{JsonValue, OccurrenceId, ToolCallId};

pub(crate) const SUPERSEDED_BY_RETRY: &str = "superseded_by_retry";

pub(crate) fn is_superseded_output(output: &Value) -> bool {
    output.get(SUPERSEDED_BY_RETRY).and_then(Value::as_bool) == Some(true)
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    /// Per-occurrence identity -- see [`OccurrenceId`]. Mirrors the
    /// `ToolCallRequest` field it answers to.
    pub occurrence_id: Option<OccurrenceId>,
    pub output: JsonValue,
    /// Explicit success/failure outcome, lifted out of `output`'s
    /// `"is_error"` JSON convention (every tool in `tools::` already writes
    /// it on failure -- `docs/agent-feedback-design.md`'s decision 1;
    /// denial constructors also set it directly) so consumers such as the
    /// turn-receipts UI (`docs/agent-output-ui-amendment.md`'s 2026-07-12
    /// addendum) have a typed field instead of having to sniff `output`
    /// itself. Use [`Self::new`] rather than a struct literal to keep this
    /// derived automatically.
    pub is_error: bool,
    /// Explicit marker for a user's tool-call denial, set only by
    /// [`Self::denied`] (used by `tools::approval::synchronous_result`'s
    /// `ran = false` path -- the deny arms of `resolve_synchronous_tool`/
    /// `resolve_bash` in `crates/horizon-agent/src/tools/approval.rs`).
    /// Replaces the old convention of a consumer sniffing `output` for
    /// `denied_output`'s exact `{"is_error": true, "message": "denied by
    /// user"}` shape -- documented as brittle when that convention shipped
    /// (`docs/agent-output-ui-amendment.md`'s round 3 note) since it
    /// couldn't distinguish "the field happens to read that way" from "this
    /// is contractually a denial".
    pub denied: bool,
}

impl ToolCallResult {
    /// Builds a result with `is_error` derived from `output`'s `"is_error"`
    /// convention -- see the field's own doc comment. The single
    /// constructor every production call site should go through, so the
    /// convention lives in one place rather than being re-checked (or
    /// forgotten) at each tool.
    ///
    /// `occurrence_id` is the per-occurrence identity from the originating
    /// `ToolCallRequest` (see [`OccurrenceId`]); `None` is acceptable when
    /// the originating request is not in scope (replayed logs, synthetic
    /// results). `transcript::tool_call::build_tool_call_views` matches
    /// `ToolCallFinished` events back to their `Building` entry by
    /// `occurrence_id` first, falling back to call_id + position -- so a
    /// `None` here does not break the transcript, it just removes the
    /// per-occurrence attribution that an older or synthetic event
    /// doesn't carry.
    pub fn new(
        call_id: ToolCallId,
        occurrence_id: Option<OccurrenceId>,
        output: impl Into<JsonValue>,
    ) -> Self {
        let output = output.into();
        let is_error = output
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Self {
            call_id,
            occurrence_id,
            output,
            is_error,
            denied: false,
        }
    }

    /// Builds a result for a user's tool-call denial -- see the `denied`
    /// field's own doc comment. Always `is_error: true` (a denial is
    /// definitionally a failure), regardless of what `output` itself
    /// carries. `occurrence_id` is forwarded the same way as [`Self::new`].
    pub(crate) fn denied(
        call_id: ToolCallId,
        occurrence_id: Option<OccurrenceId>,
        output: impl Into<JsonValue>,
    ) -> Self {
        Self {
            denied: true,
            is_error: true,
            ..Self::new(call_id, occurrence_id, output)
        }
    }
    /// Close this abandoned attempt when an approved retry replaces it. The
    /// marker is persisted for transcript/replay consumers, never sent as the
    /// provider's answer. The replacement attempt supplies that answer later.
    pub(crate) fn superseded_by_retry(&self, retry_occurrence: Option<&OccurrenceId>) -> Self {
        Self::new(
            self.call_id.clone(),
            self.occurrence_id.clone(),
            serde_json::json!({
                SUPERSEDED_BY_RETRY: true,
                "retry_occurrence_id": retry_occurrence.map(|occurrence| occurrence.0.as_str()),
                "message": "this attempt was abandoned; an approved retry of the same call replaced it",
            }),
        )
    }
}
