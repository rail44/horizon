use super::*;
use agent_client_protocol::ConnectionTo;

/// Serves `state` over an in-process pipe and hands back a client
/// connection without the protocol-version guard, so a test can send
/// anything in any order.
async fn connect(state: Arc<AgentdState>) -> ConnectionTo<Agent> {
    let (daemon_side, client_side) = tokio::io::duplex(1 << 16);
    let (daemon_read, daemon_write) = tokio::io::split(daemon_side);
    tokio::spawn(serve(daemon_read, daemon_write, state));
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

fn initialize_request() -> v2::InitializeRequest {
    v2::InitializeRequest::new(
        ProtocolVersion::V2,
        v2::Implementation::new("test-client", "0"),
    )
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
        .send_request(initialize_request())
        .block_task()
        .await
        .expect("a v2 client initializes");
    assert_eq!(response.protocol_version, ProtocolVersion::V2);
    assert!(response.capabilities.session.is_some());

    let listed = cx
        .send_request(v2::ListSessionsRequest::new())
        .block_task()
        .await
        .unwrap();
    assert!(listed.sessions.is_empty());

    assert!(
        cx.send_request(initialize_request())
            .block_task()
            .await
            .is_err()
    );
}

/// A `_horizon/*` request is refused as unknown, and the connection goes
/// on.
#[tokio::test]
async fn an_unknown_request_is_refused_and_the_connection_goes_on() {
    let state = crate::session::test_support::test_state();
    state.mark_resume_ready();
    let cx = connect(state).await;
    cx.send_request(initialize_request())
        .block_task()
        .await
        .unwrap();

    let refused = cx
        .send_request(
            agent_client_protocol::UntypedMessage::new(
                "_horizon/no_such_method",
                serde_json::json!({}),
            )
            .unwrap(),
        )
        .block_task()
        .await
        .expect_err("nothing serves it");
    assert_eq!(
        refused.code,
        agent_client_protocol::ErrorCode::METHOD_NOT_FOUND
    );

    cx.send_request(v2::ListSessionsRequest::new())
        .block_task()
        .await
        .expect("the connection goes on");
}

/// `session/options` for a new session: the mode first, at no role, then
/// the model a new session starts on.
#[tokio::test]
async fn session_options_say_what_a_new_session_starts_with() {
    let state = crate::session::test_support::test_state();
    state.mark_resume_ready();
    let cx = connect(state).await;
    let initialized = cx
        .send_request(initialize_request())
        .block_task()
        .await
        .expect("initialize");
    assert!(
        (initialized.capabilities.session.as_ref())
            .is_some_and(|session| session.options.is_some()),
        "the agent says it takes initial option values"
    );
    let options = cx
        .send_request(v2::SessionOptionsRequest::new("/project"))
        .block_task()
        .await
        .expect("session/options")
        .config_options;
    let ids: Vec<_> = (options.iter())
        .map(|option| option.config_id.0.to_string())
        .collect();
    assert_eq!(ids[..2], [acp::MODE_CONFIG_ID, acp::MODEL_CONFIG_ID]);
    let v2::SessionConfigKind::Select(mode) = &options[0].kind else {
        panic!("the mode is a select");
    };
    assert_eq!(&*mode.current_value.0, acp::DEFAULT_MODE);
    assert_eq!(
        options[0].category,
        Some(v2::SessionConfigOptionCategory::Mode)
    );
}
