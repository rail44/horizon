use super::*;

struct Peer {
    id: RouteKey<Uuid>,
    frames: tokio::sync::watch::Receiver<TerminalFrame>,
    events: crossbeam_channel::Receiver<TerminalUpdate>,
    commands: tokio::sync::mpsc::UnboundedReceiver<TerminalCommand>,
}

fn register(routes: &TerminalRoutes) -> Peer {
    let id = Uuid::new_v4();
    let (frame_tx, frames) = tokio::sync::watch::channel(TerminalFrame::from_text("seed".into()));
    let (event_tx, events) = crossbeam_channel::unbounded();
    let (command_tx, commands) = tokio::sync::mpsc::unbounded_channel();
    let id = routes.register_terminal(id, frame_tx, event_tx, command_tx);
    Peer {
        id,
        frames,
        events,
        commands,
    }
}

#[test]
fn exit_retires_all_channels_after_delivering_the_exit() {
    let routes = TerminalRoutes::new();
    let peer = register(&routes);
    routes.route_terminal_update(peer.id, TerminalUpdate::Exited);
    assert!(matches!(
        peer.events.try_recv().unwrap(),
        TerminalUpdate::Exited
    ));
    assert!(matches!(
        peer.events.try_recv(),
        Err(crossbeam_channel::TryRecvError::Disconnected)
    ));
    assert!(peer.frames.has_changed().is_err());
    assert!(peer.commands.is_closed());
}

#[test]
fn a_closed_frame_or_event_consumer_retires_the_whole_route() {
    let routes = TerminalRoutes::new();
    let peer = register(&routes);
    drop(peer.frames);
    routes.route_terminal_frame(peer.id, TerminalFrame::from_text("next".into()));
    assert!(matches!(
        peer.events.try_recv(),
        Err(crossbeam_channel::TryRecvError::Disconnected)
    ));
    assert!(peer.commands.is_closed());

    let peer = register(&routes);
    drop(peer.events);
    routes.route_terminal_update(peer.id, TerminalUpdate::Error("closed".into()));
    assert!(peer.frames.has_changed().is_err());
    assert!(peer.commands.is_closed());
}

#[test]
fn connection_failure_keeps_diagnostics_and_closes_commands() {
    let routes = TerminalRoutes::new();
    let peer = register(&routes);
    routes.connection_failed("runtime gone".into());
    assert!(
        matches!(peer.events.try_recv().unwrap(), TerminalUpdate::Error(message) if message == "runtime gone")
    );
    assert!(peer.frames.has_changed().is_ok());
    assert!(peer.commands.is_closed());
    routes.terminal_failed(peer.id, "later diagnostic".into());
    assert!(
        matches!(peer.events.try_recv().unwrap(), TerminalUpdate::Error(message) if message == "later diagnostic")
    );

    let later = register(&routes);
    assert!(
        matches!(later.events.try_recv().unwrap(), TerminalUpdate::Error(message) if message == "runtime gone")
    );
    assert!(later.frames.has_changed().is_err());
    assert!(later.commands.is_closed());
    routes.unregister_terminal(peer.id);
    assert!(peer.frames.has_changed().is_err());
}

#[test]
fn dropping_an_old_agent_registration_keeps_the_replacement() {
    let (host, _host_rx) = crossbeam_channel::unbounded();
    let (roots, _roots_rx) = crossbeam_channel::unbounded();
    let routes = AgentRoutes::new(host, roots);
    let id = contract::SessionId::new();
    let (old_tx, _old_rx) = crossbeam_channel::unbounded();
    let old = routes.register_agent(id, old_tx);
    let (new_tx, new_rx) = crossbeam_channel::unbounded();
    let current = routes.register_agent(id, new_tx);
    routes.unregister_agent(old);
    routes.agent_failed(current, "current diagnostic".into());
    assert!(
        matches!(new_rx.try_recv().unwrap().event, Event::Error(error) if error.message == "current diagnostic")
    );
}

#[test]
fn stale_agent_events_and_workspace_roots_do_not_reach_a_new_attachment() {
    let (host, _host_rx) = crossbeam_channel::unbounded();
    let (roots, roots_rx) = crossbeam_channel::unbounded();
    let routes = AgentRoutes::new(host, roots);
    let id = contract::SessionId::new();
    let (old_tx, _old_rx) = crossbeam_channel::unbounded();
    let old = routes.register_agent(id, old_tx);
    let (new_tx, new_rx) = crossbeam_channel::unbounded();
    let current = routes.register_agent(id, new_tx);
    routes.agent_failed(old, "stale failure".into());
    let root = wire::WorkspaceRootResolved {
        workspace_root: "/stale".into(),
        parent_session_id: None,
    };
    routes.route_agent_event(old, AgentWireEvent::WorkspaceRootResolved(root.clone()));
    assert!(new_rx.try_recv().is_err());
    assert!(roots_rx.try_recv().is_err());
    routes.route_agent_event(current, AgentWireEvent::WorkspaceRootResolved(root));
    assert_eq!(roots_rx.try_recv().unwrap().0, id);
}

#[test]
fn stale_terminal_updates_and_cleanup_leave_the_replacement_live() {
    let routes = TerminalRoutes::new();
    let old = register(&routes);
    let (frames_tx, frames) =
        tokio::sync::watch::channel(TerminalFrame::from_text("current".into()));
    let (events_tx, events) = crossbeam_channel::unbounded();
    let (commands_tx, commands) = tokio::sync::mpsc::unbounded_channel();
    routes.register_terminal(old.id.session_id(), frames_tx, events_tx, commands_tx);
    routes.route_terminal_frame(old.id, TerminalFrame::from_text("stale".into()));
    routes.terminal_failed(old.id, "stale failure".into());
    routes.route_terminal_update(old.id, TerminalUpdate::Exited);
    routes.unregister_terminal(old.id);
    assert_eq!(frames.borrow().text(), "current");
    assert!(events.try_recv().is_err());
    assert!(!commands.is_closed());
}
