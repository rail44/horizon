use anyhow::{Context, Result};
use duckdb::{params, params_from_iter, types::Value as DuckValue};
#[cfg(test)]
use serde_json::Value;

use crate::contract::ProviderId;
use crate::contract::SessionId;
#[cfg(test)]
use crate::contract::{MessageRole, ToolCallId};
#[cfg(test)]
use crate::frame::{agent_frame_from_events, AgentFrame};
use crate::roles::RoleId;

use super::{
    session_id_text, AgentStoredEvent, RecallEntry, RecallEntryKind, RecallSearchReport, Store,
};
#[cfg(test)]
use super::{
    AgentStoredApproval, AgentStoredMessage, AgentStoredSession, AgentStoredSessionSnapshot,
    AgentStoredToolCall, AgentStoredToolResult, AgentStoredTurn,
};

/// How many characters of `text`/`input_json`/`output_json` `search_history`
/// and `read_history_window` pull out of the database per row, via SQL
/// `substr` -- *before* Rust ever sees the row. This is what stops a single
/// huge tool result from ballooning a recall response regardless of the
/// caller's own `limit`; `tools::recall` trims further (a ~200-char search
/// snippet, a ~16k-char total cap for `recall.read`) on top of this.
const RECALL_TEXT_BOUND_CHARS: usize = 4_000;

impl Store {
    /// Test-only: both current callers (`session_snapshots` below and
    /// `projection.rs`'s `rebuild_projections`) are themselves `cfg(test)`,
    /// and this crate's own tests assert what actually landed in the
    /// projection after a rebuild or a live append.
    #[cfg(test)]
    pub(crate) fn sessions(&self) -> Result<Vec<AgentStoredSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, provider_id, role_id, last_sequence, updated_at::TEXT
             FROM agent_sessions
             ORDER BY updated_at DESC, session_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AgentStoredSession {
                session_id: parse_session_id_column(0, &row.get::<_, String>(0)?)?,
                provider_id: row.get::<_, Option<String>>(1)?.map(ProviderId),
                role_id: row.get::<_, Option<String>>(2)?.map(RoleId),
                last_sequence: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent sessions")
    }

    /// The projection's high-water mark: the largest `last_sequence` any
    /// session has recorded, or `None` if `agent_sessions` is empty. Not
    /// test-only: `horizon-sessiond`'s startup rebuild-skip check (task 2 of
    /// the readiness fix) compares this against the event log's own final
    /// record sequence to decide whether the projection is already current
    /// -- cheap enough to run before every rebuild decision since it's a
    /// single aggregate over a small table, not a scan of `agent_events`.
    pub(crate) fn max_last_sequence(&self) -> Result<Option<i64>> {
        self.conn
            .query_row("SELECT MAX(last_sequence) FROM agent_sessions", [], |row| {
                row.get(0)
            })
            .context("query max agent session sequence")
    }

    #[cfg(test)]
    pub(crate) fn session_snapshots(&self) -> Result<Vec<AgentStoredSessionSnapshot>> {
        self.sessions()?
            .into_iter()
            .map(|session| {
                let session_id = session.session_id;
                Ok(AgentStoredSessionSnapshot {
                    session,
                    frame: self.frame_for_session(session_id)?,
                    message_count: self.messages_for_session(session_id)?.len(),
                    tool_call_count: self.tool_calls_for_session(session_id)?.len(),
                    approval_count: self.approvals_for_session(session_id)?.len(),
                })
            })
            .collect()
    }

    pub(crate) fn events_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredEvent>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT
                event_id,
                turn_id,
                sequence,
                event_kind,
                horizon_event_json,
                provider_id,
                role_id,
                provider_payload_json
             FROM agent_events
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            let event_json: String = row.get(4)?;
            let provider_payload_json: Option<String> = row.get(7)?;
            Ok(AgentStoredEvent {
                event_id: row.get(0)?,
                session_id,
                turn_id: row.get(1)?,
                sequence: row.get(2)?,
                event_kind: row.get(3)?,
                event: serde_json::from_str(&event_json).map_err(|err| {
                    duckdb::Error::FromSqlConversionFailure(
                        4,
                        duckdb::types::Type::Text,
                        Box::new(err),
                    )
                })?,
                provider_id: row.get::<_, Option<String>>(5)?.map(ProviderId),
                role_id: row.get::<_, Option<String>>(6)?.map(RoleId),
                provider_payload: provider_payload_json
                    .map(|json| {
                        serde_json::from_str(&json).map_err(|err| {
                            duckdb::Error::FromSqlConversionFailure(
                                7,
                                duckdb::types::Type::Text,
                                Box::new(err),
                            )
                        })
                    })
                    .transpose()?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent events")
    }

    #[cfg(test)]
    pub(crate) fn frame_for_session(&self, session_id: SessionId) -> Result<AgentFrame> {
        let events = self
            .events_for_session(session_id)?
            .into_iter()
            .map(|record| record.event)
            .collect::<Vec<_>>();
        Ok(agent_frame_from_events(&events))
    }

    #[cfg(test)]
    pub(crate) fn messages_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredMessage>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, role, text, is_delta
             FROM agent_messages
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            Ok(AgentStoredMessage {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                role: parse_role(row.get::<_, String>(2)?.as_str()),
                text: row.get(3)?,
                is_delta: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent messages")
    }

    #[cfg(test)]
    pub(crate) fn tool_calls_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredToolCall>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, call_id, tool_id, input_json
             FROM agent_tool_calls
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            let input_json: String = row.get(4)?;
            Ok(AgentStoredToolCall {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                call_id: ToolCallId(row.get(2)?),
                tool_id: row.get(3)?,
                input: parse_json_column(4, &input_json)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent tool calls")
    }

    #[cfg(test)]
    pub(crate) fn tool_results_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredToolResult>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, call_id, output_json, is_error
             FROM agent_tool_results
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            let output_json: String = row.get(3)?;
            Ok(AgentStoredToolResult {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                call_id: ToolCallId(row.get(2)?),
                output: parse_json_column(3, &output_json)?,
                is_error: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent tool results")
    }

    /// Case-insensitive substring search over this store's durable,
    /// *committed* history: message text (`agent_messages` where `NOT
    /// is_delta` -- streaming reasoning/assistant-text deltas are never
    /// searched, only what actually got committed), tool-call `input_json`,
    /// and tool-result `output_json`. `scope` restricts the search to one
    /// session; `None` searches every persisted session. `turn_outcome`, if
    /// `Some`, restricts hits to events whose enclosing turn ended with
    /// that `agent_turns.end_reason` (`"completed"`/`"cancelled"`/
    /// `"failed"`/`"halted"`) -- see `docs/agent-feedback-design.md`'s
    /// decision 1 and decision 3 (recall scope includes labels). Callers
    /// validate the value before it reaches here (`tools::recall`); this
    /// method does not reject an unrecognized string itself, it just
    /// matches nothing.
    ///
    /// `query`, if `Some`, is escaped for `%`/`_`/the escape character
    /// itself (see [`escape_like_pattern`]) before being wrapped in a
    /// `%...%` `ILIKE` pattern, so a literal `%` or `_` in a user's search
    /// term matches itself literally rather than acting as a wildcard. If
    /// `None`, the `ILIKE` predicate is skipped entirely on every branch --
    /// this is "listing mode": every row matching `scope`/`turn_outcome`
    /// alone, newest-first, for mining recipes like "list how recent work
    /// ended" that have no substring to center on. Callers are expected to
    /// pair `query: None` with a `turn_outcome` filter (`tools::recall`
    /// enforces this); listing mode with neither would return this store's
    /// entire matched history unfiltered. Each hit's `text` is bounded to
    /// [`RECALL_TEXT_BOUND_CHARS`] *at the SQL layer* (`substr`) before it
    /// ever reaches Rust, so a single huge tool result can't balloon a
    /// response regardless of `limit`.
    ///
    /// Rows are newest-first (`event_at` then `sequence`, both descending).
    /// [`RecallSearchReport::total`] counts every match, not just the
    /// `limit`-bounded rows actually returned -- computed via `COUNT(*)
    /// OVER ()` over the unlimited match set, before the `LIMIT` clause
    /// trims it.
    pub(crate) fn search_history(
        &self,
        scope: Option<SessionId>,
        query: Option<&str>,
        limit: usize,
        turn_outcome: Option<&str>,
    ) -> Result<RecallSearchReport> {
        let pattern = query.map(|query| format!("%{}%", escape_like_pattern(query)));
        let scope_session_id = scope.map(session_id_text).transpose()?;
        // Each UNION ALL branch needs its own alias-qualified scope clause
        // (`m.session_id`/`c.session_id`/`r.session_id`) -- `agent_events`
        // (joined into every branch for `event_at`) has its own
        // `session_id` column too, so an unqualified `session_id = ?` is
        // ambiguous once that join is in play.
        let scope_clause_for = |alias: &str| -> String {
            if scope_session_id.is_some() {
                format!("AND {alias}.session_id = ?")
            } else {
                String::new()
            }
        };
        // In listing mode (`pattern` is `None`, i.e. `query` was omitted --
        // see the doc comment above), every branch's `ILIKE` predicate
        // collapses to `TRUE`, leaving only the (optional) scope/delta
        // filters -- no placeholder is emitted, and none is bound for it
        // below.
        let text_predicate_for = |alias: &str, column: &str| -> String {
            if pattern.is_some() {
                format!("{alias}.{column} ILIKE ? ESCAPE '\\'")
            } else {
                "TRUE".to_string()
            }
        };
        let turn_outcome_clause = if turn_outcome.is_some() {
            "WHERE t.end_reason = ?"
        } else {
            ""
        };

        let sql = format!(
            "WITH matches AS (
                SELECT m.session_id AS session_id, m.sequence AS sequence,
                       'message' AS kind, m.role AS role_or_tool,
                       substr(m.text, 1, {bound}) AS text,
                       e.event_at::TEXT AS at_ts, e.turn_id AS turn_id,
                       CAST(NULL AS BOOLEAN) AS is_error
                FROM agent_messages m
                JOIN agent_events e ON e.event_id = m.event_id
                WHERE NOT m.is_delta AND {predicate_m} {scope_m}
                UNION ALL
                SELECT c.session_id, c.sequence, 'tool_call', c.tool_id,
                       substr(c.input_json, 1, {bound}), e.event_at::TEXT, e.turn_id,
                       CAST(NULL AS BOOLEAN)
                FROM agent_tool_calls c
                JOIN agent_events e ON e.event_id = c.event_id
                WHERE {predicate_c} {scope_c}
                UNION ALL
                SELECT r.session_id, r.sequence, 'tool_result',
                       COALESCE(tc.tool_id, r.call_id),
                       substr(r.output_json, 1, {bound}), e.event_at::TEXT, e.turn_id,
                       r.is_error
                FROM agent_tool_results r
                JOIN agent_events e ON e.event_id = r.event_id
                LEFT JOIN agent_tool_calls tc
                    ON tc.call_id = r.call_id AND tc.session_id = r.session_id
                WHERE {predicate_r} {scope_r}
            )
            SELECT matches.session_id, matches.sequence, matches.kind, matches.role_or_tool,
                   matches.text, matches.at_ts, matches.is_error, t.end_reason,
                   COUNT(*) OVER () AS total
            FROM matches
            LEFT JOIN agent_turns t
                ON t.session_id = matches.session_id AND t.turn_id = matches.turn_id
            {turn_outcome_clause}
            ORDER BY at_ts DESC, sequence DESC
            LIMIT ?",
            bound = RECALL_TEXT_BOUND_CHARS,
            predicate_m = text_predicate_for("m", "text"),
            predicate_c = text_predicate_for("c", "input_json"),
            predicate_r = text_predicate_for("r", "output_json"),
            scope_m = scope_clause_for("m"),
            scope_c = scope_clause_for("c"),
            scope_r = scope_clause_for("r"),
        );

        let mut bind_values: Vec<DuckValue> = Vec::new();
        for _ in 0..3 {
            if let Some(pattern) = &pattern {
                bind_values.push(DuckValue::Text(pattern.clone()));
            }
            if let Some(scope_session_id) = &scope_session_id {
                bind_values.push(DuckValue::Text(scope_session_id.clone()));
            }
        }
        if let Some(turn_outcome) = turn_outcome {
            bind_values.push(DuckValue::Text(turn_outcome.to_string()));
        }
        bind_values.push(DuckValue::BigInt(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let mut total = 0usize;
        let rows = stmt.query_map(params_from_iter(bind_values), |row| {
            total = row.get::<_, i64>(8)? as usize;
            Ok(RecallEntry {
                session_id: parse_session_id_column(0, &row.get::<_, String>(0)?)?,
                sequence: row.get(1)?,
                kind: parse_recall_kind(&row.get::<_, String>(2)?)?,
                role_or_tool: row.get(3)?,
                text: row.get(4)?,
                at: row.get(5)?,
                is_error: row.get(6)?,
                turn_outcome: row.get(7)?,
            })
        })?;

        let hits = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("query recall search history")?;
        Ok(RecallSearchReport { hits, total })
    }

    /// Committed messages, tool calls, and tool results for one session,
    /// from `from_sequence` onward (inclusive), ordered ascending by
    /// sequence and limited to `limit` rows -- the "read the full context
    /// around a hit" counterpart to [`Self::search_history`]. Each entry's
    /// text is bounded the same way (see [`RECALL_TEXT_BOUND_CHARS`]).
    pub(crate) fn read_history_window(
        &self,
        session_id: SessionId,
        from_sequence: i64,
        limit: usize,
    ) -> Result<Vec<RecallEntry>> {
        let session_id_text = session_id_text(session_id)?;
        let sql = format!(
            "WITH history_window AS (
                SELECT m.session_id AS session_id, m.sequence AS sequence,
                       'message' AS kind, m.role AS role_or_tool,
                       substr(m.text, 1, {bound}) AS text,
                       e.event_at::TEXT AS at_ts,
                       CAST(NULL AS BOOLEAN) AS is_error
                FROM agent_messages m
                JOIN agent_events e ON e.event_id = m.event_id
                WHERE NOT m.is_delta AND m.session_id = ? AND m.sequence >= ?
                UNION ALL
                SELECT c.session_id, c.sequence, 'tool_call', c.tool_id,
                       substr(c.input_json, 1, {bound}), e.event_at::TEXT,
                       CAST(NULL AS BOOLEAN)
                FROM agent_tool_calls c
                JOIN agent_events e ON e.event_id = c.event_id
                WHERE c.session_id = ? AND c.sequence >= ?
                UNION ALL
                SELECT r.session_id, r.sequence, 'tool_result',
                       COALESCE(tc.tool_id, r.call_id),
                       substr(r.output_json, 1, {bound}), e.event_at::TEXT,
                       r.is_error
                FROM agent_tool_results r
                JOIN agent_events e ON e.event_id = r.event_id
                LEFT JOIN agent_tool_calls tc
                    ON tc.call_id = r.call_id AND tc.session_id = r.session_id
                WHERE r.session_id = ? AND r.sequence >= ?
            )
            SELECT sequence, kind, role_or_tool, text, at_ts, is_error
            FROM history_window
            ORDER BY sequence ASC
            LIMIT ?",
            bound = RECALL_TEXT_BOUND_CHARS,
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                &session_id_text,
                from_sequence,
                &session_id_text,
                from_sequence,
                &session_id_text,
                from_sequence,
                limit as i64,
            ],
            |row| {
                Ok(RecallEntry {
                    session_id,
                    sequence: row.get(0)?,
                    kind: parse_recall_kind(&row.get::<_, String>(1)?)?,
                    role_or_tool: row.get(2)?,
                    text: row.get(3)?,
                    at: row.get(4)?,
                    is_error: row.get(5)?,
                    turn_outcome: None,
                })
            },
        )?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("query recall history window")
    }

    #[cfg(test)]
    pub(crate) fn approvals_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredApproval>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, call_id, reason, outcome
             FROM agent_approvals
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            Ok(AgentStoredApproval {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                call_id: ToolCallId(row.get(2)?),
                reason: row.get(3)?,
                outcome: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent approvals")
    }

    #[cfg(test)]
    pub(crate) fn turns_for_session(&self, session_id: SessionId) -> Result<Vec<AgentStoredTurn>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT turn_id, end_reason, ended_event_id
             FROM agent_turns
             WHERE session_id = ?
             ORDER BY turn_id",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            Ok(AgentStoredTurn {
                session_id,
                turn_id: row.get(0)?,
                end_reason: row.get(1)?,
                ended_event_id: row.get(2)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent turns")
    }
}

#[cfg(test)]
fn parse_role(value: &str) -> MessageRole {
    match value {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        _ => MessageRole::Assistant,
    }
}

fn parse_session_id_column(column: usize, value: &str) -> duckdb::Result<SessionId> {
    let json = serde_json::Value::String(value.to_string());
    serde_json::from_value(json).map_err(|err| {
        duckdb::Error::FromSqlConversionFailure(column, duckdb::types::Type::Text, Box::new(err))
    })
}

/// Escapes `%`, `_`, and the escape character itself (`\`) in `text` for
/// safe embedding in an `ILIKE ... ESCAPE '\'` pattern -- turns those into
/// literal characters to match instead of wildcards. Used by
/// [`Store::search_history`] so a user's raw substring query (which may
/// itself contain `%`/`_`) is matched literally, never interpreted as a
/// wildcard.
fn escape_like_pattern(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn parse_recall_kind(value: &str) -> duckdb::Result<RecallEntryKind> {
    match value {
        "message" => Ok(RecallEntryKind::Message),
        "tool_call" => Ok(RecallEntryKind::ToolCall),
        "tool_result" => Ok(RecallEntryKind::ToolResult),
        other => Err(duckdb::Error::InvalidColumnType(
            2,
            other.to_string(),
            duckdb::types::Type::Text,
        )),
    }
}

#[cfg(test)]
fn parse_json_column(column: usize, json: &str) -> duckdb::Result<Value> {
    serde_json::from_str(json).map_err(|err| {
        duckdb::Error::FromSqlConversionFailure(column, duckdb::types::Type::Text, Box::new(err))
    })
}

#[cfg(test)]
mod recall_tests {
    use super::*;
    use crate::contract::{
        ApprovalRequest, Event, Message, MessageDelta, MessageRole, ToolCallId, ToolCallRequest,
        ToolCallResult, TurnEndReason,
    };
    use crate::persistence::projection::duckdb::AppendEvent;

    fn committed(role: MessageRole, text: &str) -> Event {
        Event::MessageCommitted(Message {
            role,
            text: text.to_string(),
        })
    }

    #[test]
    fn search_finds_matches_in_each_source_and_excludes_deltas() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = ToolCallId("call-fox".to_string());

        store
            .append_events(
                session_id,
                None,
                [
                    committed(MessageRole::User, "the quick brown fox"),
                    Event::ReasoningDelta(MessageDelta {
                        role: MessageRole::Assistant,
                        text: "thinking about a fox".to_string(),
                    }),
                    Event::ToolCallRequested(ToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "fs.grep".to_string(),
                        input: serde_json::json!({ "pattern": "fox" }),
                    }),
                    Event::ToolCallFinished(ToolCallResult::new(
                        call_id,
                        serde_json::json!({ "matches": ["a red fox"] }),
                    )),
                ],
            )
            .expect("append events");

        let report = store
            .search_history(Some(session_id), Some("fox"), 20, None)
            .expect("search");

        assert_eq!(report.total, 3, "the delta must not be counted as a match");
        let kinds: Vec<RecallEntryKind> = report.hits.iter().map(|hit| hit.kind).collect();
        assert!(kinds.contains(&RecallEntryKind::Message));
        assert!(kinds.contains(&RecallEntryKind::ToolCall));
        assert!(kinds.contains(&RecallEntryKind::ToolResult));
        assert!(
            report
                .hits
                .iter()
                .all(|hit| hit.text.to_lowercase().contains("fox")),
            "every hit's bounded text must actually contain the query"
        );
    }

    #[test]
    fn search_is_case_insensitive() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        store
            .append_events(
                session_id,
                None,
                [committed(MessageRole::User, "Hello World")],
            )
            .expect("append events");

        let report = store
            .search_history(Some(session_id), Some("WORLD"), 20, None)
            .expect("search");
        assert_eq!(report.total, 1);

        let report = store
            .search_history(Some(session_id), Some("hello"), 20, None)
            .expect("search");
        assert_eq!(report.total, 1);
    }

    /// Without escaping, `_` is a LIKE wildcard matching any single
    /// character -- a literal `_` in the search query must only match rows
    /// that actually contain that character, not every row.
    #[test]
    fn search_escapes_underscore_wildcard() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        store
            .append_events(
                session_id,
                None,
                [
                    committed(MessageRole::User, "contains_underscore"),
                    committed(MessageRole::User, "nounderscorehere"),
                ],
            )
            .expect("append events");

        let report = store
            .search_history(Some(session_id), Some("_"), 20, None)
            .expect("search");
        assert_eq!(
            report.total, 1,
            "a literal `_` must only match the row that actually contains one"
        );
        assert!(report.hits[0].text.contains("contains_underscore"));
    }

    /// Same idea for `%`, LIKE's zero-or-more wildcard.
    #[test]
    fn search_escapes_percent_wildcard() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        store
            .append_events(
                session_id,
                None,
                [
                    committed(MessageRole::User, "fifty% done"),
                    committed(MessageRole::User, "no percent sign here"),
                ],
            )
            .expect("append events");

        let report = store
            .search_history(Some(session_id), Some("%"), 20, None)
            .expect("search");
        assert_eq!(report.total, 1);
        assert!(report.hits[0].text.contains("fifty%"));
    }

    #[test]
    fn search_scope_restricts_to_one_session() {
        let store = Store::open_in_memory().expect("store");
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        store
            .append_events(session_a, None, [committed(MessageRole::User, "widget a")])
            .expect("append a");
        store
            .append_events(session_b, None, [committed(MessageRole::User, "widget b")])
            .expect("append b");

        let scoped = store
            .search_history(Some(session_a), Some("widget"), 20, None)
            .expect("scoped search");
        assert_eq!(scoped.total, 1);
        assert_eq!(scoped.hits[0].session_id, session_a);

        let unscoped = store
            .search_history(None, Some("widget"), 20, None)
            .expect("unscoped search");
        assert_eq!(unscoped.total, 2);
    }

    #[test]
    fn search_reports_total_separately_from_the_limited_hits() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let events: Vec<Event> = (0..5)
            .map(|index| committed(MessageRole::User, &format!("widget number {index}")))
            .collect();
        store
            .append_events(session_id, None, events)
            .expect("append events");

        let report = store
            .search_history(Some(session_id), Some("widget"), 2, None)
            .expect("search");
        assert_eq!(report.hits.len(), 2, "rows are capped at `limit`");
        assert_eq!(
            report.total, 5,
            "total counts every match, not just `limit`"
        );
    }

    #[test]
    fn search_bounds_text_at_the_sql_layer() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let huge_text = format!("needle {}", "x".repeat(RECALL_TEXT_BOUND_CHARS * 2));
        store
            .append_events(session_id, None, [committed(MessageRole::User, &huge_text)])
            .expect("append events");

        let report = store
            .search_history(Some(session_id), Some("needle"), 20, None)
            .expect("search");
        assert_eq!(report.total, 1);
        assert_eq!(
            report.hits[0].text.chars().count(),
            RECALL_TEXT_BOUND_CHARS,
            "the SQL layer must bound the returned text regardless of the original length"
        );
    }

    #[test]
    fn search_without_a_query_lists_matches_by_turn_outcome_alone() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();

        store
            .append_event(AppendEvent {
                session_id,
                turn_id: Some("turn-completed".to_string()),
                provider_id: None,
                role_id: None,
                event: committed(MessageRole::User, "alpha"),
                provider_payload: None,
            })
            .expect("append completed message");
        store
            .append_event(AppendEvent {
                session_id,
                turn_id: Some("turn-completed".to_string()),
                provider_id: None,
                role_id: None,
                event: Event::TurnEnded(TurnEndReason::Completed),
                provider_payload: None,
            })
            .expect("append completed turn end");
        store
            .append_event(AppendEvent {
                session_id,
                turn_id: Some("turn-halted".to_string()),
                provider_id: None,
                role_id: None,
                event: committed(MessageRole::User, "beta"),
                provider_payload: None,
            })
            .expect("append halted message");
        store
            .append_event(AppendEvent {
                session_id,
                turn_id: Some("turn-halted".to_string()),
                provider_id: None,
                role_id: None,
                event: Event::TurnEnded(TurnEndReason::Halted),
                provider_payload: None,
            })
            .expect("append halted turn end");

        let report = store
            .search_history(None, None, 20, Some("halted"))
            .expect("listing-mode search");
        assert_eq!(
            report.total, 1,
            "listing mode with a turn_outcome filter must return only that turn's rows, \
             with no query to narrow by"
        );
        assert_eq!(report.hits[0].text, "beta");
        assert_eq!(report.hits[0].turn_outcome.as_deref(), Some("halted"));
    }

    #[test]
    fn read_history_window_is_ordered_ascending_and_respects_from_sequence_and_limit() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = ToolCallId("call-1".to_string());
        store
            .append_events(
                session_id,
                None,
                [
                    committed(MessageRole::User, "message 0"),
                    committed(MessageRole::Assistant, "message 1"),
                    Event::ToolCallRequested(ToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "fs.read".to_string(),
                        input: serde_json::json!({}),
                    }),
                    Event::ApprovalRequested(ApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "needs approval".to_string(),
                    }),
                    Event::ToolCallFinished(ToolCallResult::new(
                        call_id,
                        serde_json::json!({ "ok": true }),
                    )),
                ],
            )
            .expect("append events");

        // Sequences: 0=message, 1=message, 2=tool_call, 3=approval (no
        // projection, so absent from the window), 4=tool_result.
        let window = store
            .read_history_window(session_id, 1, 10)
            .expect("read window");
        let sequences: Vec<i64> = window.iter().map(|entry| entry.sequence).collect();
        assert_eq!(
            sequences,
            vec![1, 2, 4],
            "must start at from_sequence, ascend, and skip the un-projected approval"
        );

        let limited = store
            .read_history_window(session_id, 0, 2)
            .expect("read window with limit");
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].sequence, 0);
        assert_eq!(limited[1].sequence, 1);
    }
}
