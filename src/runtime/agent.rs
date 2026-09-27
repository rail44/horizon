//! The `horizon-agentd` client connection: an ACP v2 client over the agentd
//! Unix socket (`docs/acp-agentd-design.md`). Connect, `initialize`,
//! dispatch agent ops as ACP and `_horizon/*` requests, route the inbound
//! traffic by session, and recover from a daemon of another extension
//! version.
//!
//! Terminal traffic has its own connection to its own daemon in
//! [`super::terminal`], so a drain sent from here cannot take a PTY with it.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::{v2, ProtocolVersion};
use agent_client_protocol::{
    is_incoming_transport_closed, Agent, ByteStreams, Client, JsonRpcRequest, V2ConnectionTo,
};
use horizon_acp::{
    read_horizon_meta, write_horizon_meta, DrainRequest, EnsureBoardOrganizerRequest,
    HostToolRequest, InitializeMeta, ListProviderModelsRequest, ListProvidersRequest,
    MemoryNotification, ProviderRequestNotification, ProviderSummary, ReloadProviderConfigRequest,
    SessionEventNotification, SessionId, SessionInfoMeta, SessionNewMeta, TaskProgressNotification,
    ToolCallProgressNotification, WatchBoardRequest, HORIZON_ACP_EXT_VERSION, MODEL_CONFIG_ID,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::attachment::{acp_session_id, AgentCommand, Inbound};
use super::common::{
    establish_timeout, wait_until_refusing, EstablishError, RuntimeControl, StreamEnd, OP_TIMEOUT,
    SILENCE_MISMATCH_THRESHOLD,
};
use super::connection::connect_or_spawn_agentd_retrying;
use super::routing::{parse_session_id, AgentRoutes, RouteKey};
use crate::agent::model::AgentEvent;

/// The message prefix of the daemon's `initialize` rejection when the
/// extension versions differ.
const EXT_VERSION_MISMATCH: &str = "horizon ext version mismatch";

/// The binary id this client reports in `initialize`.
const CLIENT_BINARY_ID: &str = concat!("horizon/", env!("CARGO_PKG_VERSION"));

/// A daemon-side agent session as `session/list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub(crate) session_id: SessionId,
    pub(crate) provider_id: String,
    pub(crate) role_id: Option<String>,
    pub(crate) parent_session_id: Option<SessionId>,
    pub(crate) workspace_root: Option<PathBuf>,
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
            provider_id: meta.provider_id,
            role_id: meta.role_id,
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
    WatchBoard {
        root: PathBuf,
        reply: crossbeam_channel::Sender<Result<(), String>>,
    },
    EnsureBoardOrganizer {
        root: PathBuf,
        reply: crossbeam_channel::Sender<Result<SessionId, String>>,
    },
    Drain,
    /// Fire-and-forget request to rebuild `[provider]` in the running
    /// daemon without a respawn.
    ReloadProviderConfig,
}

pub(super) fn spawn(
    socket_path: PathBuf,
    control_socket: PathBuf,
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
                    "could not start the horizon-agentd client runtime: {error}"
                ));
                control.mark_stopped();
                return;
            }
        };
        runtime.block_on(async {
            let mut mismatch_recovery_attempted = false;
            let mut retry_delay = Duration::from_millis(50);
            let mut consecutive_silences: u32 = 0;
            loop {
                let stream = tokio::select! {
                    result = connect_or_spawn_agentd_retrying(
                        &socket_path,
                        &control_socket,
                    ) => match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!("horizon-agentd initial connection failed: {error}");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    },
                    _ = control.cancelled() => break,
                };

                match run_stream(stream, &mut ops, routes.clone(), control.clone()).await {
                    StreamEnd::PreHelloTransport { message } => {
                        // Transient: retry with backoff, never consuming the
                        // recovery budget.
                        consecutive_silences = 0;
                        eprintln!(
                            "horizon-agentd initialize transport failed, retrying: {message}"
                        );
                        tokio::select! {
                            _ = tokio::time::sleep(retry_delay) => {}
                            _ = control.cancelled() => {
                                routes.connection_failed("agentd runtime stopped".to_string());
                                break;
                            }
                        }
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                        continue;
                    }
                    StreamEnd::Silence { message } => {
                        consecutive_silences += 1;
                        if consecutive_silences < SILENCE_MISMATCH_THRESHOLD {
                            eprintln!(
                                "horizon-agentd did not answer within the establish deadline \
                                 ({consecutive_silences}/{SILENCE_MISMATCH_THRESHOLD} before \
                                 mismatch recovery): {message}"
                            );
                            tokio::select! {
                                _ = tokio::time::sleep(retry_delay) => {}
                                _ = control.cancelled() => {
                                    routes.connection_failed(
                                        "agentd runtime stopped".to_string(),
                                    );
                                    break;
                                }
                            }
                            retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                            continue;
                        }
                        consecutive_silences = 0;
                        if let ControlFlow::Break(()) = recover_generation_mismatch(
                            &message,
                            &mut mismatch_recovery_attempted,
                            &socket_path,
                            &routes,
                            &control,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    StreamEnd::GenerationMismatch { message }
                    | StreamEnd::VersionRejected { message } => {
                        consecutive_silences = 0;
                        if let ControlFlow::Break(()) = recover_generation_mismatch(
                            &message,
                            &mut mismatch_recovery_attempted,
                            &socket_path,
                            &routes,
                            &control,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                        eprintln!("horizon-agentd connection stopped: {error}");
                        routes.connection_failed(error);
                        break;
                    }
                    StreamEnd::Cancelled => {
                        routes.connection_failed("agentd runtime stopped".to_string());
                        break;
                    }
                    StreamEnd::Dropped => break,
                }
            }
        });
        control.mark_stopped();
    });
}

/// The once-per-runtime recovery for a daemon this build cannot initialize
/// with: drain it, then let the caller's next
/// `connect_or_spawn_agentd_retrying` start a fresh binary. `Break` means
/// the runtime must stop (budget already spent, drain failed, or
/// cancelled) and `connection_failed` has been fanned out; `Continue` means
/// the caller should reconnect.
async fn recover_generation_mismatch(
    message: &str,
    mismatch_recovery_attempted: &mut bool,
    socket_path: &Path,
    routes: &Arc<AgentRoutes>,
    control: &Arc<RuntimeControl>,
) -> ControlFlow<()> {
    if *mismatch_recovery_attempted {
        let error = format!(
            "{message} -- automatic drain-and-restart was already attempted \
             once; rebuild horizon-agentd (`cargo build --workspace`) and \
             run `Reload Agent Runtime`"
        );
        eprintln!("horizon-agentd connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    *mismatch_recovery_attempted = true;
    eprintln!("{message}; draining and restarting the daemon");
    let drained = tokio::select! {
        drained = drain_stale_agentd(socket_path) => drained,
        _ = control.cancelled() => {
            routes.connection_failed("agentd runtime stopped".to_string());
            return ControlFlow::Break(());
        }
    };
    if let Err(error) = drained {
        let error = format!("{message} -- and the automatic drain failed: {error}");
        eprintln!("horizon-agentd connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
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
        let end = runtime.block_on(run_stream(
            stream,
            &mut ops,
            routes.clone(),
            control.clone(),
        ));
        match end {
            StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                routes.connection_failed(error)
            }
            // A test stream has no socket to drain and no daemon to respawn,
            // so a mismatch surfaces as a terminal failure instead.
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
) -> agent_client_protocol::V2Builder<
    Client,
    impl agent_client_protocol::HandleDispatchFrom<Agent>,
    agent_client_protocol::NullRun,
> {
    let updates = routes.clone();
    let tasks = routes.clone();
    let progress = routes.clone();
    let memory = routes.clone();
    let session_events = routes.clone();
    let provider_requests = routes.clone();
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
            async move |notification: ToolCallProgressNotification,
                        _connection: V2ConnectionTo<Agent>| {
                let _ = progress.deliver(
                    notification.session_id,
                    Inbound::Event(AgentEvent::ToolCallProgress(notification.event)),
                );
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_notification(
            async move |notification: MemoryNotification, _connection: V2ConnectionTo<Agent>| {
                let _ = memory.deliver(
                    notification.session_id,
                    Inbound::Event(AgentEvent::Memory(notification.event)),
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
        .on_receive_notification(
            async move |notification: ProviderRequestNotification,
                        _connection: V2ConnectionTo<Agent>| {
                let _ = provider_requests.deliver(
                    notification.session_id,
                    Inbound::Event(AgentEvent::ProviderRequest(notification.event)),
                );
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: v2::RequestPermissionRequest,
                        responder: agent_client_protocol::Responder<
                v2::RequestPermissionResponse,
            >,
                        _connection: V2ConnectionTo<Agent>| {
                permissions.route_permission(request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: HostToolRequest,
                        responder: agent_client_protocol::Responder<
                horizon_acp::HostToolResponse,
            >,
                        _connection: V2ConnectionTo<Agent>| {
                host_tools.host_tool_request(request, responder);
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
}

async fn run_stream<S>(
    stream: S,
    ops: &mut UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
    control: Arc<RuntimeControl>,
) -> StreamEnd
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let transport = ByteStreams::new(write_half.compat_write(), read_half.compat());
    let main_control = control.clone();
    let main_routes = routes.clone();
    let result = client(&routes)
        .connect_with(transport, async move |connection: V2ConnectionTo<Agent>| {
            let initialized = tokio::select! {
                result = initialize(&connection) => result,
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
                            "established agentd disconnected".to_string(),
                        );
                    }
                    op = ops.recv() => {
                        let Some(op) = op else {
                            break StreamEnd::Dropped;
                        };
                        handle_op(op, &connection, &main_routes);
                    }
                }
            };
            Ok(end)
        })
        .await;
    match result {
        Ok(end) => end,
        Err(error) if control.is_established() => {
            StreamEnd::EstablishedFailure(format!("agentd connection failed: {error}"))
        }
        Err(error) => StreamEnd::PreHelloTransport {
            message: format!("agentd connection failed during initialize: {error}"),
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
async fn initialize(connection: &V2ConnectionTo<Agent>) -> Result<(), EstablishError> {
    let timeout = establish_timeout();
    let response = match tokio::time::timeout(
        timeout,
        connection.send_request(initialize_request()).block_task(),
    )
    .await
    {
        Err(_elapsed) => {
            return Err(EstablishError::Silence(format!(
                "agentd did not answer initialize within {timeout:?}"
            )))
        }
        Ok(Err(error)) if error.message.starts_with(EXT_VERSION_MISMATCH) => {
            return Err(EstablishError::Rejected(format!(
                "agentd rejected the handshake: {}",
                error.message
            )))
        }
        Ok(Err(error)) if is_incoming_transport_closed(&error) => {
            return Err(EstablishError::Transient(format!(
                "the connection dropped during initialize: {error}"
            )))
        }
        Ok(Err(error)) => {
            return Err(EstablishError::Fatal(format!(
                "agentd answered initialize with an unexpected error: {error}"
            )))
        }
        Ok(Ok(response)) => response,
    };
    match read_horizon_meta::<InitializeMeta>(response.meta.as_ref()) {
        Some(Ok(meta)) if meta.ext_version == HORIZON_ACP_EXT_VERSION => {}
        Some(Ok(meta)) => {
            return Err(EstablishError::Rejected(format!(
            "{EXT_VERSION_MISMATCH}: agentd {} speaks v{}, this shell v{HORIZON_ACP_EXT_VERSION}",
            meta.binary_id, meta.ext_version
        )))
        }
        Some(Err(_)) | None => {
            return Err(EstablishError::Rejected(format!(
                "agentd did not report a horizon extension version (this shell speaks \
                 v{HORIZON_ACP_EXT_VERSION})"
            )))
        }
    }
    if response.capabilities.session.is_none() {
        return Err(EstablishError::Fatal(
            "agentd did not advertise the v2 session capability".to_string(),
        ));
    }
    Ok(())
}

/// Bounds one established-phase request. A deadline expiry fails only that
/// request; the connection stays up.
async fn call<T>(
    deadline: Duration,
    what: &str,
    request: agent_client_protocol::SentRequest<T>,
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
) -> agent_client_protocol::SentRequest<R::Response> {
    connection.send_request(request)
}

fn resume_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Dispatches one op. Each request is awaited on its own task, so a slow
/// one does not stall the others.
fn handle_op(op: Op, connection: &V2ConnectionTo<Agent>, routes: &Arc<AgentRoutes>) {
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
                                let response = opened.response();
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
                            Err(error) => routes.deliver_to(
                                route,
                                Inbound::Opened(Err(format!(
                                    "failed to start the agent session: {error}"
                                ))),
                            ),
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
                            format!("failed to attach to the agent session: {error}")
                        });
                        routes.deliver_to(route, Inbound::Opened(opened));
                        Ok(())
                    })
            });
        }
        Op::SessionList { reply } => {
            let connection = connection.clone();
            tokio::spawn(async move {
                let _ = reply.send(list_sessions(&connection).await);
            });
        }
        Op::ListProviders { reply } => {
            let request = send(connection, ListProvidersRequest {});
            tokio::spawn(async move {
                let result = call(OP_TIMEOUT, "provider list", request).await;
                let _ = reply.send(result.map(|response| response.providers));
            });
        }
        Op::ListProviderModels { provider, reply } => {
            let request = send(connection, ListProviderModelsRequest { provider });
            tokio::spawn(async move {
                let result = call(OP_TIMEOUT, "provider model list", request).await;
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
            tokio::spawn(async move {
                let result = call(OP_TIMEOUT, "set session model", request).await;
                let _ = reply.send(result.map(|_| ()));
            });
        }
        Op::WatchBoard { root, reply } => {
            let request = send(
                connection,
                WatchBoardRequest {
                    workspace_root: root,
                },
            );
            tokio::spawn(async move {
                let result = call(OP_TIMEOUT, "watch board", request).await;
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
            tokio::spawn(async move {
                let result = call(OP_TIMEOUT, "board organizer", request).await;
                let _ = reply.send(result.map(|response| response.session_id));
            });
        }
        Op::Drain => {
            let request = send(connection, DrainRequest {});
            tokio::spawn(async move {
                // The daemon exits inside this call, so the reply usually
                // never arrives; the caller observes the socket refusing
                // connections instead.
                let _ = tokio::time::timeout(establish_timeout(), request.block_task()).await;
            });
        }
        Op::ReloadProviderConfig => {
            let request = send(connection, ReloadProviderConfigRequest {});
            tokio::spawn(async move {
                if let Err(error) = call(OP_TIMEOUT, "reload_provider_config", request).await {
                    eprintln!("horizon-agentd client: provider config reload failed: {error}");
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

/// Gracefully stops a running agentd this build could not initialize with:
/// a fresh connection, `initialize`, then `_horizon/drain`.
async fn drain_stale_agentd(socket_path: &Path) -> Result<(), String> {
    let stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(stream) => stream,
        Err(_) => return Ok(()),
    };
    let (read_half, write_half) = stream.into_split();
    let transport = ByteStreams::new(write_half.compat_write(), read_half.compat());
    let drained = tokio::time::timeout(
        establish_timeout() * 2,
        Client.v2().name("horizon-drain").connect_with(
            transport,
            async |connection: V2ConnectionTo<Agent>| {
                if let Err(error) = connection
                    .send_request(initialize_request())
                    .block_task()
                    .await
                {
                    return Ok(Err(format!("initialize for the drain failed: {error}")));
                }
                let _ = tokio::time::timeout(
                    establish_timeout(),
                    connection.send_request(DrainRequest {}).block_task(),
                )
                .await;
                Ok(Ok(()))
            },
        ),
    )
    .await;
    match drained {
        Ok(Ok(Err(error))) => eprintln!("drain of the incompatible agentd failed: {error}"),
        Ok(Err(error)) => eprintln!("drain connection to the incompatible agentd failed: {error}"),
        Ok(Ok(Ok(()))) | Err(_) => {}
    }
    if wait_until_refusing(socket_path).await {
        Ok(())
    } else {
        Err(
            "horizon-agentd kept accepting connections after the drain attempt; \
             stop it manually"
                .to_string(),
        )
    }
}
