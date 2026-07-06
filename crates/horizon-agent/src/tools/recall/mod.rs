//! `recall.search`/`recall.read`: agent-facing tools over the persisted
//! DuckDB projection (`persistence::projection::duckdb`) -- MemGPT's "agent
//! queries its own external context" recall primitive, as opposed to
//! summarizing it away (see `docs/research/letta.md` §1, §10, and the
//! "(a) compaction"/"(b) DuckDB KB" sections). Both are `AutoAllowRead`:
//! read-only access to this session's (or, with `scope: "all"`, every
//! session's) own already-persisted history -- no different in trust terms
//! from `fs.read`.
//!
//! Each call opens its own short-lived `Store` (see [`Store::open`]) rather
//! than sharing the event-log writer's long-lived one: measured on this
//! machine, same-process `Store::open` calls against one path succeed and
//! share the underlying libduckdb instance cache, so a per-call open here
//! never contends with the writer thread's own long-lived handle (see
//! `docs/agent-duckdb-state-design.md`'s "Runtime Boundary" addendum). Only
//! *cross-process* access (e.g. `duckdb -readonly` from a shell while
//! `horizon-agentd` is running) is blocked -- see the `agent-inspect`
//! skill's updated DuckDB section.

use serde_json::{json, Value};

use crate::contract::SessionId;
use crate::persistence::projection::duckdb::{RecallEntry, Store};
use crate::tools::state::ToolSessionState;

const DEFAULT_SEARCH_LIMIT: usize = 20;
const DEFAULT_READ_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
/// Half-width (in characters) of the context window built around a search
/// hit's first match by [`snippet_around_match`] -- combined with the match
/// itself this yields a snippet of roughly 200 characters, per the task
/// brief's "~200 chars with ellipses".
const SNIPPET_RADIUS_CHARS: usize = 100;
/// Total character budget for one `recall.read` response, across every
/// returned entry's `text` combined -- mirrors `tools::bash`'s in-context
/// output cap (`docs/agent-tools-design.md`): a session's own persisted
/// history can be arbitrarily large, and pulling too much of it back into
/// context in one call would defeat the point of recall (bounded lookups,
/// not re-inlining everything).
const READ_TOTAL_CHAR_CAP: usize = 16_000;

/// Executes `recall.search`/`recall.read` if `tool_id` names one of them,
/// mirroring `tools::fs`/`tools::config`'s `execute_auto` contract: `None`
/// for any other tool id, so `execution::execute_auto_tool`'s chain can try
/// elsewhere.
pub(crate) fn execute_auto(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    match tool_id {
        "recall.search" => Some(search(tool_state, input)),
        "recall.read" => Some(read(tool_state, input)),
        _ => None,
    }
}

fn search(tool_state: &ToolSessionState, input: &Value) -> Value {
    let Some(query) = input.get("query").and_then(Value::as_str) else {
        return error_output("recall.search requires a `query` string argument");
    };
    let scope_arg = input
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let limit = clamp_limit(
        input.get("limit").and_then(Value::as_u64),
        DEFAULT_SEARCH_LIMIT,
    );

    let recall = tool_state.recall_context();
    let Some(duckdb_path) = recall.duckdb_path.as_deref() else {
        return error_output(
            "recall is unavailable: no persisted history database is configured for this session",
        );
    };

    let scope = match scope_arg {
        "all" => None,
        "session" => match recall.session_id {
            Some(session_id) => Some(session_id),
            None => {
                return error_output(
                    "recall.search scope \"session\" requires a session id, but this session \
                     has none configured -- pass scope: \"all\" instead",
                )
            }
        },
        other => {
            return error_output(format!(
                "recall.search: unknown scope `{other}` (expected \"session\" or \"all\")"
            ))
        }
    };

    let store = match Store::open(duckdb_path) {
        Ok(store) => store,
        Err(error) => return error_output(format!("recall is unavailable: {error}")),
    };
    let report = match store.search_history(scope, query, limit) {
        Ok(report) => report,
        Err(error) => return error_output(format!("recall.search failed: {error}")),
    };

    let own_session_id = recall.session_id;
    let hits = report
        .hits
        .into_iter()
        .map(|hit| hit_json(hit, query, own_session_id))
        .collect::<Vec<_>>();

    json!({
        "total": report.total,
        "hits": hits,
    })
}

fn hit_json(entry: RecallEntry, query: &str, own_session_id: Option<SessionId>) -> Value {
    json!({
        "session_id": session_id_json(entry.session_id),
        "own_session": Some(entry.session_id) == own_session_id,
        "sequence": entry.sequence,
        "kind": entry.kind.as_str(),
        "role_or_tool": entry.role_or_tool,
        "snippet": snippet_around_match(&entry.text, query),
        "at": entry.at,
    })
}

fn read(tool_state: &ToolSessionState, input: &Value) -> Value {
    let recall = tool_state.recall_context();
    let Some(duckdb_path) = recall.duckdb_path.as_deref() else {
        return error_output(
            "recall is unavailable: no persisted history database is configured for this session",
        );
    };

    let session_id = match input.get("session_id").and_then(Value::as_str) {
        Some(raw) => match parse_session_id(raw) {
            Ok(session_id) => session_id,
            Err(_) => return error_output(format!("recall.read: invalid session_id `{raw}`")),
        },
        None => match recall.session_id {
            Some(session_id) => session_id,
            None => {
                return error_output(
                    "recall.read requires a session_id (this session has none configured in \
                     context)",
                )
            }
        },
    };

    let Some(from_sequence) = input.get("from_sequence").and_then(Value::as_i64) else {
        return error_output("recall.read requires a `from_sequence` integer argument");
    };
    let limit = clamp_limit(
        input.get("limit").and_then(Value::as_u64),
        DEFAULT_READ_LIMIT,
    );

    let store = match Store::open(duckdb_path) {
        Ok(store) => store,
        Err(error) => return error_output(format!("recall is unavailable: {error}")),
    };
    let entries = match store.read_history_window(session_id, from_sequence, limit) {
        Ok(entries) => entries,
        Err(error) => return error_output(format!("recall.read failed: {error}")),
    };

    let mut used_chars = 0usize;
    let mut truncated = false;
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut text = entry.text;
        let text_char_count = text.chars().count();
        if used_chars + text_char_count > READ_TOTAL_CHAR_CAP {
            truncated = true;
            let remaining = READ_TOTAL_CHAR_CAP.saturating_sub(used_chars);
            if remaining == 0 {
                // Nothing left in the budget at all -- stop before adding
                // an empty-text row rather than padding the output with one.
                break;
            }
            text = text.chars().take(remaining).collect();
        }
        used_chars += text.chars().count();
        rows.push(json!({
            "sequence": entry.sequence,
            "kind": entry.kind.as_str(),
            "role_or_tool": entry.role_or_tool,
            "text": text,
            "at": entry.at,
        }));
        if truncated {
            break;
        }
    }

    let mut output = json!({ "entries": rows });
    if truncated {
        output["note"] = json!(format!(
            "output truncated at ~{READ_TOTAL_CHAR_CAP} characters; call recall.read again with \
             a later from_sequence to continue"
        ));
    }
    output
}

fn parse_session_id(raw: &str) -> Result<SessionId, serde_json::Error> {
    serde_json::from_value(Value::String(raw.to_string()))
}

fn session_id_json(session_id: SessionId) -> Value {
    serde_json::to_value(session_id).unwrap_or(Value::Null)
}

fn clamp_limit(raw: Option<u64>, default: usize) -> usize {
    raw.map(|value| value as usize)
        .unwrap_or(default)
        .clamp(1, MAX_LIMIT)
}

/// Builds a snippet of roughly [`SNIPPET_RADIUS_CHARS`] * 2 characters
/// centered on the first case-insensitive match of `query` in `text`, with
/// ellipses at whichever end was actually trimmed. Falls back to the start
/// of `text` if `query` isn't found there (shouldn't happen for a
/// `search_history` hit, since the SQL layer already matched it, but `text`
/// here is that query's SQL-bounded substring, which -- extremely rarely,
/// for a match deep past the bound -- might not contain it) so this never
/// panics or returns an empty string.
///
/// Works in char counts throughout (never raw byte offsets into `text`) to
/// stay UTF-8-safe: `to_lowercase()` can change a string's *byte* length for
/// a handful of expanding case folds (e.g. German sharp-S), so the match's
/// byte offset in the lowercased text is converted to a *char* count before
/// it's ever used to index into `text`'s own `chars()`. That conversion
/// assumes case folding doesn't change the character *count* up to the
/// match, which holds for ASCII and for CJK text (where `to_lowercase` is a
/// no-op) -- the rare expanding-fold case only nudges the snippet window by
/// a character or two, never panics or corrupts it.
fn snippet_around_match(text: &str, query: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_query = query.to_lowercase();

    let match_byte = if lower_query.is_empty() {
        0
    } else {
        lower_text.find(&lower_query).unwrap_or(0)
    };
    let match_char_index = lower_text[..match_byte].chars().count();
    let query_char_len = lower_query.chars().count();

    let chars: Vec<char> = text.chars().collect();
    let start = match_char_index
        .saturating_sub(SNIPPET_RADIUS_CHARS)
        .min(chars.len());
    let end = (match_char_index + query_char_len + SNIPPET_RADIUS_CHARS).min(chars.len());
    let end = end.max(start);

    let mut snippet: String = chars[start..end].iter().collect();
    if start > 0 {
        snippet = format!("...{snippet}");
    }
    if end < chars.len() {
        snippet.push_str("...");
    }
    snippet
}

fn error_output(message: impl Into<String>) -> Value {
    json!({ "is_error": true, "message": message.into() })
}

#[cfg(test)]
mod tests;
