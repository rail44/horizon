//! Client attachment state is transport state, separate from the agent turn.
//!
//! One attachment task per opened session: it holds the session's
//! permission-request responders, forwards the pane's commands to the
//! connection once the attachment is ready, and passes the session's
//! inbound traffic on to the pane in connection order.

use std::{sync::Arc, time::Duration};

use agent_client_protocol::schema::v2;
use agent_client_protocol::{Agent, Responder, V2ConnectionTo};
use horizon_acp::{
    write_horizon_meta, AttachmentEnd, ContinueTurnRequest, PermissionResponseMeta,
    SessionEventNotification, SessionId, PERMISSION_OPTION_APPROVE, PERMISSION_OPTION_DENY,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::routing::{attachment_end_state, cancelled_permission, AgentRoutes, RouteKey};
use crate::agent::model::{AgentEvent, Permission, PermissionDecision, ToolCallIdentity};

/// How long an attachment may stay short of ready.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AttachmentState {
    #[default]
    Connecting,
    Restoring,
    Ready,
    Failed(String),
    Disconnected(String),
}

#[derive(Clone, Debug)]
pub(crate) enum AgentUpdate {
    Event(Box<AgentEvent>),
    /// `Restoring` also tells the pane to start its fold over: everything
    /// the session sends from here on is the attachment's bootstrap.
    State(AttachmentState),
}

/// What a pane asks of its session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentCommand {
    Prompt {
        text: String,
    },
    Cancel,
    Approve {
        identity: ToolCallIdentity,
    },
    Deny {
        identity: ToolCallIdentity,
        reason: Option<String>,
    },
    ContinueTurn,
    Close,
}

/// One item of a session's inbound traffic, in connection order.
pub(crate) enum Inbound {
    Event(AgentEvent),
    Permission(
        Box<v2::RequestPermissionRequest>,
        Responder<v2::RequestPermissionResponse>,
    ),
    /// The response to the `session/new` or `session/resume` that opened
    /// this attachment.
    Opened(Result<(), String>),
    Closed(AttachmentEnd),
}

impl AttachmentState {
    pub(crate) fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub(crate) fn is_closed(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Disconnected(_))
    }

    /// The opening request's response. Only a restoring attachment becomes
    /// ready; a second response is malformed ordering.
    fn opened(&mut self, result: Result<(), String>) {
        *self = match (&self, result) {
            (Self::Restoring, Ok(())) => Self::Ready,
            (_, Ok(())) => Self::Failed("Unexpected session replay boundary".into()),
            (_, Err(message)) => Self::Failed(message),
        };
    }

    pub(crate) fn stream_ended(&mut self) {
        if !self.is_closed() {
            *self = if self.is_ready() {
                Self::Disconnected("Session connection closed; reopen the session".into())
            } else {
                Self::Failed(
                    "Session history restoration was interrupted; reopen the session to retry"
                        .into(),
                )
            };
        }
    }
}

pub(super) fn acp_session_id(session_id: SessionId) -> v2::SessionId {
    v2::SessionId::new(session_id.as_uuid().to_string())
}

/// The permission requests an attachment holds, oldest first.
#[derive(Default)]
struct HeldPermissions(Vec<(ToolCallIdentity, Responder<v2::RequestPermissionResponse>)>);

impl HeldPermissions {
    fn take(
        &mut self,
        identity: &ToolCallIdentity,
    ) -> Option<Responder<v2::RequestPermissionResponse>> {
        let index = self.0.iter().position(|(held, _)| held == identity)?;
        Some(self.0.remove(index).1)
    }

    fn drain(&mut self) -> Vec<(ToolCallIdentity, Responder<v2::RequestPermissionResponse>)> {
        std::mem::take(&mut self.0)
    }
}

fn selected(option: &str, meta: Option<v2::Meta>) -> v2::RequestPermissionResponse {
    let mut outcome = v2::SelectedPermissionOutcome::new(option.to_owned());
    outcome.meta = meta.clone();
    let mut response =
        v2::RequestPermissionResponse::new(v2::RequestPermissionOutcome::Selected(outcome));
    response.meta = meta;
    response
}

/// Runs one attachment from the moment its opening request is sent until
/// either side goes away.
pub(super) async fn run(
    routes: Arc<AgentRoutes>,
    route: RouteKey<SessionId>,
    connection: V2ConnectionTo<Agent>,
    mut inbound: UnboundedReceiver<Inbound>,
    mut commands: UnboundedReceiver<AgentCommand>,
) {
    let mut phase = AttachmentState::Restoring;
    let mut held = HeldPermissions::default();
    let restore_timeout = tokio::time::sleep(RESTORE_TIMEOUT);
    tokio::pin!(restore_timeout);
    let session = Session {
        routes: routes.clone(),
        route,
        connection,
    };
    if !routes
        .send_agent(route, AgentUpdate::State(phase.clone()))
        .await
    {
        return;
    }
    loop {
        tokio::select! {
            command = commands.recv(), if phase.is_ready() => match command {
                Some(command) => {
                    if !session.command(command, &mut held).await {
                        break;
                    }
                }
                None => break,
            },
            item = inbound.recv() => match item {
                Some(Inbound::Event(event)) => {
                    if !routes.send_agent(route, AgentUpdate::Event(Box::new(event))).await {
                        break;
                    }
                }
                Some(Inbound::Permission(request, responder)) => {
                    let Some(permission) = Permission::from_request(&request) else {
                        let _ = responder.respond(cancelled_permission());
                        continue;
                    };
                    if let Some(previous) = held.take(&permission.identity) {
                        let _ = previous.respond(cancelled_permission());
                    }
                    held.0.push((permission.identity.clone(), responder));
                    let event = AgentEvent::PermissionRequested(permission);
                    if !routes.send_agent(route, AgentUpdate::Event(Box::new(event))).await {
                        break;
                    }
                }
                Some(Inbound::Opened(result)) => {
                    phase.opened(result);
                    if phase.is_ready() {
                        routes.mark_ready(route);
                    }
                    if !routes.send_agent(route, AgentUpdate::State(phase.clone())).await
                        || phase.is_closed()
                    {
                        break;
                    }
                }
                // Before this attachment's own response, a `Replaced` closes
                // the lease an earlier attachment of this session held on
                // this connection; whatever arrived so far belonged to it.
                Some(Inbound::Closed(AttachmentEnd::Replaced))
                    if phase == AttachmentState::Restoring =>
                {
                    for (_, responder) in held.drain() {
                        let _ = responder.respond(cancelled_permission());
                    }
                    if !routes.send_agent(route, AgentUpdate::State(phase.clone())).await {
                        break;
                    }
                }
                Some(Inbound::Closed(end)) => {
                    phase = attachment_end_state(end);
                    break;
                }
                None => break,
            },
            _ = &mut restore_timeout, if !phase.is_ready() => {
                phase = AttachmentState::Failed("Timed out restoring session history".into());
                break;
            }
        }
    }
    for (identity, responder) in held.drain() {
        let _ = responder.respond(cancelled_permission());
        let event = AgentEvent::PermissionResolved {
            identity,
            decision: PermissionDecision::Cancelled,
        };
        routes
            .send_agent(route, AgentUpdate::Event(Box::new(event)))
            .await;
    }
    phase.stream_ended();
    routes.send_agent(route, AgentUpdate::State(phase)).await;
}

struct Session {
    routes: Arc<AgentRoutes>,
    route: RouteKey<SessionId>,
    connection: V2ConnectionTo<Agent>,
}

impl Session {
    /// Carries out one pane command. `false` once the pane is gone.
    async fn command(&self, command: AgentCommand, held: &mut HeldPermissions) -> bool {
        let session_id = acp_session_id(self.route.session_id());
        match command {
            AgentCommand::Prompt { text } => {
                let request = v2::PromptRequest::new(session_id, vec![text.into()]);
                self.report_failure("prompt", self.connection.send_request(request));
                true
            }
            AgentCommand::Cancel => {
                if let Err(error) = self
                    .connection
                    .send_notification(v2::CancelSessionNotification::new(session_id))
                {
                    self.surface_error(format!("cancel failed: {error}"));
                }
                for (identity, responder) in held.drain() {
                    let _ = responder.respond(cancelled_permission());
                    if !self.resolved(identity, PermissionDecision::Cancelled).await {
                        return false;
                    }
                }
                true
            }
            AgentCommand::Approve { identity } => match held.take(&identity) {
                Some(responder) => {
                    let _ = responder.respond(selected(PERMISSION_OPTION_APPROVE, None));
                    self.resolved(identity, PermissionDecision::Approved).await
                }
                None => true,
            },
            AgentCommand::Deny { identity, reason } => match held.take(&identity) {
                Some(responder) => {
                    let mut meta = None;
                    let _ = write_horizon_meta(&mut meta, &PermissionResponseMeta { reason });
                    let _ = responder.respond(selected(PERMISSION_OPTION_DENY, meta));
                    self.resolved(identity, PermissionDecision::Denied).await
                }
                None => true,
            },
            AgentCommand::ContinueTurn => {
                let request = ContinueTurnRequest {
                    session_id: self.route.session_id(),
                };
                self.report_failure("continue", self.connection.send_request(request));
                true
            }
            AgentCommand::Close => {
                let request = v2::CloseSessionRequest::new(session_id);
                self.report_failure("close", self.connection.send_request(request));
                true
            }
        }
    }

    async fn resolved(&self, identity: ToolCallIdentity, decision: PermissionDecision) -> bool {
        let event = AgentEvent::PermissionResolved { identity, decision };
        self.routes
            .send_agent(self.route, AgentUpdate::Event(Box::new(event)))
            .await
    }

    /// Waits for `request`'s response off this task and surfaces an error
    /// response in the session's transcript.
    fn report_failure<T: agent_client_protocol::JsonRpcResponse + Send + 'static>(
        &self,
        what: &'static str,
        request: agent_client_protocol::SentRequest<T>,
    ) {
        let routes = self.routes.clone();
        let route = self.route;
        tokio::spawn(async move {
            if let Err(error) = request.block_task().await {
                routes.deliver_to(
                    route,
                    session_error(route, format!("{what} failed: {error}")),
                );
            }
        });
    }

    fn surface_error(&self, message: String) {
        self.routes
            .deliver_to(self.route, session_error(self.route, message));
    }
}

fn session_error(route: RouteKey<SessionId>, message: String) -> Inbound {
    Inbound::Event(AgentEvent::Session(SessionEventNotification::Error {
        session_id: route.session_id(),
        message,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_opening_response_makes_a_restoring_attachment_ready() {
        let mut state = AttachmentState::Connecting;
        state.opened(Ok(()));
        assert!(state.is_closed(), "a response before the request was sent");

        let mut state = AttachmentState::Restoring;
        state.opened(Ok(()));
        assert!(state.is_ready());
        state.opened(Ok(()));
        assert!(
            matches!(&state, AttachmentState::Failed(message) if message == "Unexpected session replay boundary")
        );

        let mut state = AttachmentState::Restoring;
        state.opened(Err("unknown session".into()));
        assert_eq!(state, AttachmentState::Failed("unknown session".into()));
    }

    #[test]
    fn incomplete_replay_and_live_disconnect_have_distinct_outcomes() {
        for initial in [AttachmentState::Connecting, AttachmentState::Restoring] {
            let mut state = initial;
            state.stream_ended();
            assert!(matches!(state, AttachmentState::Failed(_)));
        }
        let mut state = AttachmentState::Ready;
        state.stream_ended();
        assert!(matches!(state, AttachmentState::Disconnected(_)));

        let mut state = attachment_end_state(AttachmentEnd::Lagged);
        assert!(matches!(state, AttachmentState::Failed(_)));
        let failure = state.clone();
        state.stream_ended();
        assert_eq!(state, failure);
        for end in [
            AttachmentEnd::Replaced,
            AttachmentEnd::Detached,
            AttachmentEnd::SessionEnded,
        ] {
            assert!(matches!(
                attachment_end_state(end),
                AttachmentState::Disconnected(_)
            ));
        }
    }
}
