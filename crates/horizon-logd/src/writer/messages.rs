//! Message identity and monotonic read positions within an item's conversation.

use horizon_board::wire::{IngestReply, LogError};
use horizon_board::{read_position_advances, BoardEvent, Comment, Envelope, Item};

use super::invalid;
use super::prepare::PreparedWrite;

pub(super) fn post(item: &Item, message: Comment) -> Result<PreparedWrite, LogError> {
    if message.id.is_empty() {
        return Err(invalid("Message ID must not be empty"));
    }
    if let Some(existing) = item.comments.iter().find(|m| {
        m.id == message.id
            || message
                .source
                .as_ref()
                .is_some_and(|s| m.source.as_ref() == Some(s))
    }) {
        if existing != &message {
            return Err(invalid("Message identity already has different content"));
        }
        return Ok(PreparedWrite::unchanged(IngestReply::Done));
    }
    Ok(PreparedWrite::event(BoardEvent::MessageAdded {
        id: item.id,
        message,
    }))
}

pub(super) fn mark_read(
    item: &Item,
    history: &[Envelope],
    reader: String,
    message_id: String,
) -> Result<PreparedWrite, LogError> {
    if !item.comments.iter().any(|message| message.id == message_id) {
        return Err(invalid("Read message must identify an existing message"));
    }
    let mut current: Option<&str> = None;
    for envelope in history {
        if let BoardEvent::ReadAdvanced {
            id,
            reader: owner,
            message_id: seen,
        } = &envelope.event
        {
            if *id == item.id && owner == &reader && read_position_advances(item, current, seen) {
                current = Some(seen);
            }
        }
    }
    if !read_position_advances(item, current, &message_id) {
        return Ok(PreparedWrite::unchanged(IngestReply::Done));
    }
    Ok(PreparedWrite::event(BoardEvent::ReadAdvanced {
        id: item.id,
        reader,
        message_id,
    }))
}
