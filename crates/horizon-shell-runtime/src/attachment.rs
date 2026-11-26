//! Client attachment state is transport state, separate from the agent turn.
//!
//! One attachment task per opened session: it holds the session's
//! permission-request responders, forwards the pane's commands to the
//! connection once the attachment is ready, and passes the session's
//! inbound traffic on to the pane in connection order.

use std::{sync::Arc, time::Duration};

use agent_client_protocol::Agent;
use agent_client_protocol::v2::ConnectionTo as V2ConnectionTo;
use agent_client_protocol::v2::Responder;
use agent_client_protocol::v2::schema as v2;
use horizon_wire::SessionId;
use tokio::sync::mpsc::UnboundedReceiver;

use super::routing::{AgentRoutes, Commands, RouteKey, acp_session_id, cancelled_permission};
use crate::agent_model::{AgentEvent, Permission, PermissionDecision, ToolCallIdentity};

/// How long an attachment may stay short of ready.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AttachmentState {
    #[default]
    Connecting,
    Restoring,
    Ready,
    Failed(String),
    Disconnected(String),
}

#[derive(Clone, Debug)]
pub enum AgentUpdate {
    Event(Box<AgentEvent>),
    /// `Restoring` also tells the pane to start its fold over: everything
    /// the session sends from here on is the attachment's bootstrap.
    State(AttachmentState),
}

/// What a pane asks of its session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentCommand {
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
    Close,
}

/// One item of a session's inbound traffic, in connection order.
pub enum Inbound {
    Event(AgentEvent),
    Permission(
        Box<v2::RequestPermissionRequest>,
        Responder<v2::RequestPermissionResponse>,
    ),
    /// The response to the `session/new` or `session/resume` that opened
    /// this attachment.
    Opened(Result<(), String>),
    /// The session is no longer running, for the reason said.
    Ended(String),
}

impl AttachmentState {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn is_closed(&self) -> bool {
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

    pub fn stream_ended(&mut self) {
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

/// A permission request an attachment holds: who answers it, and the
/// options it offers.
struct Held {
    identity: ToolCallIdentity,
    responder: Responder<v2::RequestPermissionResponse>,
    options: Vec<v2::PermissionOption>,
}

/// The permission requests an attachment holds, oldest first.
#[derive(Default)]
struct HeldPermissions(Vec<Held>);

impl HeldPermissions {
    fn take(&mut self, identity: &ToolCallIdentity) -> Option<Held> {
        let index = self.0.iter().position(|held| held.identity == *identity)?;
        Some(self.0.remove(index))
    }

    fn drain(&mut self) -> Vec<(ToolCallIdentity, Responder<v2::RequestPermissionResponse>)> {
        (self.0.drain(..))
            .map(|held| (held.identity, held.responder))
            .collect()
    }
}

/// The option of `kinds`' first kind a request offers, by its kind: a
/// one-time choice before one remembered.
fn option_of(
    options: &[v2::PermissionOption],
    kinds: [v2::PermissionOptionKind; 2],
) -> Option<&v2::PermissionOptionId> {
    (kinds.iter())
        .find_map(|kind| options.iter().find(|option| option.kind == *kind))
        .map(|option| &option.option_id)
}

/// Answers a held request with the option of `kinds`, or as cancelled when
/// it offers none. Whether it was answered with one.
fn answer(held: Held, kinds: [v2::PermissionOptionKind; 2]) -> bool {
    match option_of(&held.options, kinds) {
        Some(option) => {
            let _ = held.responder.respond(selected(&option.0, None));
            true
        }
        None => {
            let _ = held.responder.respond(cancelled_permission());
            false
        }
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
    commands: Commands,
) {
    // Taken before anything is told the pane: an attachment on the
    // connection before ends, and says so, first.
    let mut commands = commands.lock_owned().await;
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
                    let own = acp_session_id(routes.agent_of(route.session_id()));
                    let Some(permission) = Permission::from_request(&request, &own) else {
                        let _ = responder.respond(cancelled_permission());
                        continue;
                    };
                    if let Some(previous) = held.take(&permission.identity) {
                        let _ = previous.responder.respond(cancelled_permission());
                    }
                    held.0.push(Held {
                        identity: permission.identity.clone(),
                        responder,
                        options: request.options.clone(),
                    });
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
                Some(Inbound::Ended(reason)) => {
                    routes
                        .send_agent(route, AgentUpdate::Event(Box::new(AgentEvent::Ended)))
                        .await;
                    phase = AttachmentState::Disconnected(reason);
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
    routes
        .send_agent(route, AgentUpdate::State(phase.clone()))
        .await;
    if routes.is_detached(route) {
        // What the pane left and could not be sent is said here, the one
        // place left to say it.
        let mut lost = 0;
        while commands.try_recv().is_ok() {
            lost += 1;
        }
        if lost > 0 {
            eprintln!(
                "horizon: {lost} command(s) for agent session {} were not sent: {phase:?}",
                route.session_id().as_uuid()
            );
        }
    }
    routes.attachment_ended(route);
}

struct Session {
    routes: Arc<AgentRoutes>,
    route: RouteKey<SessionId>,
    connection: V2ConnectionTo<Agent>,
}

impl Session {
    /// Carries out one pane command. `false` once the pane is gone.
    async fn command(&self, command: AgentCommand, held: &mut HeldPermissions) -> bool {
        use v2::PermissionOptionKind::{AllowAlways, AllowOnce, RejectAlways, RejectOnce};
        let session_id = acp_session_id(self.routes.agent_of(self.route.session_id()));
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
                Some(request) => {
                    let decision = match answer(request, [AllowOnce, AllowAlways]) {
                        true => PermissionDecision::Approved,
                        false => PermissionDecision::Cancelled,
                    };
                    self.resolved(identity, decision).await
                }
                None => true,
            },
            // A reason is the next message: the agent hears it after the
            // denial.
            AgentCommand::Deny { identity, reason } => match held.take(&identity) {
                Some(request) => {
                    let decision = match answer(request, [RejectOnce, RejectAlways]) {
                        true => PermissionDecision::Denied,
                        false => PermissionDecision::Cancelled,
                    };
                    if let Some(reason) = reason.filter(|reason| !reason.trim().is_empty()) {
                        let request = v2::PromptRequest::new(session_id, vec![reason.into()]);
                        self.report_failure("prompt", self.connection.send_request(request));
                    }
                    self.resolved(identity, decision).await
                }
                None => true,
            },
            // Nothing of the session follows the answer.
            AgentCommand::Close => {
                let request = self
                    .connection
                    .send_request(v2::CloseSessionRequest::new(session_id));
                let (routes, route) = (self.routes.clone(), self.route);
                tokio::spawn(async move {
                    let inbound = match request.block_task().await {
                        Ok(_) => Inbound::Ended("Session closed".into()),
                        Err(error) => session_error(route, format!("close failed: {error}")),
                    };
                    routes.deliver_to(route, inbound);
                });
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
        request: agent_client_protocol::v2::SentRequest<T>,
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

/// A failure of the attachment itself, shown as an error notice.
fn session_error(_route: RouteKey<SessionId>, message: String) -> Inbound {
    Inbound::Event(AgentEvent::Update(Box::new(v2::SessionUpdate::Notice(
        v2::Notice::new(v2::NoticeSeverity::Error, message),
    ))))
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
        std::assert_matches!(
            &state, AttachmentState::Failed(message) if message == "Unexpected session replay boundary"
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
            std::assert_matches!(state, AttachmentState::Failed(_));
        }
        let mut state = AttachmentState::Ready;
        state.stream_ended();
        std::assert_matches!(state, AttachmentState::Disconnected(_));
    }
}
