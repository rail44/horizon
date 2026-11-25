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
use horizon_acp::{
    EFFORT_CONFIG_ID, EnsureBoardOrganizerRequest, HORIZON_ACP_EXT_VERSION, HostToolRequest,
    InitializeMeta, ListProviderModelsRequest, ListProvidersRequest, MODEL_CONFIG_ID,
    ProviderSummary, ReloadProviderConfigRequest, SessionEventNotification, SessionId,
    SessionInfoMeta, SessionNewMeta, TaskProgressNotification, read_horizon_meta,
    write_horizon_meta,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::attachment::{AgentCommand, Inbound, acp_session_id};
use super::common::{EstablishError, OP_TIMEOUT, RuntimeControl, StreamEnd};
use super::routing::{AgentRoutes, EXT_VERSION_MISMATCH, RouteKey, parse_session_id};
use crate::agent_model::AgentEvent;

/// The binary id this client reports in `initialize`.
const CLIENT_BINARY_ID: &str = concat!("horizon/", env!("CARGO_PKG_VERSION"));

/// A daemon-side agent session as `session/list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub parent_session_id: Option<SessionId>,
    pub workspace_root: Option<PathBuf>,
}

impl SessionSummary {
    fn from_info(info: v2::SessionInfo) -> Option<Self> {
        let session_id = parse_session_id(&info.session_id)?;
        let meta = read_horizon_meta::<SessionInfoMeta>(info.meta.as_ref())
            .and_then(Result::ok)
            .unwrap_or(SessionInfoMeta {
                workspace_root: Some(info.cwd.0.clone()),
                parent_session_id: None,
                role_id: None,
                provider_id: String::new(),
            });
        Some(Self {
            session_id,
            parent_session_id: meta.parent_session_id,
            workspace_root: meta.workspace_root,
        })
    }
}

/// What `session/new` needs beyond the route.
pub(super) struct SessionNew {
    pub(super) meta: SessionNewMeta,
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
    ListProviders {
        reply: crossbeam_channel::Sender<Result<Vec<ProviderSummary>, String>>,
    },
    ListProviderModels {
        provider: String,
        reply: crossbeam_channel::Sender<Result<Vec<String>, String>>,
    },
    SetSessionModel {
        session_id: SessionId,
        provider: String,
        model: String,
        reply: crossbeam_channel::Sender<Result<(), String>>,
    },
    SetSessionEffort {
        session_id: SessionId,
        /// The level's word.
        level: String,
        reply: crossbeam_channel::Sender<Result<(), String>>,
    },
    EnsureBoardOrganizer {
        root: PathBuf,
        reply: crossbeam_channel::Sender<Result<SessionId, String>>,
    },
    /// Fire-and-forget request to rebuild `[provider]` in the running
    /// daemon without a respawn.
    ReloadProviderConfig,
}

/// How long a child that was asked to end (its input closed) has to flush
/// its logs and exit before it is killed.
const CHILD_EXIT: Duration = Duration::from_secs(5);

/// Starts `binary` as the agent runtime's child and serves `ops` over its
/// standard input and output until the runtime is stopped or the child
/// ends. Its standard error is appended to `log`; its own children reach
/// this Horizon through `HORIZON_SOCKET`, the control socket.
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
            let end = run_stream(output, input, &mut ops, routes.clone(), control.clone()).await;
            match end {
                StreamEnd::Cancelled | StreamEnd::Dropped => {
                    routes.connection_failed("the agent runtime stopped".to_string());
                }
                StreamEnd::GenerationMismatch { message }
                | StreamEnd::VersionRejected { message } => {
                    eprintln!("{message}");
                    routes.connection_failed(format!(
                        "{message} -- restart Horizon to use the rebuilt agent"
                    ));
                }
                StreamEnd::PreHelloTransport { message } | StreamEnd::Silence { message } => {
                    eprintln!("the agent runtime did not start: {message}");
                    routes.connection_failed(message);
                }
                StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                    eprintln!("the agent runtime stopped: {error}");
                    routes.connection_failed(error);
                }
            }
            // The child's input is closed with the connection: it flushes
            // its logs and ends.
            if tokio::time::timeout(CHILD_EXIT, child.wait())
                .await
                .is_err()
            {
                eprintln!("the agent runtime did not end in {CHILD_EXIT:?}; killing it");
                let _ = child.kill().await;
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
    let tasks = routes.clone();
    let session_events = routes.clone();
    let permissions = routes.clone();
    let host_tools = routes.clone();
    Client
        .v2()
        .name("horizon")
        .on_receive_notification(
            async move |notification: v2::UpdateSessionNotification,
                        _connection: V2ConnectionTo<Agent>| {
                updates.route_update(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_notification(
            async move |notification: TaskProgressNotification,
                        _connection: V2ConnectionTo<Agent>| {
                let _ = tasks.deliver(
                    notification.session_id,
                    Inbound::Event(AgentEvent::TaskProgress(notification)),
                );
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_notification(
            async move |notification: SessionEventNotification,
                        _connection: V2ConnectionTo<Agent>| {
                session_events.route_session_event(notification);
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
        .on_receive_request(
            async move |request: HostToolRequest,
                        responder: agent_client_protocol::v2::Responder<
                horizon_acp::HostToolResponse,
            >,
                        _connection: V2ConnectionTo<Agent>| {
                host_tools.host_tool_request(request, responder);
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
}

async fn run_stream<R, W>(
    read: R,
    write: W,
    ops: &mut UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
    control: Arc<RuntimeControl>,
) -> StreamEnd
where
    R: AsyncRead + Send + 'static,
    W: AsyncWrite + Send + 'static,
{
    let transport = ByteStreams::new(write.compat_write(), read.compat());
    let main_control = control.clone();
    let main_routes = routes.clone();
    let establish = control.timeouts().establish;
    routes.clear_version_mismatch();
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
            let end = loop {
                tokio::select! {
                    _ = main_control.cancelled() => break StreamEnd::Cancelled,
                    _ = connection.incoming_closed() => {
                        break StreamEnd::EstablishedFailure(
                            "the agent runtime disconnected".to_string(),
                        );
                    }
                    message = main_routes.version_mismatch() => {
                        main_routes.connection_reset(message.clone());
                        break StreamEnd::VersionRejected { message };
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
    let mut request = v2::InitializeRequest::new(
        ProtocolVersion::V2,
        v2::Implementation::new("horizon", env!("CARGO_PKG_VERSION")),
    );
    let meta = InitializeMeta {
        ext_version: HORIZON_ACP_EXT_VERSION,
        binary_id: CLIENT_BINARY_ID.to_string(),
    };
    write_horizon_meta(&mut request.meta, &meta).expect("InitializeMeta serializes");
    request
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
        Ok(Err(error)) if error.message.starts_with(EXT_VERSION_MISMATCH) => {
            return Err(EstablishError::Rejected(format!(
                "the agent runtime rejected the handshake: {}",
                error.message
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
    match read_horizon_meta::<InitializeMeta>(response.meta.as_ref()) {
        Some(Ok(meta)) if meta.ext_version == HORIZON_ACP_EXT_VERSION => {}
        Some(Ok(meta)) => {
            return Err(EstablishError::Rejected(format!(
                "{EXT_VERSION_MISMATCH}: the agent runtime {} speaks v{}, this shell v{HORIZON_ACP_EXT_VERSION}",
                meta.binary_id, meta.ext_version
            )));
        }
        Some(Err(_)) | None => {
            return Err(EstablishError::Rejected(format!(
                "the agent runtime did not report a horizon extension version (this shell speaks \
                 v{HORIZON_ACP_EXT_VERSION})"
            )));
        }
    }
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
    routes: &AgentRoutes,
    deadline: Duration,
    what: &str,
    request: agent_client_protocol::v2::SentRequest<T>,
) -> Result<T, String> {
    match tokio::time::timeout(deadline, request.block_task()).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            routes.note_error(&error);
            Err(format!("{what} failed: {error}"))
        }
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
                routes.agent_failed(route, "the new session has no workspace root".to_string());
                return;
            };
            let mut request = v2::NewSessionRequest::new(workspace_root);
            if let Err(error) = write_horizon_meta(&mut request.meta, &new.meta) {
                routes.agent_failed(route, format!("failed to encode the new session: {error}"));
                return;
            }
            open_session(routes, route, connection, commands, |connection, routes| {
                connection
                    .build_session_from(request)
                    .start_session()
                    .on_receiving_result(async move |result| {
                        match result {
                            Ok(opened) => {
                                let response = &opened.response;
                                let session_id = route.session_id();
                                if let Some(Ok(meta)) =
                                    read_horizon_meta::<SessionInfoMeta>(response.meta.as_ref())
                                {
                                    routes.route_session_info(session_id, &meta);
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
                                routes.note_error(&error);
                                routes.deliver_to(
                                    route,
                                    Inbound::Opened(Err(format!(
                                        "failed to start the agent session: {error}"
                                    ))),
                                )
                            }
                        }
                        Ok(())
                    })
            });
        }
        Op::AttachAgent { route, commands } => {
            let request =
                v2::ResumeSessionRequest::new(acp_session_id(route.session_id()), resume_cwd())
                    .replay_from(v2::ReplayFrom::from(v2::ReplayFromStart::new()));
            open_session(routes, route, connection, commands, |connection, routes| {
                connection
                    .resume_session_from(request)
                    .start_session()
                    .on_receiving_result(async move |result| {
                        let opened = result.map(|_| ()).map_err(|error| {
                            routes.note_error(&error);
                            format!("failed to attach to the agent session: {error}")
                        });
                        routes.deliver_to(route, Inbound::Opened(opened));
                        Ok(())
                    })
            });
        }
        Op::SessionList { reply } => {
            let connection = connection.clone();
            let routes = routes.clone();
            tokio::spawn(async move {
                let _ = reply.send(list_sessions(&routes, &connection).await);
            });
        }
        Op::ListProviders { reply } => {
            let request = send(connection, ListProvidersRequest {});
            let routes = routes.clone();
            tokio::spawn(async move {
                let result = call(&routes, OP_TIMEOUT, "provider list", request).await;
                let _ = reply.send(result.map(|response| response.providers));
            });
        }
        Op::ListProviderModels { provider, reply } => {
            let request = send(connection, ListProviderModelsRequest { provider });
            let routes = routes.clone();
            tokio::spawn(async move {
                let result = call(&routes, OP_TIMEOUT, "provider model list", request).await;
                let _ = reply.send(result.map(|response| response.models));
            });
        }
        Op::SetSessionModel {
            session_id,
            provider,
            model,
            reply,
        } => {
            let request = v2::SetSessionConfigOptionRequest::new(
                acp_session_id(session_id),
                MODEL_CONFIG_ID,
                v2::SessionConfigOptionValue::id(horizon_acp::encode_model_option_id(
                    &provider, &model,
                )),
            );
            let request = send(connection, request);
            let routes = routes.clone();
            tokio::spawn(async move {
                let result = call(&routes, OP_TIMEOUT, "set session model", request).await;
                let _ = reply.send(result.map(|_| ()));
            });
        }
        Op::SetSessionEffort {
            session_id,
            level,
            reply,
        } => {
            let request = v2::SetSessionConfigOptionRequest::new(
                acp_session_id(session_id),
                EFFORT_CONFIG_ID,
                v2::SessionConfigOptionValue::id(level),
            );
            let request = send(connection, request);
            let routes = routes.clone();
            tokio::spawn(async move {
                let result = call(&routes, OP_TIMEOUT, "set session effort", request).await;
                let _ = reply.send(result.map(|_| ()));
            });
        }
        Op::EnsureBoardOrganizer { root, reply } => {
            let request = send(
                connection,
                EnsureBoardOrganizerRequest {
                    workspace_root: root,
                },
            );
            let routes = routes.clone();
            tokio::spawn(async move {
                let result = call(&routes, OP_TIMEOUT, "board organizer", request).await;
                let _ = reply.send(result.map(|response| response.session_id));
            });
        }
        Op::ReloadProviderConfig => {
            let request = send(connection, ReloadProviderConfigRequest {});
            let routes = routes.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    call(&routes, OP_TIMEOUT, "reload_provider_config", request).await
                {
                    eprintln!(
                        "horizon-the agent runtime client: provider config reload failed: {error}"
                    );
                }
            });
        }
    }
}

/// Opens `route`'s inbound queue, sends the opening request `send`
/// builds (whose response callback must deliver [`Inbound::Opened`]), and
/// starts the attachment task.
fn open_session(
    routes: &Arc<AgentRoutes>,
    route: RouteKey<SessionId>,
    connection: &V2ConnectionTo<Agent>,
    commands: UnboundedReceiver<AgentCommand>,
    send: impl FnOnce(
        &V2ConnectionTo<Agent>,
        Arc<AgentRoutes>,
    ) -> Result<(), agent_client_protocol::Error>,
) {
    let Some(inbound) = routes.open_inbound(route) else {
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

async fn list_sessions(
    routes: &AgentRoutes,
    connection: &V2ConnectionTo<Agent>,
) -> Result<Vec<SessionSummary>, String> {
    let mut summaries = Vec::new();
    let mut cursor = None;
    loop {
        let mut request = v2::ListSessionsRequest::new();
        request.cursor = cursor;
        let response = call(routes, OP_TIMEOUT, "agent list", send(connection, request)).await?;
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
