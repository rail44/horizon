//! The daemon's ACP v2 agent side (`docs/acp-agentd-design.md`): one
//! JSON-RPC connection per accepted socket connection, multiplexing every
//! session by its id, plus the `_horizon/*` extension methods
//! (`crates/horizon-acp`). The process-lifetime state stays in
//! [`crate::session::AgentdState`], reached through the same
//! [`Connection`] seam.
//!
//! `initialize` succeeds for any v2 client. A client at another extension
//! version gets the daemon's version in the reply, and from then on only
//! `_horizon/drain` is served on that connection.
//!
//! Handlers run one at a time on the connection's dispatch loop, so any
//! handler that waits (resume readiness, a replay, a provider listing)
//! answers from a spawned task. Requests to the client (permission prompts,
//! host tools) are sent from spawned tasks as well.

mod attachment;
mod mapping;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::{v2, ProtocolVersion};
use agent_client_protocol::{Agent, ByteStreams, Client, Error, Responder, V2ConnectionTo};
use horizon_acp as acp;
use horizon_agent::contract::{Command, ProviderId, SessionId};
use horizon_agent::hosting::{HostToolRequest, HostToolResponse, SessionNew};
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::roles::RoleId;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::session::{AgentdState, Connection, HOST_TOOL_TIMEOUT};
use crate::DAEMON_NAME;

use attachment::Opening;

/// JSON-RPC error code of a refused call.
const CALL_ERROR_CODE: i32 = -32000;

pub(crate) fn call_error(message: impl Into<String>) -> Error {
    Error::new(CALL_ERROR_CODE, message)
}

fn not_initialized() -> Error {
    Error::new(
        i32::from(agent_client_protocol::ErrorCode::InvalidRequest),
        "initialize must succeed before this method can be used",
    )
}

/// One attachment slot per session on this connection.
struct Slot {
    generation: u64,
    commands: Option<mpsc::UnboundedSender<Command>>,
}

/// One connection's state, shared by its handlers and its attachment pumps.
pub(crate) struct Shared {
    connection: Connection,
    binary_id: &'static str,
    initialized: AtomicBool,
    /// Set when the client initialized at another extension version.
    mismatch: Mutex<Option<String>>,
    slots: Mutex<HashMap<SessionId, Slot>>,
    next_generation: AtomicU64,
}

impl Shared {
    fn new(connection: Connection, binary_id: &'static str) -> Self {
        Self {
            connection,
            binary_id,
            initialized: AtomicBool::new(false),
            mismatch: Mutex::new(None),
            slots: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
        }
    }

    fn gate(&self) -> Result<(), Error> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(not_initialized());
        }
        match self
            .mismatch
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
        {
            Some(message) => Err(Error::new(
                i32::from(agent_client_protocol::ErrorCode::InvalidRequest),
                message.clone(),
            )),
            None => Ok(()),
        }
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Slot>> {
        self.slots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Makes a new attachment of `session_id` current on this connection.
    /// The previous one (if any) stops sending and loses its commands.
    fn claim(&self, session_id: SessionId) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.slots().insert(
            session_id,
            Slot {
                generation,
                commands: None,
            },
        );
        generation
    }

    fn activate(
        &self,
        session_id: SessionId,
        generation: u64,
        commands: mpsc::UnboundedSender<Command>,
    ) {
        if let Some(slot) = self.slots().get_mut(&session_id) {
            if slot.generation == generation {
                slot.commands = Some(commands);
            }
        }
    }

    fn release(&self, session_id: SessionId, generation: u64) {
        let mut slots = self.slots();
        if slots
            .get(&session_id)
            .is_some_and(|slot| slot.generation == generation)
        {
            slots.remove(&session_id);
        }
    }

    fn is_current(&self, session_id: SessionId, generation: u64) -> bool {
        self.slots()
            .get(&session_id)
            .is_some_and(|slot| slot.generation == generation)
    }

    /// Runs `send` while holding the slot table, so a replacement cannot
    /// interleave with a stale attachment's send.
    fn while_current<T>(
        &self,
        session_id: SessionId,
        generation: u64,
        send: impl FnOnce() -> T,
    ) -> Option<T> {
        let slots = self.slots();
        slots
            .get(&session_id)
            .is_some_and(|slot| slot.generation == generation)
            .then(send)
    }

    /// Routes a client command through the session's current attachment on
    /// this connection.
    fn command(&self, session_id: SessionId, command: Command) -> Result<(), Error> {
        self.slots()
            .get(&session_id)
            .and_then(|slot| slot.commands.as_ref())
            .ok_or_else(|| call_error(format!("Session {session_id:?} is not attached")))?
            .send(command)
            .map_err(|_| call_error(format!("Session {session_id:?} is not attached")))
    }

    fn close(&self) {
        self.slots().clear();
    }

    fn session_facts(&self, session_id: SessionId) -> mapping::SessionFacts {
        match self.connection.session_summary(session_id) {
            Some(summary) => mapping::SessionFacts {
                provider_id: summary.provider_id.0,
                role_id: summary.role_id.map(|role| role.0),
                parent_session_id: summary.parent_session_id,
                workspace_root: summary.workspace_root,
            },
            None => mapping::SessionFacts {
                provider_id: String::new(),
                role_id: None,
                parent_session_id: None,
                workspace_root: None,
            },
        }
    }

    fn session_model_options(&self, session_id: SessionId) -> Vec<v2::SessionConfigOption> {
        let provider_id = self.session_facts(session_id).provider_id;
        let (model, selection) = self.connection.session_model_state(session_id);
        mapping::model_options(&provider_id, model.as_deref(), selection.as_ref())
    }

    fn initialize(
        self: &Arc<Self>,
        request: v2::InitializeRequest,
        responder: Responder<v2::InitializeResponse>,
        cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if self.initialized.load(Ordering::Acquire) {
            return responder.respond_with_error(Error::new(
                i32::from(agent_client_protocol::ErrorCode::InvalidRequest),
                "ACP connections may only be initialized once",
            ));
        }
        if request.protocol_version != ProtocolVersion::V2 {
            return responder.respond_with_error(Error::new(
                i32::from(agent_client_protocol::ErrorCode::InvalidRequest),
                format!(
                    "unsupported ACP protocol version {}; {DAEMON_NAME} speaks 2",
                    request.protocol_version
                ),
            ));
        }
        let client = acp::read_horizon_meta::<acp::InitializeMeta>(request.meta.as_ref())
            .and_then(Result::ok);
        let matched =
            client.as_ref().map(|meta| meta.ext_version) == Some(acp::HORIZON_ACP_EXT_VERSION);
        if !matched {
            let client = client
                .map(|meta| meta.ext_version.to_string())
                .unwrap_or_else(|| "none".to_string());
            *self
                .mismatch
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(format!(
                "horizon ext version mismatch: client {client}, daemon {}",
                acp::HORIZON_ACP_EXT_VERSION
            ));
        }
        self.initialized.store(true, Ordering::Release);

        let meta = mapping::horizon_meta(&acp::InitializeMeta {
            ext_version: acp::HORIZON_ACP_EXT_VERSION,
            binary_id: self.binary_id.to_string(),
        });
        responder.respond(
            v2::InitializeResponse::new(
                ProtocolVersion::V2,
                v2::Implementation::new(DAEMON_NAME, env!("CARGO_PKG_VERSION")),
            )
            .capabilities(v2::AgentCapabilities::new().session(v2::SessionCapabilities::new()))
            .meta(meta),
        )?;
        if !matched {
            return Ok(());
        }
        self.connect_host_tools(cx.clone());

        // Startup diagnostics: at most one notice, after the resume
        // finishes.
        let connection = self.connection.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            if let Some(summary) = connection.skipped_lines_summary() {
                let _ =
                    cx.send_notification(acp::SessionEventNotification::SkippedLines { summary });
            }
        });
        Ok(())
    }

    /// Sessions push host-tool requests into the connection-global bridge;
    /// each one is sent to the client from its own task, and its answer
    /// wakes the session thread waiting on it.
    fn connect_host_tools(&self, cx: V2ConnectionTo<Client>) {
        let (requests, mut incoming) = mpsc::unbounded_channel::<HostToolRequest>();
        self.connection.connect_host_tools(requests);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            while let Some(request) = incoming.recv().await {
                let cx = cx.clone();
                let connection = connection.clone();
                tokio::spawn(async move {
                    let sent = cx.send_request(acp::HostToolRequest {
                        request_id: request.request_id.0.clone(),
                        tool_id: request.tool_id,
                        input: request.input.0,
                    });
                    match tokio::time::timeout(HOST_TOOL_TIMEOUT, sent.block_task()).await {
                        Ok(Ok(response)) => {
                            connection.handle_host_tool_response(HostToolResponse {
                                request_id: request.request_id,
                                output: response.output.into(),
                            })
                        }
                        Ok(Err(error)) => {
                            eprintln!("horizon-agentd: host-tool request failed: {error}")
                        }
                        Err(_) => eprintln!("horizon-agentd: host-tool request timed out"),
                    }
                });
            }
        });
    }

    fn new_session(
        self: &Arc<Self>,
        request: v2::NewSessionRequest,
        responder: Responder<v2::NewSessionResponse>,
        cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let meta = match acp::read_horizon_meta::<acp::SessionNewMeta>(request.meta.as_ref()) {
            Some(Ok(meta)) => meta,
            Some(Err(error)) => {
                return responder
                    .respond_with_error(call_error(format!("Invalid _meta.horizon: {error}")))
            }
            None => {
                return responder.respond_with_error(call_error(
                    "session/new needs _meta.horizon with the session id and provider",
                ))
            }
        };
        let new = SessionNew {
            session_id: meta.session_id,
            provider_id: ProviderId(meta.provider_id),
            role_id: meta.role_id.map(RoleId),
            workspace_root: Some(request.cwd.into_inner()),
            spawn_source_session_id: meta.spawn_source_session_id,
            isolate: meta.isolate,
        };
        let this = self.clone();
        tokio::spawn(async move {
            // Readiness-gated: the session's persistence choice is fixed at
            // spawn, and a spawn racing `set_writer` would run without it.
            this.connection.wait_until_resume_ready().await;
            let session_id = new.session_id;
            let connection = this.connection.clone();
            let started = tokio::task::spawn_blocking(move || connection.handle_session_new(new))
                .await
                .map_err(|error| format!("Session startup failed: {error}"))
                .and_then(|started| started);
            match started {
                Ok(()) => {
                    this.open_attachment(session_id, Opening::New(responder), cx)
                        .await
                }
                Err(message) => {
                    let _ = responder.respond_with_error(call_error(message));
                }
            }
        });
        Ok(())
    }

    fn resume_session(
        self: &Arc<Self>,
        request: v2::ResumeSessionRequest,
        responder: Responder<v2::ResumeSessionResponse>,
        cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let Some(session_id) = mapping::parse_session_id(&request.session_id) else {
            return responder.respond_with_error(call_error(format!(
                "Unknown agent session {}",
                request.session_id
            )));
        };
        let this = self.clone();
        tokio::spawn(async move {
            this.connection.wait_until_resume_ready().await;
            this.open_attachment(session_id, Opening::Resume(responder), cx)
                .await;
        });
        Ok(())
    }

    /// The session owner captures history and subscribes at one event
    /// boundary; the pump streams that snapshot before live updates.
    async fn open_attachment(
        self: &Arc<Self>,
        session_id: SessionId,
        opening: Opening,
        cx: V2ConnectionTo<Client>,
    ) {
        let generation = self.claim(session_id);
        match self.connection.attach(session_id).await {
            Ok(bootstrap) => {
                let (commands, incoming) = mpsc::unbounded_channel();
                self.activate(session_id, generation, commands);
                attachment::start(
                    self.clone(),
                    cx,
                    session_id,
                    generation,
                    bootstrap,
                    incoming,
                    opening,
                );
            }
            Err(message) => {
                self.release(session_id, generation);
                opening.fail(message);
            }
        }
    }

    fn list_sessions(
        self: &Arc<Self>,
        _request: v2::ListSessionsRequest,
        responder: Responder<v2::ListSessionsResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let connection = self.connection.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            let fallback = std::env::current_dir().unwrap_or_else(|_| "/".into());
            let sessions = connection
                .session_list()
                .into_iter()
                .map(|summary| {
                    let cwd = summary
                        .workspace_root
                        .clone()
                        .unwrap_or_else(|| fallback.clone());
                    v2::SessionInfo::new(mapping::acp_session_id(summary.session_id), cwd).meta(
                        mapping::horizon_meta(&acp::SessionInfoMeta {
                            workspace_root: summary.workspace_root,
                            parent_session_id: summary.parent_session_id,
                            role_id: summary.role_id.map(|role| role.0),
                            provider_id: summary.provider_id.0,
                        }),
                    )
                })
                .collect();
            let _ = responder.respond(v2::ListSessionsResponse::new(sessions));
        });
        Ok(())
    }

    fn session_command(&self, id: &v2::SessionId, command: Command) -> Result<(), Error> {
        self.gate()?;
        let session_id = mapping::parse_session_id(id)
            .ok_or_else(|| call_error(format!("Unknown agent session {id}")))?;
        self.command(session_id, command)
    }

    fn prompt(
        self: &Arc<Self>,
        request: v2::PromptRequest,
        responder: Responder<v2::PromptResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let text: String = request
            .prompt
            .iter()
            .filter_map(|block| match block {
                v2::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        let result = self
            .session_command(&request.session_id, Command::UserMessage { text })
            .map(|()| v2::PromptResponse::new(uuid::Uuid::new_v4().to_string()));
        answer(responder, result)
    }

    fn close_session(
        self: &Arc<Self>,
        request: v2::CloseSessionRequest,
        responder: Responder<v2::CloseSessionResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let result = self
            .session_command(&request.session_id, Command::Shutdown)
            .map(|()| v2::CloseSessionResponse::new());
        answer(responder, result)
    }

    /// Validation errors are caller bugs or a stale picker's view, so they
    /// are refused rather than ignored. The reply carries the model applied
    /// so far; the switch itself arrives as a `config_option_update`.
    fn set_config_option(
        self: &Arc<Self>,
        request: v2::SetSessionConfigOptionRequest,
        responder: Responder<v2::SetSessionConfigOptionResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let result = (|| {
            self.gate()?;
            let session_id = mapping::parse_session_id(&request.session_id).ok_or_else(|| {
                call_error(format!("Unknown agent session {}", request.session_id))
            })?;
            if &*request.config_id.0 != acp::MODEL_CONFIG_ID {
                return Err(call_error(format!(
                    "Unknown config option {}",
                    request.config_id.0
                )));
            }
            let (provider, model) = request
                .value
                .as_id()
                .and_then(|value| acp::decode_model_option_id(&value.0))
                .ok_or_else(|| call_error("The model option's value must be `provider/model`"))?;
            self.connection
                .set_session_model(session_id, provider.to_string(), model.to_string())
                .map_err(call_error)?;
            Ok(v2::SetSessionConfigOptionResponse::new(
                self.session_model_options(session_id),
            ))
        })();
        answer(responder, result)
    }

    fn continue_turn(
        self: &Arc<Self>,
        request: acp::ContinueTurnRequest,
        responder: Responder<acp::EmptyResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let result = self
            .gate()
            .and_then(|()| self.command(request.session_id, Command::ContinueTurn))
            .map(|()| acp::EmptyResponse {});
        answer(responder, result)
    }

    fn list_providers(
        self: &Arc<Self>,
        _request: acp::ListProvidersRequest,
        responder: Responder<acp::ListProvidersResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let result = self.gate().map(|()| acp::ListProvidersResponse {
            providers: self
                .connection
                .list_providers()
                .into_iter()
                .map(|summary| acp::ProviderSummary {
                    name: summary.name,
                    base_url: summary.base_url,
                    api_key_env: summary.api_key_env,
                    default_model: summary.default_model,
                    available: summary.available,
                    default: summary.default,
                })
                .collect(),
        });
        answer(responder, result)
    }

    /// Never an error: discovery that answers nothing is an empty list.
    fn list_provider_models(
        self: &Arc<Self>,
        request: acp::ListProviderModelsRequest,
        responder: Responder<acp::ListProviderModelsResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let models = connection.list_provider_models(&request.provider).await;
            let _ = responder.respond(acp::ListProviderModelsResponse { models });
        });
        Ok(())
    }

    fn watch_board(
        self: &Arc<Self>,
        request: acp::WatchBoardRequest,
        responder: Responder<acp::EmptyResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let connection = self.connection.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            let result = connection
                .register_board(request.workspace_root)
                .map(|()| acp::EmptyResponse {})
                .map_err(call_error);
            let _ = answer(responder, result);
        });
        Ok(())
    }

    fn ensure_board_organizer(
        self: &Arc<Self>,
        request: acp::EnsureBoardOrganizerRequest,
        responder: Responder<acp::EnsureBoardOrganizerResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let connection = self.connection.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            let result = connection
                .ensure_board_organizer(request.workspace_root)
                .map(|session_id| acp::EnsureBoardOrganizerResponse { session_id })
                .map_err(call_error);
            let _ = answer(responder, result);
        });
        Ok(())
    }

    /// Rebuilds the provider registry from the config file in place. A parse
    /// error keeps the previous registry and is logged; the call succeeds
    /// either way, since no respawn is needed in both cases.
    fn reload_provider_config(
        self: &Arc<Self>,
        _request: acp::ReloadProviderConfigRequest,
        responder: Responder<acp::EmptyResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let result = self.gate().map(|()| {
            if let Err(error) = self.connection.reload_provider_config() {
                eprintln!(
                    "horizon-agentd: provider config reload failed, keeping the previous config: {error}"
                );
            }
            acp::EmptyResponse {}
        });
        answer(responder, result)
    }

    /// Served on a connection at another extension version too: the
    /// recovery such a client uses. Flushes the event log and exits; every
    /// PTY lives in `horizon-terminald`, so nothing else ends with it.
    fn drain(
        self: &Arc<Self>,
        _request: acp::DrainRequest,
        _responder: Responder<acp::EmptyResponse>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        flush_event_log_before_exit(self.connection.writer());
        eprintln!("horizon-agentd: drained, exiting");
        std::process::exit(0);
    }
}

/// Answers `responder` with `result`, turning a refusal into a JSON-RPC
/// error on that request only.
fn answer<T: agent_client_protocol::JsonRpcResponse>(
    responder: Responder<T>,
    result: Result<T, Error>,
) -> Result<(), Error> {
    match result {
        Ok(response) => responder.respond(response),
        Err(error) => responder.respond_with_error(error),
    }
}

/// Serves one accepted connection until the client goes away. A dropped
/// connection is never a session lifecycle event: every attachment on it
/// ends as `Detached` and the sessions keep running.
pub(crate) async fn serve<R, W>(
    read: R,
    write: W,
    state: Arc<AgentdState>,
    binary_id: &'static str,
) -> Result<(), Error>
where
    R: AsyncRead + Send + 'static,
    W: AsyncWrite + Send + 'static,
{
    let connection = Connection::new(state);
    let shared = Arc::new(Shared::new(connection.clone(), binary_id));
    let transport = ByteStreams::new(write.compat_write(), read.compat());

    macro_rules! on_request {
        ($builder:expr, $ty:ty, $method:ident) => {{
            let shared = shared.clone();
            $builder.on_receive_request(
                async move |request: $ty,
                            responder: Responder<
                    <$ty as agent_client_protocol::JsonRpcRequest>::Response,
                >,
                            cx: V2ConnectionTo<Client>| {
                    shared.$method(request, responder, cx)
                },
                agent_client_protocol::on_receive_request!(),
            )
        }};
    }

    let builder = Agent.v2().name(DAEMON_NAME);
    let builder = on_request!(builder, v2::InitializeRequest, initialize);
    let builder = on_request!(builder, v2::NewSessionRequest, new_session);
    let builder = on_request!(builder, v2::ResumeSessionRequest, resume_session);
    let builder = on_request!(builder, v2::ListSessionsRequest, list_sessions);
    let builder = on_request!(builder, v2::PromptRequest, prompt);
    let builder = on_request!(builder, v2::CloseSessionRequest, close_session);
    let builder = on_request!(
        builder,
        v2::SetSessionConfigOptionRequest,
        set_config_option
    );
    let builder = on_request!(builder, acp::ContinueTurnRequest, continue_turn);
    let builder = on_request!(builder, acp::ListProvidersRequest, list_providers);
    let builder = on_request!(
        builder,
        acp::ListProviderModelsRequest,
        list_provider_models
    );
    let builder = on_request!(builder, acp::WatchBoardRequest, watch_board);
    let builder = on_request!(
        builder,
        acp::EnsureBoardOrganizerRequest,
        ensure_board_organizer
    );
    let builder = on_request!(
        builder,
        acp::ReloadProviderConfigRequest,
        reload_provider_config
    );
    let builder = on_request!(builder, acp::DrainRequest, drain);
    let cancel = shared.clone();
    let builder = builder.on_receive_notification(
        async move |notification: v2::CancelSessionNotification, _cx: V2ConnectionTo<Client>| {
            if let Err(error) = cancel.session_command(
                &notification.session_id,
                Command::Cancel { request_id: None },
            ) {
                eprintln!("horizon-agentd: dropping a cancel: {}", error.message);
            }
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );

    let served = builder.connect_to(transport).await;
    shared.close();
    connection.disconnect();
    served
}

/// Flush queued log work on graceful daemon exit. Session publication already
/// waits for its own commit; the exit barrier also covers non-session appends.
pub(crate) fn flush_event_log_before_exit(writer: Option<WriterHandle>) {
    if let Some(writer) = writer {
        if let Err(error) = writer.flush() {
            eprintln!("horizon-agentd: failed to flush event log before draining: {error}");
        }
    }
}

#[cfg(test)]
mod tests;
