use anyhow::Result;
use duckdb::params;

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
            Event::ToolCallStarted(identity) => self.mark_approval_outcome(
                session_id, &identity.call_id.0, &identity.occurrence_id.0, "approved",
            ),
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
            | Event::SessionInputSent { .. } | Event::EnvironmentReady { .. } | Event::EnvironmentActivated(_) | Event::EnvironmentActivationFailed(_) | Event::SessionResumed | Event::InputQueuePaused(_) | Event::InputStarted(_) | Event::InputAccepted(_) | Event::InputOutcome(_) | Event::DeliveryAcknowledged(_) | Event::MemorySeeded | Event::MoaPassStarted(_) | Event::ConversationRecorded(_) => Ok(()),
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
                request.occurrence_id.0.as_str(),
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
                result.occurrence_id.0.as_str(),
                serde_json::to_string(&result.output)?,
                result.is_error(),
            ],
        )?;
        // A result closes only its own pending approval. Cancellation is not
        // a user denial; a started call has already recorded approval.
        let approval_outcome = match result.outcome {
            crate::contract::ToolOutcome::Denied => "denied",
            crate::contract::ToolOutcome::Cancelled => "cancelled",
            crate::contract::ToolOutcome::Superseded { .. } => "superseded",
            crate::contract::ToolOutcome::Succeeded | crate::contract::ToolOutcome::Failed => {
                "approved"
            }
        };
        self.mark_approval_outcome(
            session_id,
            &result.call_id.0,
            result.occurrence_id.0.as_str(),
            approval_outcome,
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
                request.occurrence_id.0.as_str(),
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
    /// Exactly identifies one execution; starts and finishes never resolve an
    /// unrelated pending approval that happens to reuse the provider call id.
    fn mark_approval_outcome(
        &self,
        session_id: &str,
        call_id: &str,
        occurrence_id: &str,
        outcome: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_approvals SET outcome = ?
             WHERE session_id = ? AND call_id = ? AND occurrence_id = ?
               AND outcome IS NULL",
            params![outcome, session_id, call_id, occurrence_id],
        )?;
        Ok(())
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
        TurnEndReason::HaltedByIterationCap | TurnEndReason::HaltedByDoomLoop => "halted",
    }
}
