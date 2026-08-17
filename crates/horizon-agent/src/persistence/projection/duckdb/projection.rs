use anyhow::Result;
use duckdb::{params, OptionalExt};

#[cfg(test)]
use crate::contract::SessionId;
use crate::contract::{
    ApprovalRequest, Event, Message, MessageDelta, ToolCallRequest, ToolCallResult, TurnEndReason,
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
    pub(crate) fn rebuild_projections(&self) -> Result<()> {
        for session in self.sessions()? {
            self.rebuild_projections_for_session(session.session_id)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn rebuild_projections_for_session(&self, session_id: SessionId) -> Result<()> {
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
    /// Several variants below have no dedicated table and are a no-op here
    /// (see the comments on that arm for why, per variant).
    pub(super) fn project_event(&self, record: EventRecordRef) -> Result<()> {
        let EventRecordRef {
            event_id,
            session_id,
            turn_id,
            sequence,
            event,
        } = record;
        match event {
            Event::MessageCommitted(message) => {
                self.insert_message(event_id, session_id, sequence, message, false)
            }
            Event::ReasoningDelta(delta) | Event::AssistantTextDelta(delta) => {
                self.insert_delta(event_id, session_id, sequence, delta)
            }
            Event::ToolCallRequested(request) => {
                self.insert_tool_call(event_id, session_id, sequence, request)
            }
            // A human approved this call -- the order-derived counterpart to
            // the deny short-circuit handled in `insert_tool_result` below
            // (see `docs/agent-feedback-design.md`'s decision 1 and its
            // implementation-shape addendum). Only affects a row still
            // pending (`outcome IS NULL`); a call with no approval row at
            // all (never gated) simply matches nothing.
            //
            // `ToolCallStarted(ToolCallId)` carries no `occurrence_id`
            // (it stays a unit-style variant to keep the wire change
            // additive, see `contract::OccurrenceId`'s doc comment), so
            // this falls through to the most-recent-pending lookup in
            // `mark_approval_outcome` -- which is still the correct
            // target because `ToolCallStarted` always fires after the
            // matching `ApprovalRequested`, so any later-reissued
            // approval for the same `call_id` is also still pending and
            // comes after this one's approval row by `sequence`.
            Event::ToolCallStarted(call_id) => {
                self.mark_approval_outcome(session_id, &call_id.0, None, "approved")
            }
            Event::ToolCallFinished(result) => {
                self.insert_tool_result(event_id, session_id, sequence, result)
            }
            Event::ApprovalRequested(request) => {
                self.insert_approval(event_id, session_id, sequence, request)
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
            | Event::ProviderRequestUsage(_)
            // `HistoryCleared` wants no projection row either: it records a
            // decision about the *provider view*, not transcript/tool state,
            // and the raw record in `agent_events` is what the rig session's
            // resume path replays it from (`providers::rig::history`).
            | Event::HistoryCleared(_)
            // Operator-intervention audit records (`ApprovalResolved` /
            // `ContinueTurnRequested`): no dedicated projection table --
            // they are pure audit signals whose primary consumer is the raw
            // `agent_events` row (the `requested -> resolved` join for
            // approval-wait times, and the `TurnEnded -> ContinueTurnRequested`
            // join for continue-turn usage). The order-derived
            // `agent_approvals.outcome` (populated by `ToolCallStarted` /
            // `ToolCallFinished` arms above) stays in place as a derived,
            // best-effort projection of `ApprovalResolved::decision` for
            // backward compatibility with existing queries; the new event
            // is the authoritative source from now on.
            | Event::ApprovalResolved(_)
            | Event::ContinueTurnRequested(_)
            | Event::Error(_)
            | Event::ProviderRateLimited(_)
            | Event::Exited(_) => Ok(()),
            // Standing-agent memory events: no dedicated projection table —
            // the raw `agent_events` row is what the provider-view projection
            // replays the document from
            // (`tools::memory::memory_document_from_events`).
            | Event::MemoryDigest(_)
            | Event::MemoryCheckpointMissed
            | Event::MemorySeeded => Ok(()),
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
                message.role.db_key(),
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
                occurrence_id,
                tool_id,
                input_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &request.call_id.0,
                request.occurrence_id.as_ref().map(|o| o.0.as_str()),
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
                occurrence_id,
                output_json,
                is_error
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &result.call_id.0,
                result.occurrence_id.as_ref().map(|o| o.0.as_str()),
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
        //
        // The result's `occurrence_id` (when present, stamped by the
        // agent's tool executor at fold time from the originating
        // request) lets `mark_approval_outcome` target the specific
        // approval row this result answers to, instead of the
        // most-recent-pending-with-this-call_id fallback below -- which
        // would otherwise flip an unrelated pending approval's outcome
        // for a reused `call_id` (provider-reuse or sandbox-denial-retry,
        // see `backlog 42 / 55`).
        self.mark_approval_outcome(
            session_id,
            &result.call_id.0,
            result.occurrence_id.as_ref().map(|o| o.0.as_str()),
            "denied",
        )?;
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
            "INSERT INTO agent_approvals (event_id, session_id, sequence, call_id, occurrence_id, reason)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &request.call_id.0,
                request.occurrence_id.as_ref().map(|o| o.0.as_str()),
                &request.reason,
            ],
        )?;
        Ok(())
    }

    /// Sets `agent_approvals.outcome` for a row matching `call_id` in
    /// `session_id`, but only for a row still pending (`outcome IS NULL`)
    /// -- see `agent_approvals.outcome`'s doc comment in `schema.rs` for
    /// why outcome is derived from event order rather than any string
    /// match.
    ///
    /// When `occurrence_id` is `Some`, this targets the specific approval
    /// row it was stamped on (the agentd's
    /// `begin_reissued_approval` mints a fresh one per reissue, see
    /// `session/approval.rs`) and nothing else. When it is `None` --
    /// the `ToolCallStarted` arm, which carries no occurrence, and
    /// legacy / replayed pre-feature events -- this instead falls back
    /// to the *most recent* pending approval for this `call_id`, the
    /// order-derived
    /// counterpart of the order-derived resolution (a deny short-circuits
    /// without ever emitting `ToolCallStarted`, so a result landing on a
    /// still-pending approval means the human denied it; a
    /// `ToolCallStarted` landing on a still-pending approval means the
    /// human approved it). That most-recent row is picked by a separate
    /// `ORDER BY sequence DESC LIMIT 1` lookup rather than folded into the
    /// `UPDATE` as a `sequence = (SELECT MAX(sequence) ...)` scalar
    /// subquery, which is what this used to be -- see
    /// [`Self::most_recent_pending_approval`] for why that shape had to
    /// go. Either way exactly one row is picked, avoiding the pre-
    /// `occurrence_id` behavior's bug where multiple pending rows for the
    /// same `call_id` (provider-reuse, sandbox-denial-retry) would all
    /// flip to the same outcome.
    ///
    /// Matches zero rows harmlessly when the call was never gated by an
    /// approval, or its outcome is already resolved.
    fn mark_approval_outcome(
        &self,
        session_id: &str,
        call_id: &str,
        occurrence_id: Option<&str>,
        outcome: &str,
    ) -> Result<()> {
        if let Some(occ) = occurrence_id {
            self.conn.execute(
                "UPDATE agent_approvals SET outcome = ?
                 WHERE session_id = ? AND call_id = ? AND occurrence_id = ?
                   AND outcome IS NULL",
                params![outcome, session_id, call_id, occ],
            )?;
        } else {
            // Fallback / `ToolCallStarted` arm (which has no
            // `occurrence_id`): most-recent-pending for this call_id.
            // Deliberately `else`, not a second unconditional statement:
            // running both would resolve the targeted row *and* the most
            // recent pending one, so with two pending approvals sharing a
            // call_id, resolving either would silently stamp the other with
            // the same outcome -- the exact collapse `occurrence_id` exists
            // to prevent.
            if let Some(event_id) = self.most_recent_pending_approval(session_id, call_id)? {
                // `event_id` is `agent_approvals`'s primary key, so this
                // updates exactly the row the lookup picked.
                self.conn.execute(
                    "UPDATE agent_approvals SET outcome = ? WHERE event_id = ?",
                    params![outcome, &event_id],
                )?;
            }
        }
        Ok(())
    }

    /// `event_id` of the highest-`sequence` still-pending approval for
    /// `(session_id, call_id)`, or `None` when there is none.
    ///
    /// Split out of [`Self::mark_approval_outcome`]'s single statement --
    /// which used to select the same row inline via `sequence = (SELECT
    /// MAX(sequence) FROM agent_approvals WHERE ... outcome IS NULL)` --
    /// because that shape crashes DuckDB. An aggregate over a scan whose
    /// filter the optimizer can *statically prove* selects nothing (min/max
    /// column statistics rule the `call_id` out, or the null count rules
    /// `outcome IS NULL` out) fails during statistics propagation with
    /// `INTERNAL Error: Attempted to access index 0 within vector of size 0`
    /// -- but only while the table carries transaction-local, uncommitted
    /// rows. That is why it never showed up on the live per-event append
    /// path, whose one-record transaction covers a single event and so
    /// never both inserts an approval row and calls this, and instead took
    /// down the whole batched rebuild, where every approval inserted so
    /// far in the batch is still uncommitted.
    /// Reproduced down to `SELECT MAX(sequence) FROM agent_approvals WHERE
    /// call_id = <absent>` on both libduckdb 1.5.0 (the system library
    /// here) and 1.5.4 (libduckdb-sys 1.10504.0's bundled build), so this
    /// is not the version skew AGENTS.md "Build setup" warns about -- see
    /// `docs/tasks/backlog.md` 69. The same query is fine once those rows
    /// are committed, and dropping the aggregate (this `ORDER BY`/`LIMIT`
    /// lookup) is fine on both versions either way. Prefer a top-N lookup
    /// over an aggregate anywhere a filter may select nothing.
    fn most_recent_pending_approval(
        &self,
        session_id: &str,
        call_id: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT event_id FROM agent_approvals
                 WHERE session_id = ? AND call_id = ? AND outcome IS NULL
                 ORDER BY sequence DESC
                 LIMIT 1",
                params![session_id, call_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Turn-level bookkeeping row for a `TurnEnded` event -- see
    /// `agent_turns`'s doc comment in `schema.rs` (decision 2: schema
    /// mirrors the existing per-tool-call granularity, no derived
    /// durations). `turn_id` is `Some` for a real `TurnEnded` (see
    /// `Event::TurnEnded`'s doc comment and `event_log::turn::TurnTracker`);
    /// if it's ever `None`, `agent_turns.turn_id`'s `NOT NULL` constraint
    /// surfaces that as a genuine insert error rather than a silently
    /// skipped projection -- this project carries no compatibility with an
    /// archived pre-`turn_id` log.
    fn insert_turn(
        &self,
        event_id: &str,
        session_id: &str,
        turn_id: Option<&str>,
        reason: TurnEndReason,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_turns (session_id, turn_id, end_reason, ended_event_id)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (session_id, turn_id) DO UPDATE SET
                end_reason = excluded.end_reason,
                ended_event_id = excluded.ended_event_id",
            params![session_id, turn_id, turn_end_reason_text(reason), event_id],
        )?;
        Ok(())
    }
}

fn turn_end_reason_text(reason: TurnEndReason) -> &'static str {
    match reason {
        TurnEndReason::Completed => "completed",
        TurnEndReason::Cancelled => "cancelled",
        TurnEndReason::Failed => "failed",
        // All three guard-halt reasons project to the same coarse label --
        // nothing queries a finer distinction here today; the specific
        // guard kind is a UI-rendering concern (`TurnEndReason`'s own doc
        // comment), not a `agent_turns` query one.
        TurnEndReason::Halted
        | TurnEndReason::HaltedByIterationCap
        | TurnEndReason::HaltedByDoomLoop => "halted",
    }
}
