use super::*;
use agent_client_protocol::ConnectionTo;
use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
use horizon_agent::registry::ProviderRegistry;

/// Serves `state` over an in-process pipe and hands back a client
/// connection without the protocol-version guard, so a test can send
/// anything in any order.
async fn connect(state: Arc<AgentdState>) -> ConnectionTo<Agent> {
    let (daemon_side, client_side) = tokio::io::duplex(1 << 16);
    let (daemon_read, daemon_write) = tokio::io::split(daemon_side);
    tokio::spawn(serve(daemon_read, daemon_write, state, "test-agentd"));
    let (client_read, client_write) = tokio::io::split(client_side);
    let (cx_tx, cx_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(
        Client
            .builder()
            .without_acp_version_guard()
            .connect_with(
                ByteStreams::new(client_write.compat_write(), client_read.compat()),
                async move |cx| {
                    let _ = cx_tx.send(cx.clone());
                    cx.incoming_closed().await;
                    Ok(())
                },
            ),
    );
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

/// Every method but `initialize` (and `_horizon/drain`, which exits the
/// process and so cannot be exercised here) is refused until `initialize`
/// succeeds; a rejected `initialize` leaves them refused, and a connection
/// initializes once.
#[tokio::test]
async fn methods_are_refused_until_initialize_succeeds() {
    let cx = connect(ready_state()).await;

    let refused = cx
        .send_request(v2::ListSessionsRequest::new())
        .block_task()
        .await
        .unwrap_err();
    assert!(refused.message.contains("initialize"), "{refused:?}");

    let mismatch = cx
        .send_request(initialize_request(acp::HORIZON_ACP_EXT_VERSION + 1))
        .block_task()
        .await
        .unwrap_err();
    assert!(
        mismatch.message.starts_with("horizon ext version mismatch"),
        "{mismatch:?}"
    );
    assert!(
        mismatch
            .message
            .contains(&(acp::HORIZON_ACP_EXT_VERSION + 1).to_string())
            && mismatch
                .message
                .contains(&acp::HORIZON_ACP_EXT_VERSION.to_string()),
        "both versions are named: {mismatch:?}"
    );
    assert!(cx
        .send_request(v2::ListSessionsRequest::new())
        .block_task()
        .await
        .is_err());

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

    assert!(cx
        .send_request(initialize_request(acp::HORIZON_ACP_EXT_VERSION))
        .block_task()
        .await
        .is_err());
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
    std::env::set_var("OPENAI_API_KEY", "test-only");
    std::env::remove_var("HORIZON_RIG_MODEL");

    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "auxiliary_provider = \"default\"\n[[providers]]\nname = \"default\"\ndefault_model = \"before-model\"\n").unwrap();

    let agent_config =
        crate::providers::agent_config(&horizon_config::reload_from_path(Some(&config)).unwrap());
    let providers =
        ProviderRegistry::builtin_with_config(agent_config.clone(), SharedDuckdbStore::unavailable());
    let state = Arc::new(AgentdState::new(
        providers,
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
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

    std::fs::write(&config, "auxiliary_provider = \"default\"\n[[providers]]\nname = \"default\"\ndefault_model = \"after-model\"\n").unwrap();
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
