use super::*;
use agent_client_protocol::ConnectionTo;
use horizon_agent::registry::ProviderRegistry;

/// Serves `state` over an in-process pipe and hands back a client
/// connection without the protocol-version guard, so a test can send
/// anything in any order.
async fn connect(state: Arc<AgentdState>) -> ConnectionTo<Agent> {
    let (daemon_side, client_side) = tokio::io::duplex(1 << 16);
    let (daemon_read, daemon_write) = tokio::io::split(daemon_side);
    tokio::spawn(serve(
        daemon_read,
        daemon_write,
        state,
        "test-agentd",
        Arc::new(|| {}),
    ));
    let (client_read, client_write) = tokio::io::split(client_side);
    let (cx_tx, cx_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(agent_client_protocol::Builder::new(Client).connect_with(
        ByteStreams::new(client_write.compat_write(), client_read.compat()),
        async move |cx| {
            let _ = cx_tx.send(cx.clone());
            cx.incoming_closed().await;
            Ok(())
        },
    ));
    cx_rx.await.unwrap()
}

fn initialize_request(ext_version: u32) -> v2::InitializeRequest {
    v2::InitializeRequest::new(
        ProtocolVersion::V2,
        v2::Implementation::new("test-client", "0"),
    )
    .meta(mapping::horizon_meta(&acp::InitializeMeta {
        ext_version,
        binary_id: "test-client".into(),
    }))
}

fn ready_state() -> Arc<AgentdState> {
    let state = crate::session::test_support::test_state();
    // Nothing here runs the startup resume, so open the readiness gate the
    // session methods wait on.
    state.mark_resume_ready();
    state
}

/// Methods are refused until `initialize` succeeds, and a connection
/// initializes once.
#[tokio::test]
async fn methods_are_refused_until_initialize_succeeds() {
    let cx = connect(ready_state()).await;

    assert!(
        cx.send_request(v2::ListSessionsRequest::new())
            .block_task()
            .await
            .is_err()
    );

    let response = cx
        .send_request(initialize_request(acp::HORIZON_ACP_EXT_VERSION))
        .block_task()
        .await
        .expect("a matching extension version initializes");
    assert_eq!(response.protocol_version, ProtocolVersion::V2);
    let meta: acp::InitializeMeta = acp::read_horizon_meta(response.meta.as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(meta.ext_version, acp::HORIZON_ACP_EXT_VERSION);
    assert_eq!(meta.binary_id, "test-agentd");
    assert!(response.capabilities.session.is_some());

    let listed = cx
        .send_request(v2::ListSessionsRequest::new())
        .block_task()
        .await
        .unwrap();
    assert!(listed.sessions.is_empty());

    assert!(
        cx.send_request(initialize_request(acp::HORIZON_ACP_EXT_VERSION))
            .block_task()
            .await
            .is_err()
    );
}

/// A client at another extension version still initializes and learns the
/// daemon's version; every later method but `_horizon/drain` (which exits
/// the process, so it is covered end to end) is refused, naming both
/// versions.
#[tokio::test]
async fn an_ext_version_mismatch_initializes_but_refuses_everything_else() {
    let cx = connect(ready_state()).await;
    let client_version = acp::HORIZON_ACP_EXT_VERSION + 1;

    let response = cx
        .send_request(initialize_request(client_version))
        .block_task()
        .await
        .expect("a v2 client initializes whatever its extension version");
    let meta: acp::InitializeMeta = acp::read_horizon_meta(response.meta.as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(meta.ext_version, acp::HORIZON_ACP_EXT_VERSION);

    for refused in [
        cx.send_request(v2::ListSessionsRequest::new())
            .block_task()
            .await
            .map(drop),
        cx.send_request(acp::ListProvidersRequest {})
            .block_task()
            .await
            .map(drop),
    ] {
        let error = refused.unwrap_err();
        assert!(
            error.message.starts_with("horizon ext version mismatch"),
            "{error:?}"
        );
        assert!(
            error.message.contains(&client_version.to_string())
                && error
                    .message
                    .contains(&acp::HORIZON_ACP_EXT_VERSION.to_string()),
            "both versions are named: {error:?}"
        );
    }
}

/// `_horizon/reload_provider_config` re-reads the config file at the
/// state's `config_path` and rebuilds the registry in place, so the next
/// session sees the new model. Proven on both halves of the swap: the
/// registry's `resolved_model` (which reads the rig provider's rebuilt
/// config) and `agent_config.rig.model` (the judge's base-URL source).
/// `OPENAI_API_KEY` only flips `api_key_present` on so `resolved_model`
/// reports the model; nextest's per-test processes keep the `set_var`
/// from racing another test.
#[tokio::test]
async fn reload_provider_config_rebuilds_the_registry_from_the_config_file() {
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("OPENAI_API_KEY", "test-only") };

    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "default_model = { provider = \"default\", model = \"before-model\" }\n[[providers]]\nname = \"default\"\n").unwrap();

    let agent_config =
        crate::providers::agent_config(&horizon_config::reload_from_path(Some(&config)).unwrap());
    let providers = ProviderRegistry::builtin_with_config(agent_config.clone());
    let state = Arc::new(AgentdState::new(
        providers,
        agent_config,
        None,
        Some(config.clone()),
        Vec::new(),
        Vec::new(),
    ));
    state.mark_resume_ready();
    let cx = connect(state.clone()).await;
    cx.send_request(initialize_request(acp::HORIZON_ACP_EXT_VERSION))
        .block_task()
        .await
        .expect("initialize");

    let provider_id = state.providers.lock().unwrap().default_provider_id();
    assert_eq!(
        state
            .providers
            .lock()
            .unwrap()
            .resolved_model(&provider_id, None),
        Some("before-model".to_string()),
    );

    std::fs::write(&config, "default_model = { provider = \"default\", model = \"after-model\" }\n[[providers]]\nname = \"default\"\n").unwrap();
    cx.send_request(acp::ReloadProviderConfigRequest {})
        .block_task()
        .await
        .expect("reload");

    assert_eq!(
        state
            .providers
            .lock()
            .unwrap()
            .resolved_model(&provider_id, None),
        Some("after-model".to_string()),
    );
    assert_eq!(state.agent_config.lock().unwrap().rig.model, "after-model");
}

/// An extension answers the client request it serves, through the same
/// typed request a client sends; a request nobody serves is refused as
/// unknown, and the connection goes on.
#[tokio::test]
async fn an_extension_answers_its_request_and_an_unknown_one_is_refused() {
    struct Organizer;
    impl crate::extension::Extension for Organizer {
        fn serves(&self, method: &str) -> bool {
            method == "_horizon/ensure_board_organizer"
        }
        fn request(
            &self,
            _state: &Arc<AgentdState>,
            _method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            let request: acp::EnsureBoardOrganizerRequest =
                serde_json::from_value(params).map_err(|error| error.to_string())?;
            assert_eq!(request.workspace_root, std::path::Path::new("/project"));
            let session_id = SessionId::from_uuid(uuid::Uuid::nil());
            serde_json::to_value(acp::EnsureBoardOrganizerResponse { session_id })
                .map_err(|error| error.to_string())
        }
    }
    let state = Arc::into_inner(crate::session::test_support::test_state())
        .expect("the fixture holds the only reference")
        .with_extensions(crate::extension::Extensions::new(vec![Arc::new(Organizer)]));
    let state = Arc::new(state);
    state.mark_resume_ready();
    let cx = connect(state).await;
    cx.send_request(initialize_request(acp::HORIZON_ACP_EXT_VERSION))
        .block_task()
        .await
        .unwrap();

    let answered = cx
        .send_request(acp::EnsureBoardOrganizerRequest {
            workspace_root: "/project".into(),
        })
        .block_task()
        .await
        .expect("the extension serves it");
    assert_eq!(answered.session_id, SessionId::from_uuid(uuid::Uuid::nil()));

    let refused = cx
        .send_request(acp::WatchBoardRequest {
            workspace_root: "/project".into(),
        })
        .block_task()
        .await
        .expect_err("no extension serves it");
    assert_eq!(
        refused.code,
        agent_client_protocol::ErrorCode::METHOD_NOT_FOUND
    );

    cx.send_request(v2::ListSessionsRequest::new())
        .block_task()
        .await
        .expect("the connection goes on");
}
