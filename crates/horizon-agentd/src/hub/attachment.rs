//! One attachment's pump: streams the private bootstrap, then the live
//! events, as ACP notifications on the connection, and feeds the
//! attachment's commands into its lease.
use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v2;
use agent_client_protocol::{Client, ConnectionTo, Responder};
use horizon_acp as acp;
use horizon_agent::contract::{ApprovalRequest, Command, SessionId};
use horizon_agent::wire::{AgentWireEvent, AttachmentEnd};
use tokio::sync::{mpsc, oneshot};

use super::mapping::{self, horizon_meta, Mapper, Outgoing};
use super::{call_error, Shared};
use crate::session::{AttachmentLease, Bootstrap};

/// The request that opened the attachment; answered by the pump.
pub(super) enum Opening {
    /// Answered before the bootstrap streams.
    New(Responder<v2::NewSessionResponse>),
    /// Answered once the bootstrap has streamed.
    Resume(Responder<v2::ResumeSessionResponse>),
}

impl Opening {
    pub(super) fn fail(self, message: String) {
        let _ = match self {
            Self::New(responder) => responder.respond_with_error(call_error(message)),
            Self::Resume(responder) => responder.respond_with_error(call_error(message)),
        };
    }
}

/// Sends for one attachment, only while it is still this connection's
/// current attachment of its session.
#[derive(Clone)]
pub(super) struct Sink {
    shared: Arc<Shared>,
    cx: ConnectionTo<Client>,
    session_id: SessionId,
    acp_id: v2::SessionId,
    generation: u64,
}

impl Sink {
    fn is_current(&self) -> bool {
        self.shared.is_current(self.session_id, self.generation)
    }

    fn notify<N: agent_client_protocol::JsonRpcNotification>(&self, notification: N) -> bool {
        self.shared
            .while_current(self.session_id, self.generation, || {
                self.cx.send_notification(notification).is_ok()
            })
            .unwrap_or(false)
    }

    fn emit(&self, outgoing: Outgoing, asks: &mut Asks) -> bool {
        match outgoing {
            Outgoing::Update(update) => self.notify(v2::UpdateSessionNotification::new(
                self.acp_id.clone(),
                update,
            )),
            Outgoing::TaskProgress(notification) => self.notify(notification),
            Outgoing::ToolCallProgress(notification) => self.notify(notification),
            Outgoing::Memory(notification) => self.notify(notification),
            Outgoing::SessionEvent(notification) => self.notify(notification),
            Outgoing::ProviderRequest(notification) => self.notify(notification),
            Outgoing::AskPermission(request) => {
                asks.ask(self, request);
                true
            }
            Outgoing::ApprovalSettled(occurrence) => {
                asks.settle(&occurrence);
                true
            }
        }
    }
}

/// The attachment's outstanding `session/request_permission` requests,
/// by occurrence id. Settling or dropping one cancels its request.
#[derive(Default)]
struct Asks(HashMap<String, oneshot::Sender<()>>);

impl Asks {
    fn ask(&mut self, sink: &Sink, request: ApprovalRequest) {
        let (cancel, cancelled) = oneshot::channel();
        self.0.insert(request.occurrence_id.0.clone(), cancel);
        tokio::spawn(ask_permission(sink.clone(), request, cancelled));
    }

    fn settle(&mut self, occurrence: &str) {
        if let Some(cancel) = self.0.remove(occurrence) {
            let _ = cancel.send(());
        }
    }
}

fn permission_request(acp_id: v2::SessionId, request: &ApprovalRequest) -> v2::RequestPermissionRequest {
    let options = vec![
        v2::PermissionOption::new(
            acp::PERMISSION_OPTION_APPROVE,
            "Approve",
            v2::PermissionOptionKind::AllowOnce,
        ),
        v2::PermissionOption::new(
            acp::PERMISSION_OPTION_DENY,
            "Deny",
            v2::PermissionOptionKind::RejectOnce,
        ),
    ];
    v2::RequestPermissionRequest::new(acp_id, mapping::approval_title(&request.kind), options)
        .description(request.reason.clone())
        .subject(v2::RequestPermissionSubject::ToolCall(Box::new(
            v2::ToolCallPermissionSubject::new(v2::ToolCallUpdate::new(
                request.occurrence_id.0.clone(),
            )),
        )))
        .meta(horizon_meta(&acp::ApprovalMeta {
            call_id: request.call_id.0.clone(),
            occurrence_id: request.occurrence_id.0.clone(),
            kind: mapping::approval_kind(&request.kind),
        }))
}

/// Asks the client and turns its choice into the command a client would
/// have sent. Never returns an error: a failure here is this approval's
/// alone.
async fn ask_permission(sink: Sink, request: ApprovalRequest, cancelled: oneshot::Receiver<()>) {
    if !sink.is_current() {
        return;
    }
    let sent = sink
        .cx
        .send_request(permission_request(sink.acp_id.clone(), &request));
    let request_id = sent.id().clone();
    let response = tokio::select! {
        response = sent.block_task() => response,
        _ = cancelled => {
            let _ = sink.cx.send_cancel_request(request_id);
            return;
        }
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            eprintln!("horizon-agentd: permission request failed: {error}");
            return;
        }
    };
    let identity = request.identity();
    let command = match &response.outcome {
        v2::RequestPermissionOutcome::Selected(selected)
            if &*selected.option_id.0 == acp::PERMISSION_OPTION_APPROVE =>
        {
            Command::ApproveToolCall { identity }
        }
        v2::RequestPermissionOutcome::Selected(selected)
            if &*selected.option_id.0 == acp::PERMISSION_OPTION_DENY =>
        {
            let reason = acp::read_horizon_meta::<acp::PermissionResponseMeta>(
                response.meta.as_ref(),
            )
            .or_else(|| acp::read_horizon_meta(selected.meta.as_ref()))
            .and_then(Result::ok)
            .and_then(|meta| meta.reason);
            Command::DenyToolCall { identity, reason }
        }
        v2::RequestPermissionOutcome::Selected(selected) => {
            eprintln!(
                "horizon-agentd: ignoring unknown permission option {}",
                selected.option_id.0
            );
            return;
        }
        _ => return,
    };
    if let Err(error) = sink.shared.command(sink.session_id, command) {
        eprintln!("horizon-agentd: dropping a permission answer: {}", error.message);
    }
}

pub(super) fn start(
    shared: Arc<Shared>,
    cx: ConnectionTo<Client>,
    session_id: SessionId,
    generation: u64,
    bootstrap: Bootstrap,
    commands: mpsc::UnboundedReceiver<Command>,
    opening: Opening,
) {
    tokio::spawn(run(
        shared, cx, session_id, generation, bootstrap, commands, opening,
    ));
}

/// Owns the lease for the attachment's whole life; any exit revokes it.
async fn run(
    shared: Arc<Shared>,
    cx: ConnectionTo<Client>,
    session_id: SessionId,
    generation: u64,
    bootstrap: Bootstrap,
    mut commands: mpsc::UnboundedReceiver<Command>,
    opening: Opening,
) {
    let Bootstrap {
        history,
        metadata,
        mut events,
        mut ended,
        lease,
    } = bootstrap;
    let sink = Sink {
        acp_id: mapping::acp_session_id(session_id),
        shared: shared.clone(),
        cx,
        session_id,
        generation,
    };
    let mut mapper = Mapper::new(session_id, shared.session_facts(session_id));
    let mut asks = Asks::default();
    let (head, tail): (Vec<_>, Vec<_>) = metadata.into_iter().partition(|event| {
        matches!(
            event,
            AgentWireEvent::SessionModel(_)
                | AgentWireEvent::SessionSelection(_)
                | AgentWireEvent::WorkspaceRootResolved(_)
        )
    });
    for event in &head {
        mapper.absorb_metadata(event);
    }

    let resume = match opening {
        Opening::New(responder) => {
            let response = v2::NewSessionResponse::new(sink.acp_id.clone())
                .config_options(mapper.config_options())
                .meta(horizon_meta(&mapper.session_info_meta()));
            let _ = responder.respond(response);
            None
        }
        Opening::Resume(responder) => Some(responder),
    };

    let mut streamed = mapper
        .config_option_update()
        .into_iter()
        .chain([mapper.session_info_update()])
        .all(|outgoing| sink.emit(outgoing, &mut asks));
    for event in history
        .into_iter()
        .map(AgentWireEvent::Event)
        .chain(tail)
    {
        if !streamed {
            break;
        }
        for outgoing in mapper.map(&event) {
            streamed &= sink.emit(outgoing, &mut asks);
        }
    }
    if !streamed {
        if let Some(responder) = resume {
            let _ = responder.respond_with_error(call_error(
                "The attachment ended before its history was restored".to_string(),
            ));
        }
        drop(lease);
        shared.release(session_id, generation);
        return;
    }
    if let Some(responder) = resume {
        let _ = responder.respond(v2::ResumeSessionResponse::new());
    }
    for request in mapper.finish_replay() {
        asks.ask(&sink, request);
    }

    let reason = tokio::select! {
        biased;
        reason = ended.wait_for(|end| end.is_some()) => reason.ok().and_then(|end| *end),
        reason = live(&mut events, &mut commands, &lease, &mut mapper, &sink, &mut asks) => reason,
    };
    // Revoke before the final diagnostic.
    drop(lease);
    drop(asks);
    if let Some(reason) = reason {
        sink.notify(acp::SessionEventNotification::AttachmentClosed {
            session_id,
            reason: attachment_end(reason),
        });
    }
    shared.release(session_id, generation);
}

/// `None` when the attachment stopped being this connection's current one
/// or the connection is gone; there is nobody left to tell then.
async fn live(
    events: &mut mpsc::Receiver<AgentWireEvent>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    lease: &AttachmentLease,
    mapper: &mut Mapper,
    sink: &Sink,
    asks: &mut Asks,
) -> Option<AttachmentEnd> {
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => {
                    for outgoing in mapper.map(&event) {
                        if !sink.emit(outgoing, asks) {
                            return None;
                        }
                    }
                }
                None => return Some(AttachmentEnd::SessionEnded),
            },
            command = commands.recv() => match command {
                Some(command) => {
                    if !lease.command(command) {
                        return Some(AttachmentEnd::Detached);
                    }
                }
                None => return sink.is_current().then_some(AttachmentEnd::Detached),
            },
        }
    }
}

fn attachment_end(end: AttachmentEnd) -> acp::AttachmentEnd {
    match end {
        AttachmentEnd::Replaced => acp::AttachmentEnd::Replaced,
        AttachmentEnd::Lagged => acp::AttachmentEnd::Lagged,
        AttachmentEnd::Detached => acp::AttachmentEnd::Detached,
        AttachmentEnd::SessionEnded => acp::AttachmentEnd::SessionEnded,
    }
}
