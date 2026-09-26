//! One attachment's private replay and duplex live transport.
use crate::session::Bootstrap;
use horizon_agent::contract::Command;
use horizon_agent::wire::{AgentAttachment, AgentWireEvent, AttachmentEnd};
use horizon_wire::{WireCodec, CHANNEL_BUFFER, COMMAND_MAX_ITEM_BYTES, TOOL_IO_MAX_ITEM_BYTES};
use remoc::rch;

/// One task owns both directions and the subscription lease. Any exit
/// (including a blocked replay send losing its peer) revokes the lease.
pub(super) fn start(bootstrap: Bootstrap) -> AgentAttachment {
    let (event_tx, event_rx) = rch::mpsc::channel::<AgentWireEvent, WireCodec>(CHANNEL_BUFFER);
    let event_rx = event_rx.set_max_item_size::<TOOL_IO_MAX_ITEM_BYTES>();
    let (mut command_tx, mut command_rx) = rch::mpsc::channel::<Command, WireCodec>(CHANNEL_BUFFER);
    command_tx.set_max_item_size(COMMAND_MAX_ITEM_BYTES);
    tokio::spawn(async move {
        let Bootstrap {
            history,
            metadata,
            mut events,
            mut ended,
            lease,
        } = bootstrap;
        let pump = async {
            // Never route replay through send_session_event: it is not a
            // newly produced event and must not reach internal observers.
            event_tx.send(AgentWireEvent::ReplayStarted).await?;
            for event in history {
                event_tx.send(AgentWireEvent::Event(event)).await?;
            }
            for event in metadata {
                event_tx.send(event).await?;
            }
            event_tx.send(AgentWireEvent::ReplayComplete).await?;
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Some(event) => { event_tx.send(event).await?; },
                        None => return Ok::<_, remoc::rch::mpsc::SendError<AgentWireEvent>>(AttachmentEnd::SessionEnded),
                    },
                    command = command_rx.recv() => match command {
                        Ok(Some(command)) => { if !lease.command(command) { return Ok(AttachmentEnd::Detached); } },
                        _ => return Ok(AttachmentEnd::Detached),
                    },
                }
            }
        };
        let reason = tokio::select! {
            biased;
            reason = ended.wait_for(|end| end.is_some()) => reason.ok().and_then(|end| *end),
            _ = event_tx.closed() => None,
            result = pump => result.ok(),
        };
        // Revoke before attempting a best-effort final diagnostic. Even a
        // client that stops reading cannot keep command acceptance alive.
        drop(lease);
        if let Some(reason) = reason {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                event_tx.send(AgentWireEvent::AttachmentClosed(reason)),
            )
            .await;
        }
    });
    AgentAttachment {
        events: event_rx,
        commands: command_tx,
    }
}
