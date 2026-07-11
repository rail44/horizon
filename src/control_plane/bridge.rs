//! The one floem-specific layer in the control plane: bridges accepted
//! requests from (potentially many, concurrent) connection threads onto the
//! UI thread, using the same `floem::ext_event::create_signal_from_channel`
//! and `create_effect` pattern `agent::agentd_runtime::wire_host_tool_responder`
//! already established for a shared multi-producer channel (there, every
//! agent session's host-tool requests; here, every control connection's
//! requests) -- see that function's doc comment for why this pattern
//! tolerates concurrent producers safely.
//!
//! [`ChannelExecutor`] is the [`ControlExecutor`] every connection thread
//! actually holds: `execute` sends the request plus a oneshot reply channel
//! and blocks on the reply, so from a connection thread's perspective this
//! looks like an ordinary (if cross-thread) function call -- the UI thread
//! never blocks on anything here, since [`wire`]'s effect only reacts to
//! values already sitting in the channel.
//!
//! Deliberately thin: the actual "what does this request do" logic
//! ([`app::external_commands::dispatch_invoke`]/`dispatch_query`) lives next
//! to the command model in `app`, not here -- this module only ever forwards
//! to it, so it stays directly unit-testable against the same
//! `CommandActionState` fixtures `app::command_actions`'s own tests use
//! (something this module's private-to-`app` `SessionRuntimeState`
//! dependency would otherwise make impossible to construct from outside
//! `app`).

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;
use horizon_control::contract::EnvelopeBody;

use crate::app::command_actions::CommandActionState;
use crate::app::external_commands;

use horizon_control::host::executor::{error_body, ControlExecutor, ControlRequest};

/// How long [`ChannelExecutor::execute`] waits for the UI thread to answer
/// before giving up -- generous for what should be a same-process, same-
/// frame round trip (mirrors `agent::agentd_runtime::SESSION_LIST_TIMEOUT`'s
/// rationale for a comparable cross-thread wait).
const EXECUTE_TIMEOUT: Duration = Duration::from_secs(5);

/// One request in flight from a connection thread to the UI thread, paired
/// with the reply channel [`ChannelExecutor::execute`] is blocked on.
/// `Clone` so [`wire`] can pull it out of a `ReadSignal` with `.get()`
/// (`SignalGet::get` requires `Clone` -- the same shape
/// `agent::agentd_runtime::wire_host_tool_responder`'s `HostToolRequestEnvelope`
/// already has, for the same reason); cloning is cheap, just a
/// `crossbeam_channel::Sender` and a small enum.
#[derive(Clone)]
pub(super) struct PendingRequest {
    request: ControlRequest,
    reply: Sender<EnvelopeBody>,
}

/// The production [`ControlExecutor`]: every connection thread holds a clone
/// via [`channel_pair`], and calling `execute` ships the request to whichever
/// UI-thread effect [`wire`] registered against the receiving half.
pub(super) struct ChannelExecutor {
    sender: Sender<PendingRequest>,
}

impl ControlExecutor for ChannelExecutor {
    fn execute(&self, request: ControlRequest) -> EnvelopeBody {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        if self
            .sender
            .send(PendingRequest {
                request,
                reply: reply_tx,
            })
            .is_err()
        {
            return error_body("control plane UI bridge is no longer running");
        }
        reply_rx
            .recv_timeout(EXECUTE_TIMEOUT)
            .unwrap_or_else(|_| error_body("timed out waiting for the UI thread to answer"))
    }
}

/// Builds a fresh [`ChannelExecutor`] and the receiver [`wire`] consumes --
/// split out of `wire` itself since the executor is handed to
/// `listener::spawn` (a plain OS thread, no floem involved) while `wire`
/// must run on the UI thread.
pub(super) fn channel_pair() -> (ChannelExecutor, Receiver<PendingRequest>) {
    let (sender, receiver) = crossbeam_channel::unbounded();
    (ChannelExecutor { sender }, receiver)
}

/// Registers the UI-thread effect that answers every [`PendingRequest`]
/// arriving on `requests` against `command_state`, by forwarding to
/// `app::external_commands::dispatch_invoke`/`dispatch_query`. Must run on
/// the UI thread (registers a `create_effect`); the one production call site
/// is `control_plane::start`.
pub(super) fn wire(requests: Receiver<PendingRequest>, command_state: CommandActionState) {
    let requests = create_signal_from_channel(requests);
    create_effect(move |_| {
        if let Some(pending) = requests.get() {
            let body = match &pending.request {
                ControlRequest::Invoke(invoke) => {
                    external_commands::dispatch_invoke(invoke, &command_state)
                }
                ControlRequest::Query(query) => {
                    external_commands::dispatch_query(query, &command_state)
                }
            };
            let _ = pending.reply.send(body);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_control::contract::Query;
    use std::thread;

    /// Proves the channel plumbing [`ChannelExecutor::execute`] relies on
    /// round-trips correctly, without floem: a plain background thread
    /// stands in for [`wire`]'s effect (which needs a running
    /// `floem::Application` event loop to actually fire when a value arrives
    /// from another thread -- not available in a unit test), reading the
    /// request straight off the receiver and replying. The real dispatch
    /// logic `wire` forwards to is tested directly in
    /// `app::external_commands`'s own tests.
    #[test]
    fn channel_executor_round_trips_a_request_through_a_manually_driven_receiver() {
        let (executor, receiver) = channel_pair();
        let handle = thread::spawn(move || {
            let pending = receiver.recv().expect("a request should arrive");
            assert!(matches!(pending.request, ControlRequest::Query(_)));
            let _ = pending.reply.send(EnvelopeBody::Ok);
        });

        let body = executor.execute(ControlRequest::Query(Query {
            what: "state".to_string(),
        }));

        assert!(matches!(body, EnvelopeBody::Ok));
        handle.join().unwrap();
    }

    #[test]
    fn channel_executor_answers_immediately_when_nothing_is_listening() {
        let (executor, receiver) = channel_pair();
        drop(receiver);

        let body = executor.execute(ControlRequest::Query(Query {
            what: "state".to_string(),
        }));

        assert!(matches!(body, EnvelopeBody::Error(_)));
    }
}
