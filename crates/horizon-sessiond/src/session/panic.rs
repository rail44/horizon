//! The session thread's panic boundary: what it was doing, and how a
//! crossed boundary is reported and committed as a terminal outcome.

use std::cell::Cell;
use std::fmt;
use std::sync::Arc;

use horizon_agent::contract::{
    Error as AgentError, Event, ProviderEvent, ProviderId, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::frame::{AgentFrame, AgentFrameItem};
use horizon_agent::live::LiveState;
use horizon_agent::persistence::event_log::Appender;
use horizon_agent::roles::RoleId;
use horizon_agent::runtime_panic::{catch_runtime_panic, PanicLocation, PanicReport};
use horizon_agent::wire::AgentWireEvent;

use super::events::send_session_event;
use super::resume::session_is_dead;
use super::state::SessiondState;

/// What the dedicated session thread was doing when an unwind crossed its
/// runtime boundary. Provider events retain their exact contract kind so a
/// persisted panic report can identify the event whose handling failed
/// without relying on sessiond's transient stderr.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SessionLoopPhase {
    Starting,
    WaitingForInput,
    ProviderEvent(&'static str),
    ToolCompletion,
    InboundCommand,
    Replay,
    RecordingPanic,
    CleaningUp,
}

impl fmt::Display for SessionLoopPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Starting => formatter.write_str("starting the session runtime"),
            Self::WaitingForInput => formatter.write_str("waiting for session input"),
            Self::ProviderEvent(kind) => write!(formatter, "handling provider event `{kind}`"),
            Self::ToolCompletion => formatter.write_str("folding an asynchronous tool completion"),
            Self::InboundCommand => formatter.write_str("dispatching an inbound command"),
            Self::Replay => formatter.write_str("answering an event replay request"),
            Self::RecordingPanic => formatter.write_str("recording a previous session panic"),
            Self::CleaningUp => formatter.write_str("cleaning up the session runtime"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SessionPanic {
    phase: SessionLoopPhase,
    payload: String,
    location: Option<PanicLocation>,
}

impl SessionPanic {
    pub(super) fn from_report(phase: SessionLoopPhase, report: PanicReport) -> Self {
        Self {
            phase,
            payload: report.payload,
            location: report.location,
        }
    }

    pub(super) fn message(&self) -> String {
        match &self.location {
            Some(location) => format!(
                "internal agent session panic while {} at {location}: {}",
                self.phase, self.payload
            ),
            None => format!(
                "internal agent session panic while {}: {}",
                self.phase, self.payload
            ),
        }
    }

    fn terminal_events(&self, turn_in_flight: bool) -> Vec<Event> {
        let mut events = vec![Event::Error(AgentError {
            message: self.message(),
        })];
        if turn_in_flight {
            events.push(Event::TurnEnded(TurnEndReason::Failed));
        }
        events.push(Event::StateChanged(SessionState::Terminated));
        events
    }
}

pub(super) fn catch_session_panic<T>(
    phase: &Cell<SessionLoopPhase>,
    operation: impl FnOnce() -> T,
) -> Result<T, SessionPanic> {
    catch_runtime_panic(operation).map_err(|report| SessionPanic::from_report(phase.get(), report))
}

/// Commits a dispatcher panic through the session's existing `LiveState`, so
/// an open turn retains its active `turn_id`, then forwards the same terminal
/// sequence to the attached client. A panicked dispatcher is not reusable:
/// reporting `WaitingForUser` here would expose an input-ready state after
/// this thread and provider handle are dropped, so the final state is
/// deliberately `Terminated`.
pub(super) fn record_session_loop_panic(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
    failure: &SessionPanic,
) {
    let events = failure.terminal_events(live_state.frame().is_turn_in_flight());
    let _ = live_state.extend_provider_events(events.iter().cloned().map(ProviderEvent::from));
    for event in events {
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }
}

/// Converts an event-channel disconnect into an explicit terminal outcome.
///
/// A provider's normal `Command::Shutdown` path sends `Terminated` before
/// dropping its sender, so an already-dead frame is a no-op here. Any other
/// disconnect means the provider runtime can no longer accept input. Leaving
/// its last live state (`Running` in the incident that motivated this guard)
/// persisted would make the pane look indefinitely busy and would revive a
/// non-existent provider on replay.
///
/// The provider panic boundary emits its detailed `Error` before dropping the
/// last sender. Crossbeam drains queued messages before reporting disconnect,
/// so a trailing error item suppresses the generic fallback diagnostic while
/// this function still closes the active turn and terminates the session.
pub(super) fn record_unexpected_provider_exit(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
) {
    let events = unexpected_provider_exit_events(&live_state.frame());
    let _ = live_state.extend_provider_events(events.iter().cloned().map(ProviderEvent::from));
    for event in events {
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }
}

fn unexpected_provider_exit_events(frame: &AgentFrame) -> Vec<Event> {
    if session_is_dead(frame) {
        return Vec::new();
    }

    let mut events = Vec::new();
    if !matches!(frame.items.last(), Some(AgentFrameItem::Error(_))) {
        events.push(Event::Error(AgentError {
            message: "Agent provider runtime exited unexpectedly.".to_string(),
        }));
    }
    if frame.is_turn_in_flight() {
        events.push(Event::TurnEnded(TurnEndReason::Failed));
    }
    events.push(Event::StateChanged(SessionState::Terminated));
    events
}

/// Last-resort reporting for a panic that happens before `LiveState` exists,
/// or while the normal panic recorder itself is unwinding. There may be no
/// active turn tracker at this boundary, so it records only the diagnostic
/// and terminal state rather than fabricating a turn-less `TurnEnded`.
pub(super) fn record_uncaught_session_panic(
    state: &Arc<SessiondState>,
    session_id: SessionId,
    provider_id: &ProviderId,
    role_id: Option<&RoleId>,
    failure: &SessionPanic,
) {
    let events = failure.terminal_events(false);
    if let Some(writer) = state.writer() {
        let mut appender = Appender::new(
            writer,
            session_id,
            Some(provider_id.clone()),
            role_id.cloned(),
        );
        let _ = appender
            .append_provider_events(events.iter().cloned().map(ProviderEvent::from).collect());
    }
    for event in events {
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::{drain_events, judge_test_state};
    use crate::session::Connection;
    use horizon_agent::frame::agent_frame_from_events;
    use horizon_agent::persistence::event_log::WriterHandle;

    #[test]
    fn session_panic_capture_preserves_provider_event_kind_and_payload() {
        let phase = Cell::new(SessionLoopPhase::ProviderEvent("provider_request_finished"));
        let outcome = catch_session_panic(&phase, || -> () {
            panic!("frame reducer invariant failed");
        });

        let failure = outcome.expect_err("panic must cross the session boundary");
        assert_eq!(
            failure.phase,
            SessionLoopPhase::ProviderEvent("provider_request_finished")
        );
        assert_eq!(failure.payload, "frame reducer invariant failed");
        let location = failure
            .location
            .as_ref()
            .expect("the panic hook must retain the source location");
        assert!(location
            .file
            .ends_with("crates/horizon-sessiond/src/session/panic.rs"));
        assert!(location.line > 0);
        assert_eq!(
            failure.message(),
            format!(
                "internal agent session panic while handling provider event \
                 `provider_request_finished` at {location}: frame reducer invariant failed"
            )
        );
    }

    #[test]
    fn session_panic_capture_labels_non_string_payloads() {
        let phase = Cell::new(SessionLoopPhase::InboundCommand);
        let outcome = catch_session_panic(&phase, || {
            std::panic::panic_any(42_u8);
        });

        let failure = outcome.unwrap_err();
        assert_eq!(failure.phase, SessionLoopPhase::InboundCommand);
        assert_eq!(failure.payload, "non-string panic payload");
        assert!(failure.location.is_some());
    }

    #[test]
    fn session_loop_panic_is_forwarded_and_persisted_as_a_failed_terminated_turn() {
        let dir = tempfile::tempdir().expect("create panic log directory");
        let path = dir.path().join("events.jsonl");
        let (writer, init_rx) = WriterHandle::open(&path);
        match init_rx.recv().expect("writer startup outcome") {
            horizon_agent::persistence::event_log::WriterInit::Ready(_) => {}
            horizon_agent::persistence::event_log::WriterInit::Failed(error) => {
                panic!("writer startup failed: {error}")
            }
        }

        let state = judge_test_state();
        let session_id = SessionId::new();
        let provider_id = ProviderId("panic-test".to_string());
        let mut outgoing_rx = Connection::new(state.clone()).subscribe_agent(session_id);
        let live_state = LiveState::with_event_log_and_history(
            session_id,
            Some(provider_id),
            None,
            writer.clone(),
            Vec::new(),
        );
        live_state.extend_provider_events(
            vec![
                Event::MessageCommitted(horizon_agent::contract::Message {
                    role: horizon_agent::contract::MessageRole::User,
                    text: "trigger a turn".to_string(),
                }),
                Event::StateChanged(SessionState::Running),
            ]
            .into_iter()
            .map(ProviderEvent::from),
        );
        let failure = SessionPanic {
            phase: SessionLoopPhase::ProviderEvent("provider_request_finished"),
            payload: "test panic".to_string(),
            location: Some(PanicLocation {
                file: "crates/horizon-sessiond/src/session.rs".to_string(),
                line: 1234,
                column: 5,
            }),
        };

        record_session_loop_panic(&state, &live_state, session_id, &failure);
        writer.flush().expect("flush panic events");

        let forwarded = drain_events(&mut outgoing_rx);
        assert_eq!(
            forwarded,
            vec![
                Event::Error(AgentError {
                    message: failure.message(),
                }),
                Event::TurnEnded(TurnEndReason::Failed),
                Event::StateChanged(SessionState::Terminated),
            ]
        );

        let report =
            horizon_agent::persistence::event_log::read(&path).expect("read panic event log");
        let records = report
            .records
            .iter()
            .filter(|record| record.session_id == session_id)
            .collect::<Vec<_>>();
        let events = records
            .iter()
            .map(|record| record.event.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                Event::MessageCommitted(horizon_agent::contract::Message {
                    role: horizon_agent::contract::MessageRole::User,
                    text: "trigger a turn".to_string(),
                }),
                Event::StateChanged(SessionState::Running),
                Event::Error(AgentError {
                    message: failure.message(),
                }),
                Event::TurnEnded(TurnEndReason::Failed),
                Event::StateChanged(SessionState::Terminated),
            ]
        );
        let recovery_turn_ids = &records[2..];
        assert!(
            recovery_turn_ids[0].turn_id.is_some(),
            "the panic diagnostic must retain the active turn id"
        );
        assert_eq!(
            recovery_turn_ids[0].turn_id, recovery_turn_ids[1].turn_id,
            "Error and TurnEnded must identify the same failed turn"
        );
        assert!(
            recovery_turn_ids[2].turn_id.is_none(),
            "TurnEnded is the turn boundary; the following terminal state is \
             outside the closed turn"
        );
    }

    #[test]
    fn unexpected_provider_exit_fails_and_terminates_an_active_turn() {
        let frame = agent_frame_from_events(&[
            Event::MessageCommitted(horizon_agent::contract::Message {
                role: horizon_agent::contract::MessageRole::User,
                text: "keep working".to_string(),
            }),
            Event::StateChanged(SessionState::Running),
        ]);

        assert_eq!(
            unexpected_provider_exit_events(&frame),
            vec![
                Event::Error(AgentError {
                    message: "Agent provider runtime exited unexpectedly.".to_string(),
                }),
                Event::TurnEnded(TurnEndReason::Failed),
                Event::StateChanged(SessionState::Terminated),
            ]
        );
    }

    #[test]
    fn unexpected_provider_exit_keeps_the_provider_panic_diagnostic() {
        let panic_message =
            "internal Rig provider panic at memory.rs:266:66: attempt to subtract with overflow";
        let frame = agent_frame_from_events(&[
            Event::MessageCommitted(horizon_agent::contract::Message {
                role: horizon_agent::contract::MessageRole::User,
                text: "keep working".to_string(),
            }),
            Event::StateChanged(SessionState::Running),
            Event::Error(AgentError {
                message: panic_message.to_string(),
            }),
        ]);

        assert_eq!(
            unexpected_provider_exit_events(&frame),
            vec![
                Event::TurnEnded(TurnEndReason::Failed),
                Event::StateChanged(SessionState::Terminated),
            ],
            "the detailed provider error is already folded before disconnect"
        );
        assert!(matches!(
            frame.items.last(),
            Some(AgentFrameItem::Error(error)) if error.message == panic_message
        ));
    }

    #[test]
    fn expected_provider_exit_after_shutdown_adds_nothing() {
        let frame = agent_frame_from_events(&[
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Terminated),
        ]);

        assert!(unexpected_provider_exit_events(&frame).is_empty());
    }
}
