//! `board.read` / `board.comment` tool dispatch — the board keeper's (and any
//! future board-aware role's) interface to the task board.
//!
//! **The seam.** This crate cannot depend on `horizon-board` (owner decision —
//! see `docs/board-keeper-design.md` §1), so board operations go through
//! [`BoardHost`]: a daemon-provided capability handle, installed on
//! `ToolSessionState` at session construction exactly like
//! [`ExplorationHost`](crate::tools::ExplorationHost) already is. The daemon
//! (`horizon-agentd`) implements `BoardHost` using `horizon_board::Store`; this
//! crate never touches the board store or its types directly.
//!
//! **Permissions.** Both tools are `AutoAllowRead`: `board.read` is a read,
//! and `board.comment` is an append-only write whose audit trail is the board
//! event log itself (same reasoning as `knowledge.write`). Structural
//! enforcement of "comments only" happens at the role level: the keeper role's
//! `allowed_tool_ids` lists `board.comment` but no board state-mutation tool
//! (`board.set_status`, `board.assign`, etc.) — and those tools do not exist
//! in the catalog at all, so no role can express them.

use serde_json::{json, Value};

use crate::contract::{Event, SessionId, SessionState, ToolCallRequest, ToolCallResult};
use crate::tools::error_output;
use crate::tools::state::ToolSessionState;
use crate::tools::Execution;

pub(crate) fn report_schema() -> Value {
    let strings = json!({"type":"array","items":{"type":"string"}});
    let decision = json!({"type":"object","additionalProperties":false,
        "required":["key","question","context","recommendation","consequence","affected_tasks"],
        "properties":{"key":{"type":"string"},"question":{"type":"string"},"context":{"type":"string"},
            "recommendation":{"type":"string"},"consequence":{"type":"string"},"affected_tasks":strings}});
    let task = json!({"type":"object","additionalProperties":false,
        "required":["key","title","instructions","acceptance","depends_on","scope"],
        "properties":{"key":{"type":"string"},"item_id":{"type":["integer","null"]},"title":{"type":"string"},
            "instructions":{"type":"string"},"acceptance":strings,"depends_on":strings,"retry":{"type":"boolean"},
            "scope":{"type":"object","additionalProperties":false,"required":["paths","functions"],
                "properties":{"paths":strings,"functions":strings}}}});
    let evidence = json!({"type":"object","additionalProperties":false,
        "required":["criterion","detail","satisfied","decision","check"],
        "properties":{"criterion":{"type":"string"},"detail":{"type":"string"},"satisfied":{"type":"boolean"},
            "decision":{"type":["string","null"]},"check":{"type":["string","null"]}}});
    json!({"type":"object","additionalProperties":false,"required":["id","attempt","report"],
        "properties":{"id":{"type":"integer"},"attempt":{"type":"string"},
            "report":{"type":"object","additionalProperties":false,"required":["kind"],
                "properties":{"kind":{"type":"string","enum":["plan","discussion","task","verification","blocked"]},
                    "summary":{"type":"string"},"checks":strings,"commit":{"type":"string"},"reason":{"type":"string"},
                    "reply":{"type":"string"},"resolution":{"type":["string","null"]},
                    "acceptance":{"type":["array","null"],"items":{"type":"string"}},
                    "plan":{"type":"object","additionalProperties":false,"required":["summary","reason","acceptance","tasks","decisions"],
                        "properties":{"summary":{"type":"string"},"reason":{"type":"string"},"acceptance":strings,
                            "tasks":{"type":"array","items":task},"decisions":{"type":"array","items":decision},
                            "priorities":{"type":"array","items":{"type":"integer"}},"implementation_decisions":strings}},
                    "verification":{"type":"object","additionalProperties":false,"required":["summary","commit","evidence","checks","decisions"],
                        "properties":{"summary":{"type":"string"},"commit":{"type":"string"},
                            "evidence":{"type":"array","items":evidence},"checks":strings,"decisions":{"type":"array","items":decision}}}}}}})
}

/// The daemon capability `board.read` and `board.comment` are built on: read
/// the board (list items or show one) and append a comment. Implemented by
/// `horizon-agentd` (`session::AgentdBoardHost`) using `horizon_board::Store`
/// and installed on each session's `ToolSessionState` via
/// [`ToolSessionState::with_board_host`].
///
/// Methods are synchronous: board reads are file folds, and board writes
/// (comment) go through `horizon-logd` via a tokio round-trip the daemon
/// implementation blocks on internally. The caller (the session thread) is
/// never async, so the trait stays sync — mirroring how `ExplorationHost`
/// presents a sync interface despite the daemon's async internals.
///
/// Returns `serde_json::Value` for reads so this crate never imports
/// `horizon-board`'s `Item`/`ListResult` types. The daemon serializes board
/// types to JSON; the tool executor passes them straight through to the model.
pub trait BoardHost: Send + Sync {
    /// Lists board items in rank order, optionally filtered by status.
    /// Returns a JSON object `{ items: [...], statuses: [...] }`.
    fn list(&self, status_filter: Option<&str>) -> Result<Value, String>;

    /// Shows one item with its full comment thread, or `None` if the id
    /// doesn't exist. Returns a JSON `Item` or `null`.
    fn show(&self, id: u64) -> Result<Value, String>;

    /// Appends a comment to item `id`. `author` is set by the caller (the
    /// daemon, from the session id) — the model never controls the author
    /// field. `Err` carries a message suitable for the model to read.
    fn comment(&self, id: u64, author: &str, text: &str) -> Result<(), String>;

    /// Saves a structured report for an attempt owned by this session.
    fn report(
        &self,
        _id: u64,
        _token: &str,
        _session: &str,
        _report: Value,
    ) -> Result<Value, String> {
        Err("This session has no milestone reporting capability".into())
    }
}

/// Executes an auto-allowed board read tool (`board.read`). Returns `None` for
/// any other tool id, so the caller can try elsewhere — same contract as
/// `tools::knowledge::execute_auto`.
pub(crate) fn execute_auto(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    if tool_id != "board.read" {
        return None;
    }
    let Some(host) = tool_state.board_host() else {
        return Some(error_output(
            "board.read is not available: no board host is installed for this session",
        ));
    };
    // If `id` is present, show that item; otherwise list all (optionally
    // filtered by `status`). One tool, two operations — keeps the role
    // allowlist short and the model's surface simple.
    if let Some(id) = input.get("id").and_then(Value::as_u64) {
        match host.show(id) {
            Ok(value) => Some(value),
            Err(message) => Some(error_output(message)),
        }
    } else {
        let status_filter = input
            .get("status")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        match host.list(status_filter.as_deref()) {
            Ok(value) => Some(value),
            Err(message) => Some(error_output(message)),
        }
    }
}

/// Executes `board.comment` — a write, but `AutoAllowRead` so it skips the
/// approval gate (the event log is the audit trail, same as `knowledge.write`).
/// Special-cased in `execute_agent_tool` (not routed through
/// `execute_auto_tool`) because it needs the session id for the comment author,
/// which `execute_auto_tool`'s signature doesn't carry.
pub(crate) fn execute_comment(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    let Some(host) = tool_state.board_host() else {
        return synchronous(
            request,
            error_output(
                "board.comment is not available: no board host is installed for this session",
            ),
        );
    };
    let Some(id) = request.input.get("id").and_then(Value::as_u64) else {
        return synchronous(
            request,
            error_output("board.comment requires an `id` integer argument"),
        );
    };
    let Some(text) = request.input.get("text").and_then(Value::as_str) else {
        return synchronous(
            request,
            error_output("board.comment requires a `text` string argument"),
        );
    };
    // The author is the session id — the model never controls it, so a
    // keeper session cannot impersonate the owner or another session.
    let author = format!("session:{}", session_id.as_uuid());
    let output = match host.comment(id, &author, text) {
        Ok(()) => json!({ "ok": true }),
        Err(message) => error_output(message),
    };
    synchronous(request, output)
}

/// Builds the `Execution::Auto` event list for a synchronous board tool result.
pub(crate) fn execute_report(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    let result = (|| {
        let host = tool_state
            .board_host()
            .ok_or("No board host is installed")?;
        let id = request
            .input
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("Missing milestone id")?;
        let token = request
            .input
            .get("attempt")
            .and_then(Value::as_str)
            .ok_or("Missing attempt token")?;
        let report = request.input.get("report").ok_or("Missing report")?.clone();
        host.report(id, token, &session_id.as_uuid().to_string(), report)
    })();
    synchronous(request, result.unwrap_or_else(error_output))
}

fn synchronous(request: &ToolCallRequest, output: Value) -> Execution {
    Execution::Auto(vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult::new(
            request.call_id.clone(),
            request.occurrence_id.clone(),
            output,
        )),
    ])
}
