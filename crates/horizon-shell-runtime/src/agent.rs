//! The agent runtime's client: an ACP v2 client over the standard input and
//! output of `horizon-agent-acp`, which this runtime starts as its child
//! (`docs/agent-stdio-child-design.md`). Start it, `initialize`, dispatch
//! agent ops as ACP and `_horizon/*` requests, and route the inbound
//! traffic by session. Stopping the runtime closes the child's input, and
//! the child flushes its session logs and ends.
//!
//! Terminal traffic has its own connection to its own daemon in
//! [`super::terminal`], so restarting the agent cannot take a PTY with it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::ConnectionTo as V2ConnectionTo;
use agent_client_protocol::v2::schema as v2;
use agent_client_protocol::{
    Agent, ByteStreams, Client, JsonRpcRequest, is_incoming_transport_closed,
};
use horizon_wire::SessionId;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::attachment::{AgentCommand, Inbound, acp_session_id};
use super::common::{EstablishError, OP_TIMEOUT, RuntimeControl, StreamEnd};
use super::routing::{AgentRoutes, RouteKey, parse_session_id};
use crate::agent_model::AgentEvent;

/// A daemon-side agent session as `session/list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub workspace_root: Option<PathBuf>,
}

impl SessionSummary {
    fn from_info(info: v2::SessionInfo) -> Option<Self> {
        Some(Self {
            session_id: parse_session_id(&info.session_id)?,
            workspace_root: Some(info.cwd.0.clone()),
        })
    }
}

/// What `session/new` needs beyond the route.
pub(super) struct SessionNew {
    pub(super) workspace_root: Option<PathBuf>,
}

/// One typed request from the sync world to the runtime. Requests carry
/// their reply channel; the session-opening ops carry the receiving half of
/// their handle's command bridge.
pub(super) enum Op {
    NewAgent {
        route: RouteKey<SessionId>,
        new: Box<SessionNew>,
        commands: UnboundedReceiver<AgentCommand>,
    },
    AttachAgent {
        route: RouteKey<SessionId>,
        commands: UnboundedReceiver<AgentCommand>,
    },
    SessionList {
        reply: crossbeam_channel::Sender<Result<Vec<SessionSummary>, String>>,
    },
    SetSessionOption {
        session_id: SessionId,
        config_id: String,
        /// The value id of the entry chosen.
        value: String,
        reply: crossbeam_channel::Sender<Result<(), String>>,
    },
}

/// How long a child that was asked to end (its input closed) has to flush
/// its logs and exit before it is killed.
const CHILD_EXIT: Duration = Duration::from_secs(5);

/// How many times in a row a child may end before it served, before the
/// runtime gives up starting it.
const START_ATTEMPTS: u32 = 3;

/// Starts `binary` as the agent runtime's child and serves `ops` over its
/// standard input and output until the runtime is stopped. A child that
/// ends after it served is started again, and the sessions it had open are
/// attached to again with `session/resume`, which an agent that no longer
/// has one refuses; one that keeps ending before it serves is given up on. Its standard error is appended to `log`; its own children
/// reach this Horizon through `HORIZON_SOCKET`, the control socket.
pub(super) fn spawn(
    binary: PathBuf,
    control_socket: PathBuf,
    log: PathBuf,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
    control: Arc<RuntimeControl>,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                routes.connection_failed(format!(
                    "could not start the agent runtime's client: {error}"
                ));
                control.mark_stopped();
                return;
            }
        };
        runtime.block_on(async {
            let (mut served, mut failed_starts) = (false, 0);
            loop {
                let mut child = match start_child(&binary, &control_socket, &log) {
                    Ok(child) => child,
                    Err(error) => {
                        routes.connection_failed(error);
                        return;
                    }
                };
                let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
                    routes.connection_failed("the agent runtime has no standard streams".into());
                    return;
                };
                let end = run_stream(
                    output,
                    input,
                    &mut ops,
                    routes.clone(),
                    control.clone(),
                    served,
                )
                .await;
                // The child's input is closed with the connection: it
                // flushes its logs and ends.
                if tokio::time::timeout(CHILD_EXIT, child.wait())
                    .await
                    .is_err()
                {
                    eprintln!("the agent runtime did not end in {CHILD_EXIT:?}; killing it");
                    let _ = child.kill().await;
                }
                match end {
                    StreamEnd::Cancelled | StreamEnd::Dropped => {
                        routes.connection_failed("the agent runtime stopped".to_string());
                        return;
                    }
                    // It served, and ended: another is started.
                    StreamEnd::EstablishedFailure(error) => {
                        eprintln!("the agent runtime ended: {error}; starting it again");
                        routes.connection_lost();
                        (served, failed_starts) = (true, 0);
                    }
                    StreamEnd::GenerationMismatch { message }
                    | StreamEnd::VersionRejected { message }
                    | StreamEnd::PreHelloTransport { message }
                    | StreamEnd::Silence { message }
                    | StreamEnd::Fatal(message) => {
                        eprintln!("the agent runtime did not start: {message}");
                        failed_starts += 1;
                        if failed_starts >= START_ATTEMPTS {
                            routes.connection_failed(message);
                            return;
                        }
                        routes.connection_lost();
                        tokio::time::sleep(Duration::from_millis(200 * u64::from(failed_starts)))
                            .await;
                    }
                }
            }
        });
        control.mark_stopped();
    });
}

/// Starts `binary` with piped standard input and output, its standard
/// error appended to `log`.
fn start_child(
    binary: &Path,
    control_socket: &Path,
    log: &Path,
) -> Result<tokio::process::Child, String> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Stdio;

    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    tokio::process::Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .env("HORIZON_SOCKET", control_socket)
        .env_remove("HORIZON_SESSION_ID")
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", binary.display()))
}

#[cfg(test)]
pub(super) fn spawn_test_stream<S>(
    stream: S,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
    control: Arc<RuntimeControl>,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (read, write) = tokio::io::split(stream);
        let end = runtime.block_on(run_stream(
            read,
            write,
            &mut ops,
            routes.clone(),
            control.clone(),
            false,
        ));
        match end {
            StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                routes.connection_failed(error)
            }
            StreamEnd::GenerationMismatch { message } | StreamEnd::VersionRejected { message } => {
                routes.connection_failed(message)
            }
            StreamEnd::PreHelloTransport { .. }
            | StreamEnd::Silence { .. }
            | StreamEnd::Cancelled
            | StreamEnd::Dropped => {}
        }
        control.mark_stopped();
    });
}

/// Builds the client with every inbound handler routing into `routes`.
fn client(
    routes: &Arc<AgentRoutes>,
) -> agent_client_protocol::v2::Builder<
    Client,
    impl agent_client_protocol::HandleDispatchFrom<Agent> + use<>,
    agent_client_protocol::NullRun,
> {
    let updates = routes.clone();
    let permissions = routes.clone();
    Client
        .v2()
        .name("horizon")
        .on_receive_notification(
            async move |notification: v2::UpdateSessionNotification,
                        connection: V2ConnectionTo<Agent>| {
                // A session whose information changed may work elsewhere
                // now: its directory is read again from `session/list`.
                if let v2::SessionUpdate::SessionInfoUpdate(_) = &notification.update
                    && let Some(session_id) = parse_session_id(&notification.session_id)
                {
                    let routes = updates.clone();
                    let connection = connection.clone();
                    tokio::spawn(async move {
                        if let Ok(sessions) = list_sessions(&connection).await
                            && let Some(root) = (sessions.into_iter())
                                .find(|summary| summary.session_id == session_id)
                                .and_then(|summary| summary.workspace_root)
                        {
                            routes.route_session_info(session_id, root);
                        }
                    });
                }
                // After an error the session may have stopped: one
                // `session/list` no longer lists has ended.
                if let v2::SessionUpdate::Notice(notice) = &notification.update
                    && notice.severity == v2::NoticeSeverity::Error
                    && let Some(session_id) = parse_session_id(&notification.session_id)
                {
                    let routes = updates.clone();
                    tokio::spawn(async move {
                        if let Ok(sessions) = list_sessions(&connection).await
                            && !sessions
                                .iter()
                                .any(|summary| summary.session_id == session_id)
                        {
                            routes.route_ended(session_id, "Session ended".into());
                        }
                    });
                }
                updates.route_update(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: v2::RequestPermissionRequest,
                        responder: agent_client_protocol::v2::Responder<
                v2::RequestPermissionResponse,
            >,
                        _connection: V2ConnectionTo<Agent>| {
                permissions.route_permission(request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
}

/// Serves `ops` over one connection. `again`: an earlier connection served
/// and ended, and the sessions it had open are opened again on this one.
async fn run_stream<R, W>(
    read: R,
    write: W,
    ops: &mut UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
    control: Arc<RuntimeControl>,
    again: bool,
) -> StreamEnd
where
    R: AsyncRead + Send + 'static,
    W: AsyncWrite + Send + 'static,
{
    let transport = ByteStreams::new(write.compat_write(), read.compat());
    let main_control = control.clone();
    let main_routes = routes.clone();
    let establish = control.timeouts().establish;
    let result = client(&routes)
        .connect_with(transport, async move |connection: V2ConnectionTo<Agent>| {
            let initialized = tokio::select! {
                result = initialize(&connection, establish) => result,
                _ = main_control.cancelled() => return Ok(StreamEnd::Cancelled),
            };
            if let Err(error) = initialized {
                return Ok(error.into());
            }
            main_control.mark_established();
            // The sessions the connection before had open are opened again.
            if again {
                for route in main_routes.reopened() {
                    resume(&main_routes, route, &connection, None);
                }
            }
            let end = loop {
                tokio::select! {
                    _ = main_control.cancelled() => break StreamEnd::Cancelled,
                    _ = connection.incoming_closed() => {
                        break StreamEnd::EstablishedFailure(
                            "the agent runtime disconnected".to_string(),
                        );
                    }
                    op = ops.recv() => {
                        let Some(op) = op else {
                            break StreamEnd::Dropped;
                        };
                        handle_op(op, &connection, &main_routes, establish);
                    }
                }
            };
            Ok(end)
        })
        .await;
    match result {
        Ok(end) => end,
        Err(error) if control.is_established() => {
            StreamEnd::EstablishedFailure(format!("the agent runtime connection failed: {error}"))
        }
        Err(error) => StreamEnd::PreHelloTransport {
            message: format!("the agent runtime connection failed during initialize: {error}"),
        },
    }
}

fn initialize_request() -> v2::InitializeRequest {
    v2::InitializeRequest::new(
        ProtocolVersion::V2,
        v2::Implementation::new("horizon", env!("CARGO_PKG_VERSION")),
    )
}

/// Sends `initialize` within the establish deadline and checks the
/// daemon's extension version.
async fn initialize(
    connection: &V2ConnectionTo<Agent>,
    timeout: Duration,
) -> Result<(), EstablishError> {
    let response = match tokio::time::timeout(
        timeout,
        connection.send_request(initialize_request()).block_task(),
    )
    .await
    {
        Err(_elapsed) => {
            return Err(EstablishError::Silence(format!(
                "the agent runtime did not answer initialize within {timeout:?}"
            )));
        }
        Ok(Err(error)) if is_incoming_transport_closed(&error) => {
            return Err(EstablishError::Transient(format!(
                "the connection dropped during initialize: {error}"
            )));
        }
        Ok(Err(error)) => {
            return Err(EstablishError::Fatal(format!(
                "the agent runtime answered initialize with an unexpected error: {error}"
            )));
        }
        Ok(Ok(response)) => response,
    };
    if response.capabilities.session.is_none() {
        return Err(EstablishError::Fatal(
            "the agent runtime did not advertise the v2 session capability".to_string(),
        ));
    }
    Ok(())
}

/// Bounds one established-phase request. A deadline expiry fails only that
/// request; the connection stays up.
async fn call<T>(
    deadline: Duration,
    what: &str,
    request: agent_client_protocol::v2::SentRequest<T>,
) -> Result<T, String> {
    match tokio::time::timeout(deadline, request.block_task()).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(format!("{what} failed: {error}")),
        Err(_elapsed) => Err(format!("{what} did not answer within {deadline:?}")),
    }
}

fn send<R: JsonRpcRequest>(
    connection: &V2ConnectionTo<Agent>,
    request: R,
) -> agent_client_protocol::v2::SentRequest<R::Response> {
    connection.send_request(request)
}

fn resume_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Dispatches one op. Each request is awaited on its own task, so a slow
/// one does not stall the others.
fn handle_op(
    op: Op,
    connection: &V2ConnectionTo<Agent>,
    routes: &Arc<AgentRoutes>,
    _establish: Duration,
) {
    match op {
        Op::NewAgent {
            route,
            new,
            commands,
        } => {
            let Some(workspace_root) = new.workspace_root.or_else(|| std::env::current_dir().ok())
            else {
                let error = "the new session has no workspace root".to_string();
                routes.not_named(route.session_id(), error.clone());
                routes.agent_failed(route, error);
                return;
            };
            let request = v2::NewSessionRequest::new(workspace_root);
            open_session(
                routes,
                route,
                connection,
                Some(commands),
                |connection, routes| {
                    connection
                        .build_session_from(request)
                        .start_session()
                        .on_receiving_result(async move |result| {
                            match result {
                                Ok(opened) => {
                                    let response = &opened.response;
                                    // The agent names the session.
                                    if let Some(named) = parse_session_id(&response.session_id) {
                                        routes.name(route.session_id(), named);
                                    }
                                    routes.deliver_to(
                                        route,
                                        Inbound::Event(AgentEvent::Update(Box::new(
                                            v2::SessionUpdate::ConfigOptionUpdate(
                                                v2::ConfigOptionUpdate::new(
                                                    response.config_options.clone(),
                                                ),
                                            ),
                                        ))),
                                    );
                                    routes.deliver_to(route, Inbound::Opened(Ok(())));
                                }
                                Err(error) => {
                                    let error =
                                        format!("failed to start the agent session: {error}");
                                    routes.not_named(route.session_id(), error.clone());
                                    routes.deliver_to(route, Inbound::Opened(Err(error)))
                                }
                            }
                            Ok(())
                        })
                },
            );
        }
        Op::AttachAgent { route, commands } => resume(routes, route, connection, Some(commands)),
        Op::SessionList { reply } => {
            let connection = connection.clone();
            tokio::spawn(async move {
                let _ = reply.send(list_sessions(&connection).await);
            });
        }
        Op::SetSessionOption {
            session_id,
            config_id,
            value,
            reply,
        } => {
            let request = v2::SetSessionConfigOptionRequest::new(
                acp_session_id(routes.agent_of(session_id)),
                config_id,
                v2::SessionConfigOptionValue::id(value),
            );
            let request = send(connection, request);
            tokio::spawn(async move {
                let result = call(OP_TIMEOUT, "set session option", request).await;
                let _ = reply.send(result.map(|_| ()));
            });
        }
    }
}

/// Opens `route`'s inbound queue, sends the opening request `send`
/// builds (whose response callback must deliver [`Inbound::Opened`]), and
/// starts the attachment task.
/// Attaches to a session the agent has, with `session/resume` and its
/// history replayed from the start. `commands`: the pane's, when the
/// session is first attached to; none when it is attached to again.
fn resume(
    routes: &Arc<AgentRoutes>,
    route: RouteKey<SessionId>,
    connection: &V2ConnectionTo<Agent>,
    commands: Option<UnboundedReceiver<AgentCommand>>,
) {
    let request = v2::ResumeSessionRequest::new(
        acp_session_id(routes.agent_of(route.session_id())),
        resume_cwd(),
    )
    .replay_from(v2::ReplayFrom::from(v2::ReplayFromStart::new()));
    open_session(routes, route, connection, commands, |connection, routes| {
        connection
            .resume_session_from(request)
            .start_session()
            .on_receiving_result(async move |result| {
                let opened = result
                    .map(|_| ())
                    .map_err(|error| format!("failed to attach to the agent session: {error}"));
                routes.deliver_to(route, Inbound::Opened(opened));
                Ok(())
            })
    });
}

fn open_session(
    routes: &Arc<AgentRoutes>,
    route: RouteKey<SessionId>,
    connection: &V2ConnectionTo<Agent>,
    commands: Option<UnboundedReceiver<AgentCommand>>,
    send: impl FnOnce(
        &V2ConnectionTo<Agent>,
        Arc<AgentRoutes>,
    ) -> Result<(), agent_client_protocol::Error>,
) {
    let Some((inbound, commands)) = routes.open_inbound(route, commands) else {
        return;
    };
    if let Err(error) = send(connection, routes.clone()) {
        routes.agent_failed(
            route,
            format!("failed to send the session request: {error}"),
        );
        return;
    }
    tokio::spawn(super::attachment::run(
        routes.clone(),
        route,
        connection.clone(),
        inbound,
        commands,
    ));
}

async fn list_sessions(connection: &V2ConnectionTo<Agent>) -> Result<Vec<SessionSummary>, String> {
    let mut summaries = Vec::new();
    let mut cursor = None;
    loop {
        let mut request = v2::ListSessionsRequest::new();
        request.cursor = cursor;
        let response = call(OP_TIMEOUT, "agent list", send(connection, request)).await?;
        summaries.extend(
            response
                .sessions
                .into_iter()
                .filter_map(SessionSummary::from_info),
        );
        match response.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(summaries),
        }
    }
}
