//! The host's ACP v2 agent side (`docs/agent-stdio-child-design.md`): one
//! JSON-RPC connection, multiplexing every session by its id, plus the `_horizon/*` extension methods
//! (`crates/horizon-acp`). The process-lifetime state stays in
//! [`crate::session::AgentdState`], reached through the same
//! [`Connection`] seam.
//!
//! `initialize` succeeds for any v2 client. A client at another extension
//! version gets the host's version in the reply, and every later request
//! on that connection is refused.
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

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::ConnectionTo as V2ConnectionTo;
use agent_client_protocol::v2::Responder;
use agent_client_protocol::v2::schema as v2;
use agent_client_protocol::{Agent, ByteStreams, Client, Error};
use horizon_acp as acp;
use horizon_agent::config::ReasoningLevel;
use horizon_agent::contract::{Command, ProviderId, SessionId};
use horizon_agent::hosting::SessionNew;
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::roles::RoleId;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::DAEMON_NAME;
use crate::session::{AgentdState, Connection, ForkError};

use attachment::Opening;

/// JSON-RPC error code of a refused call.
const CALL_ERROR_CODE: i32 = -32000;

pub(crate) fn call_error(message: impl Into<String>) -> Error {
    Error::new(CALL_ERROR_CODE, message)
}

fn not_initialized() -> Error {
    Error::new(
        agent_client_protocol::ErrorCode::INVALID_REQUEST,
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
                agent_client_protocol::ErrorCode::INVALID_REQUEST,
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
        if let Some(slot) = self.slots().get_mut(&session_id)
            && slot.generation == generation
        {
            slot.commands = Some(commands);
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
        let (model, selection, reasoning) = self.connection.session_model_state(session_id);
        mapping::model_options(
            &provider_id,
            model.as_deref(),
            selection.as_ref(),
            reasoning.as_ref(),
        )
    }

    fn initialize(
        self: &Arc<Self>,
        request: v2::InitializeRequest,
        responder: Responder<v2::InitializeResponse>,
        cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if self.initialized.load(Ordering::Acquire) {
            return responder.respond_with_error(Error::new(
                agent_client_protocol::ErrorCode::INVALID_REQUEST,
                "ACP connections may only be initialized once",
            ));
        }
        if request.protocol_version != ProtocolVersion::V2 {
            return responder.respond_with_error(Error::new(
                agent_client_protocol::ErrorCode::INVALID_REQUEST,
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
            .capabilities(
                v2::AgentCapabilities::new().session(
                    v2::SessionCapabilities::new().fork(v2::SessionForkCapabilities::new()),
                ),
            )
            .meta(meta),
        )?;
        if !matched {
            return Ok(());
        }

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
                    .respond_with_error(call_error(format!("Invalid _meta.horizon: {error}")));
            }
            None => {
                return responder.respond_with_error(call_error(
                    "session/new needs _meta.horizon with the session id and provider",
                ));
            }
        };
        let new = SessionNew {
            session_id: meta.session_id,
            provider_id: ProviderId(meta.provider_id),
            role_id: meta.role_id.map(RoleId),
            workspace_root: Some(request.cwd.into_inner()),
            spawn_source_session_id: meta.spawn_source_session_id,
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

    /// `session/fork`: a new session from the source's history before a
    /// cursor it announced, or from all of it when it is between turns.
    fn fork_session(
        self: &Arc<Self>,
        request: v2::ForkSessionRequest,
        responder: Responder<v2::ForkSessionResponse>,
        cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let Some(source) = mapping::parse_session_id(&request.session_id) else {
            return responder.respond_with_error(Error::new(
                agent_client_protocol::ErrorCode::INVALID_PARAMS,
                format!("Unknown agent session {}", request.session_id),
            ));
        };
        let cursor = request.cursor_id;
        let workspace_root = request.cwd.into_inner();
        let this = self.clone();
        tokio::spawn(async move {
            this.connection.wait_until_resume_ready().await;
            let connection = this.connection.clone();
            let forked = tokio::task::spawn_blocking(move || {
                connection.handle_session_fork(source, workspace_root, |events| {
                    mapping::fork_point(events, cursor.as_ref())
                })
            })
            .await
            .unwrap_or_else(|error| {
                Err(ForkError::Failed(format!("Session fork failed: {error}")))
            });
            match forked {
                Ok(session_id) => {
                    this.open_attachment(session_id, Opening::Fork(responder), cx)
                        .await
                }
                Err(ForkError::Position(message)) => {
                    let _ = responder.respond_with_error(Error::new(
                        agent_client_protocol::ErrorCode::INVALID_PARAMS,
                        message,
                    ));
                }
                Err(ForkError::Failed(message)) => {
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
            let value = request.value.as_id().map(|value| &*value.0);
            match &*request.config_id.0 {
                acp::MODEL_CONFIG_ID => {
                    let (provider, model) =
                        value.and_then(acp::decode_model_option_id).ok_or_else(|| {
                            call_error("The model option's value must be `provider/model`")
                        })?;
                    self.connection
                        .set_session_model(session_id, provider.to_string(), model.to_string())
                        .map_err(call_error)?;
                }
                acp::EFFORT_CONFIG_ID => {
                    let level = value
                        .ok_or_else(|| call_error("The effort option's value must be a level"))
                        .and_then(|word| ReasoningLevel::parse(word).map_err(call_error))?;
                    self.connection
                        .set_session_reasoning(session_id, level)
                        .map_err(call_error)?;
                }
                other => return Err(call_error(format!("Unknown config option {other}"))),
            }
            Ok(v2::SetSessionConfigOptionResponse::new(
                self.session_model_options(session_id),
            ))
        })();
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
                    "horizon-agent: provider config reload failed, keeping the previous config: {error}"
                );
            }
            acp::EmptyResponse {}
        });
        answer(responder, result)
    }

    /// A request no core handler claims: an extension's
    /// (`crate::extension::Extension::serves`), answered once the sessions
    /// are resumed, or refused as unknown.
    fn extension_request(
        self: &Arc<Self>,
        request: agent_client_protocol::UntypedMessage,
        responder: Responder<serde_json::Value>,
        _cx: V2ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let (method, params) = request.into_parts();
        let Some(extension) = self.connection.extension_serving(&method) else {
            return responder.respond_with_error(Error::method_not_found());
        };
        if let Err(error) = self.gate() {
            return responder.respond_with_error(error);
        }
        let connection = self.connection.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            let result = extension
                .request(connection.state(), &method, params)
                .map_err(call_error);
            let _ = answer(responder, result);
        });
        Ok(())
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
pub async fn serve<R, W>(
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
        ($builder:expr_2021, $ty:ty, $method:ident) => {{
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
    let builder = on_request!(builder, v2::ForkSessionRequest, fork_session);
    let builder = on_request!(builder, v2::ListSessionsRequest, list_sessions);
    let builder = on_request!(builder, v2::PromptRequest, prompt);
    let builder = on_request!(builder, v2::CloseSessionRequest, close_session);
    let builder = on_request!(
        builder,
        v2::SetSessionConfigOptionRequest,
        set_config_option
    );
    let builder = on_request!(builder, acp::ListProvidersRequest, list_providers);
    let builder = on_request!(
        builder,
        acp::ListProviderModelsRequest,
        list_provider_models
    );
    let builder = on_request!(
        builder,
        acp::ReloadProviderConfigRequest,
        reload_provider_config
    );
    let builder = on_request!(
        builder,
        agent_client_protocol::UntypedMessage,
        extension_request
    );
    let cancel = shared.clone();
    let builder = builder.on_receive_notification(
        async move |notification: v2::CancelSessionNotification, _cx: V2ConnectionTo<Client>| {
            if let Err(error) = cancel.session_command(
                &notification.session_id,
                Command::Cancel { request_id: None },
            ) {
                eprintln!("horizon-agent: dropping a cancel: {}", error.message);
            }
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );

    let served = builder.connect_to(transport).await;
    shared.close();
    served
}

/// Flush queued log work on graceful daemon exit. Session publication already
/// waits for its own commit; the exit barrier also covers non-session appends.
pub fn flush_event_log_before_exit(writer: Option<WriterHandle>) {
    if let Some(writer) = writer
        && let Err(error) = writer.flush()
    {
        eprintln!("horizon-agent: failed to flush the session logs before exiting: {error}");
    }
}

#[cfg(test)]
mod tests;
