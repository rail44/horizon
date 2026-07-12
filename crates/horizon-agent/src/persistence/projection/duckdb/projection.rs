use anyhow::Result;
use duckdb::params;

#[cfg(test)]
use crate::contract::SessionId;
use crate::contract::{
    ApprovalRequest, Event, Message, MessageDelta, MessageRole, ToolCallRequest, ToolCallResult,
    TurnEndReason,
};

use super::Store;
#[cfg(test)]
use super::{schema::PROJECTION_TABLES, session_id_text};

pub(super) struct EventRecordRef<'a> {
    pub(super) event_id: &'a str,
    pub(super) session_id: &'a str,
    pub(super) turn_id: Option<&'a str>,
    pub(super) sequence: i64,
    pub(super) event: &'a Event,
}

impl Store {
    #[cfg(test)]
    pub fn rebuild_projections(&self) -> Result<()> {
        for session in self.sessions()? {
            self.rebuild_projections_for_session(session.session_id)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn rebuild_projections_for_session(&self, session_id: SessionId) -> Result<()> {
        let session_id_text = session_id_text(session_id)?;
        let events = self.events_for_session(session_id)?;
        self.clear_projections_for_session(&session_id_text)?;
        for record in events {
            self.project_event(EventRecordRef {
                event_id: &record.event_id,
                session_id: &session_id_text,
                turn_id: record.turn_id.as_deref(),
                sequence: record.sequence,
                event: &record.event,
            })?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn clear_projections_for_session(&self, session_id: &str) -> Result<()> {
        for table in PROJECTION_TABLES {
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE session_id = ?"),
                params![session_id],
            )?;
        }
        Ok(())
    }

    /// Projects `record` into its dedicated transcript/tool/approval table.
    /// Returns `Ok(true)` when this was a legacy `TurnEnded` event with no
    /// `turn_id` whose `agent_turns` projection was skipped as a result --
    /// see [`Self::insert_turn`]'s doc comment. Every other event returns
    /// `Ok(false)`, including the no-dedicated-table markers below. Callers
    /// that process many records in one pass (a rebuild, an incremental
    /// catch-up) sum this flag across the batch and print one combined
    /// summary instead of one line per event -- see
    /// `import::apply_records`'s doc comment. The rare *live* case (a
    /// same-run event genuinely missing a `turn_id`, which should not
    /// happen for a current provider) is handled by
    /// `event_log::writer::run_writer`'s own warn-once latch.
    pub(super) fn project_event(&self, record: EventRecordRef) -> Result<bool> {
        let EventRecordRef {
            event_id,
            session_id,
            turn_id,
            sequence,
            event,
        } = record;
        match event {
            Event::MessageCommitted(message) => {
                self.insert_message(event_id, session_id, sequence, message, false)?;
                Ok(false)
            }
            Event::ReasoningDelta(delta) | Event::AssistantTextDelta(delta) => {
                self.insert_delta(event_id, session_id, sequence, delta)?;
                Ok(false)
            }
            Event::ToolCallRequested(request) => {
                self.insert_tool_call(event_id, session_id, sequence, request)?;
                Ok(false)
            }
            // A human approved this call -- the order-derived counterpart to
            // the deny short-circuit handled in `insert_tool_result` below
            // (see `docs/agent-feedback-design.md`'s decision 1 and its
            // implementation-shape addendum). Only affects a row still
            // pending (`outcome IS NULL`); a call with no approval row at
            // all (never gated) simply matches nothing.
            Event::ToolCallStarted(call_id) => {
                self.mark_approval_outcome(session_id, &call_id.0, "approved")?;
                Ok(false)
            }
            Event::ToolCallFinished(result) => {
                self.insert_tool_result(event_id, session_id, sequence, result)?;
                Ok(false)
            }
            Event::ApprovalRequested(request) => {
                self.insert_approval(event_id, session_id, sequence, request)?;
                Ok(false)
            }
            Event::TurnEnded(reason) => self.insert_turn(event_id, session_id, turn_id, *reason),
            // No projection table wants these yet: they're timing markers
            // for replay/inspection (see their doc comments on `Event`),
            // not transcript/tool/approval state. They still land in
            // `agent_events` via the caller's insert before `project_event`
            // runs, so `agent_events` remains the durable source a future
            // projection could be built from.
            Event::StateChanged(_)
            | Event::ProviderRequestSent(_)
            | Event::ProviderRequestFirstToken
            | Event::ProviderRequestFinished
            | Event::Error(_)
            | Event::Exited(_) => Ok(false),
        }
    }

    fn insert_message(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        message: &Message,
        is_delta: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_messages (event_id, session_id, sequence, role, text, is_delta)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                role_text(message.role),
                &message.text,
                is_delta,
            ],
        )?;
        Ok(())
    }

    fn insert_delta(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        delta: &MessageDelta,
    ) -> Result<()> {
        self.insert_message(
            event_id,
            session_id,
            sequence,
            &Message {
                role: delta.role,
                text: delta.text.clone(),
            },
            true,
        )
    }

    fn insert_tool_call(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        request: &ToolCallRequest,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_tool_calls (
                event_id,
                session_id,
                sequence,
                call_id,
                tool_id,
                input_json
             ) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &request.call_id.0,
                &request.tool_id,
                serde_json::to_string(&request.input)?,
            ],
        )?;
        Ok(())
    }

    fn insert_tool_result(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        result: &ToolCallResult,
    ) -> Result<()> {
        // Every tool's error output carries `"is_error": true` (the
        // convention every tool in `tools::` follows -- verified against
        // fs/bash/config/skill/recall's own error outputs); absence means
        // success. See `docs/agent-feedback-design.md`'s decision 1.
        let is_error = result
            .output
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        self.conn.execute(
            "INSERT INTO agent_tool_results (
                event_id,
                session_id,
                sequence,
                call_id,
                output_json,
                is_error
             ) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &result.call_id.0,
                serde_json::to_string(&result.output)?,
                is_error,
            ],
        )?;
        // A deny short-circuits without ever emitting `ToolCallStarted`
        // (`tools::approval::synchronous_result(ran=false)`), so a call
        // whose approval is still pending when its result lands must have
        // been denied -- the order-derived counterpart to the `approved`
        // case in `project_event`'s `ToolCallStarted` arm. A no-op if there
        // was no approval row (never gated) or it's already resolved.
        self.mark_approval_outcome(session_id, &result.call_id.0, "denied")?;
        Ok(())
    }

    fn insert_approval(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        request: &ApprovalRequest,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_approvals (event_id, session_id, sequence, call_id, reason)
             VALUES (?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &request.call_id.0,
                &request.reason,
            ],
        )?;
        Ok(())
    }

    /// Sets `agent_approvals.outcome` for `call_id` in `session_id`, but
    /// only for a row still pending (`outcome IS NULL`) -- see
    /// `agent_approvals.outcome`'s doc comment in `schema.rs` for why
    /// outcome is derived from event order rather than any string match.
    /// Matches zero rows harmlessly when the call was never gated by an
    /// approval, or its outcome is already resolved.
    fn mark_approval_outcome(&self, session_id: &str, call_id: &str, outcome: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_approvals SET outcome = ?
             WHERE session_id = ? AND call_id = ? AND outcome IS NULL",
            params![outcome, session_id, call_id],
        )?;
        Ok(())
    }

    /// Turn-level bookkeeping row for a `TurnEnded` event -- see
    /// `agent_turns`'s doc comment in `schema.rs` (decision 2: schema
    /// mirrors the existing per-tool-call granularity, no derived
    /// durations). `turn_id` should always be `Some` for a real
    /// `TurnEnded` (see `Event::TurnEnded`'s doc comment); if it's ever
    /// `None` (a legacy pre-turn_id event, the common real-world case
    /// against an archived log), this skips the projection and returns
    /// `Ok(true)` rather than panicking or printing here -- this is a
    /// rebuildable, non-authoritative projection, so a bad event here must
    /// not take down the writer thread or the rebuild, and a caller
    /// replaying thousands of legacy records at once must not print one
    /// line per skip (see [`Self::project_event`]'s doc comment for who
    /// reports this and how).
    fn insert_turn(
        &self,
        event_id: &str,
        session_id: &str,
        turn_id: Option<&str>,
        reason: TurnEndReason,
    ) -> Result<bool> {
        let Some(turn_id) = turn_id else {
            return Ok(true);
        };
        self.conn.execute(
            "INSERT INTO agent_turns (session_id, turn_id, end_reason, ended_event_id)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (session_id, turn_id) DO UPDATE SET
                end_reason = excluded.end_reason,
                ended_event_id = excluded.ended_event_id",
            params![session_id, turn_id, turn_end_reason_text(reason), event_id],
        )?;
        Ok(false)
    }
}

fn turn_end_reason_text(reason: TurnEndReason) -> &'static str {
    match reason {
        TurnEndReason::Completed => "completed",
        TurnEndReason::Cancelled => "cancelled",
        TurnEndReason::Failed => "failed",
        TurnEndReason::Halted => "halted",
    }
}

fn role_text(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}
