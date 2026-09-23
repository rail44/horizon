use super::*;

struct Peer {
    id: Uuid,
    frames: tokio::sync::watch::Receiver<TerminalFrame>,
    events: crossbeam_channel::Receiver<TerminalUpdate>,
    commands: tokio::sync::mpsc::UnboundedReceiver<TerminalCommand>,
}

fn register(routes: &TerminalRoutes) -> Peer {
    let id = Uuid::new_v4();
    let (frame_tx, frames) = tokio::sync::watch::channel(TerminalFrame::from_text("seed".into()));
    let (event_tx, events) = crossbeam_channel::unbounded();
    let (command_tx, commands) = tokio::sync::mpsc::unbounded_channel();
    routes.register_terminal(id, frame_tx, event_tx, command_tx);
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
