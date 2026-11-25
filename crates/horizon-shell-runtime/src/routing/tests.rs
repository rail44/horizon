use super::*;

struct Peer {
    id: RouteKey<Uuid>,
    events: crossbeam_channel::Receiver<TerminalUpdate>,
    commands: tokio::sync::mpsc::UnboundedReceiver<TerminalCommand>,
}

fn register(routes: &TerminalRoutes) -> Peer {
    let id = Uuid::new_v4();
    let (event_tx, events) = crossbeam_channel::unbounded();
    let (command_tx, commands) = tokio::sync::mpsc::unbounded_channel();
    let id = routes.register_terminal(id, event_tx, command_tx);
    Peer {
        id,
        events,
        commands,
    }
}

#[test]
fn exit_retires_all_channels_after_delivering_the_exit() {
    let routes = TerminalRoutes::new();
    let peer = register(&routes);
    routes.route_terminal_update(peer.id, TerminalUpdate::Exited);
    std::assert_matches!(peer.events.try_recv().unwrap(), TerminalUpdate::Exited);
    std::assert_matches!(
        peer.events.try_recv(),
        Err(crossbeam_channel::TryRecvError::Disconnected)
    );
    assert!(peer.commands.is_closed());
}

#[test]
fn a_closed_event_consumer_retires_the_whole_route() {
    let routes = TerminalRoutes::new();
    let peer = register(&routes);
    drop(peer.events);
    routes.route_terminal_update(peer.id, TerminalUpdate::Error("closed".into()));
    assert!(peer.commands.is_closed());
}

#[test]
fn connection_failure_keeps_diagnostics_and_closes_commands() {
    let routes = TerminalRoutes::new();
    let peer = register(&routes);
    routes.connection_failed("runtime gone".into());
    std::assert_matches!(
        peer.events.try_recv().unwrap(), TerminalUpdate::Error(message) if message == "runtime gone"
    );
    assert!(peer.commands.is_closed());
    routes.terminal_failed(peer.id, "later diagnostic".into());
    std::assert_matches!(
        peer.events.try_recv().unwrap(), TerminalUpdate::Error(message) if message == "later diagnostic"
    );

    let later = register(&routes);
    std::assert_matches!(
        later.events.try_recv().unwrap(), TerminalUpdate::Error(message) if message == "runtime gone"
    );
    // (Never registered: nothing more comes.)
    std::assert_matches!(
        later.events.try_recv(),
        Err(crossbeam_channel::TryRecvError::Disconnected)
    );
    assert!(later.commands.is_closed());
    routes.unregister_terminal(peer.id);
    std::assert_matches!(
        peer.events.try_recv(),
        Err(crossbeam_channel::TryRecvError::Disconnected)
    );
}

#[test]
fn dropping_an_old_agent_registration_keeps_the_replacement() {
    let (roots, _roots_rx) = crossbeam_channel::unbounded();
    let routes = AgentRoutes::new(roots);
    let id = SessionId::new();
    let (old_tx, _old_rx) = tokio::sync::mpsc::channel(16);
    let old = routes.register_agent(id, old_tx);
    let (new_tx, mut new_rx) = tokio::sync::mpsc::channel(16);
    let current = routes.register_agent(id, new_tx);
    routes.unregister_agent(old);
    routes.agent_failed(current, "current diagnostic".into());
    std::assert_matches!(
        new_rx.try_recv().unwrap(), AgentUpdate::State(AttachmentState::Failed(message)) if message == "current diagnostic"
    );
}

#[tokio::test]
async fn stale_agent_events_and_workspace_roots_do_not_reach_a_new_attachment() {
    use agent_client_protocol::v2::schema as v2;
    let (roots, roots_rx) = crossbeam_channel::unbounded();
    let routes = AgentRoutes::new(roots);
    let id = SessionId::new();
    let (old_tx, _old_rx) = tokio::sync::mpsc::channel(16);
    let old = routes.register_agent(id, old_tx);
    let _old_inbound = routes.open_inbound(old).unwrap();
    let (new_tx, _new_rx) = tokio::sync::mpsc::channel(16);
    let current = routes.register_agent(id, new_tx);
    assert!(
        routes.open_inbound(old).is_none(),
        "a replaced route opens nothing"
    );
    let mut inbound = routes.open_inbound(current).unwrap();
    routes.agent_failed(old, "stale failure".into());
    routes.deliver_to(old, Inbound::Opened(Ok(())));
    assert!(inbound.try_recv().is_err());

    let mut meta = None;
    horizon_acp::write_horizon_meta(
        &mut meta,
        &SessionInfoMeta {
            workspace_root: Some("/resolved".into()),
            parent_session_id: None,
            role_id: None,
            provider_id: "default".into(),
        },
    )
    .unwrap();
    let mut info = v2::SessionInfoUpdate::new();
    info.meta = agent_client_protocol::schema::MaybeUndefined::Value(meta.unwrap());
    let update = |session: SessionId| {
        v2::UpdateSessionNotification::new(
            session.as_uuid().to_string(),
            v2::SessionUpdate::SessionInfoUpdate(info.clone()),
        )
    };
    routes.route_update(update(SessionId::new()));
    assert!(roots_rx.try_recv().is_err(), "an unregistered session");
    routes.route_update(update(id));
    let (routed, root) = roots_rx.try_recv().unwrap();
    assert_eq!(routed, id);
    assert_eq!(root.workspace_root, std::path::PathBuf::from("/resolved"));
    assert!(matches!(inbound.try_recv(), Ok(Inbound::Event(_))));
}

#[test]
fn stale_terminal_updates_and_cleanup_leave_the_replacement_live() {
    let routes = TerminalRoutes::new();
    let old = register(&routes);
    let (events_tx, events) = crossbeam_channel::unbounded();
    let (commands_tx, commands) = tokio::sync::mpsc::unbounded_channel();
    routes.register_terminal(old.id.session_id(), events_tx, commands_tx);
    routes.route_terminal_update(old.id, TerminalUpdate::Bell);
    routes.terminal_failed(old.id, "stale failure".into());
    routes.route_terminal_update(old.id, TerminalUpdate::Exited);
    routes.unregister_terminal(old.id);
    assert!(events.try_recv().is_err());
    assert!(!commands.is_closed());
}

#[tokio::test]
async fn replacing_a_route_cancels_a_send_blocked_by_a_slow_view() {
    let (roots, _roots_rx) = crossbeam_channel::unbounded();
    let routes = std::sync::Arc::new(AgentRoutes::new(roots));
    let id = SessionId::new();
    let (old_tx, mut old_rx) = tokio::sync::mpsc::channel(1);
    let old = routes.register_agent(id, old_tx);
    assert!(
        routes
            .send_agent(old, AgentUpdate::State(AttachmentState::Restoring))
            .await
    );
    let sender_routes = routes.clone();
    let blocked = tokio::spawn(async move {
        sender_routes
            .send_agent(old, AgentUpdate::State(AttachmentState::Ready))
            .await
    });
    tokio::task::yield_now().await;
    let (new_tx, mut new_rx) = tokio::sync::mpsc::channel(1);
    let current = routes.register_agent(id, new_tx);
    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(1), blocked)
            .await
            .unwrap()
            .unwrap()
    );
    std::assert_matches!(
        old_rx.recv().await,
        Some(AgentUpdate::State(AttachmentState::Restoring))
    );
    assert!(old_rx.recv().await.is_none());
    assert!(
        routes
            .send_agent(current, AgentUpdate::State(AttachmentState::Ready))
            .await
    );
    std::assert_matches!(
        new_rx.recv().await,
        Some(AgentUpdate::State(AttachmentState::Ready))
    );
}
