//! Decide the event and reply from the snapshot held by the writer's file lock.

use std::collections::HashMap;

use horizon_board::wire::{IngestReply, IngestRequest, LogError};
use horizon_board::{fold, BoardEvent, Comment, Item, ReadReport};

use super::relationships::{compute_rank, set_parent, validate_dependencies};
use super::{invalid, messages, unix_ms};

pub(super) struct PreparedWrite {
    pub(super) event: Option<BoardEvent>,
    pub(super) reply: IngestReply,
}

impl PreparedWrite {
    pub(super) fn event(event: BoardEvent) -> Self {
        Self {
            event: Some(event),
            reply: IngestReply::Done,
        }
    }

    fn store(item: Item, reply: IngestReply) -> Self {
        Self {
            event: Some(BoardEvent::ItemStored { id: item.id, item }),
            reply,
        }
    }

    pub(super) fn unchanged(reply: IngestReply) -> Self {
        Self { event: None, reply }
    }
}

fn take_item(items: &mut HashMap<u64, Item>, id: u64) -> Result<Item, LogError> {
    // Rank and relationship checks must see the other items only.
    items.remove(&id).ok_or(LogError::ItemNotFound(id))
}

pub(super) fn prepare(
    report: &ReadReport,
    request: IngestRequest,
) -> Result<PreparedWrite, LogError> {
    let mut items = fold(&report.envelopes);
    match request {
        IngestRequest::Add {
            title,
            body,
            parent,
            position,
        } => {
            if let Some(parent) = parent {
                if !items.contains_key(&parent) {
                    return Err(LogError::ItemNotFound(parent));
                }
            }
            let id = report
                .max_id
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| invalid("Task IDs exhausted"))?;
            let item = Item {
                id,
                title,
                body,
                parent,
                rank: compute_rank(&items, parent, &position)?,
                ..Item::default()
            };
            Ok(PreparedWrite::store(item.clone(), IngestReply::Item(item)))
        }
        IngestRequest::AdvanceCursor { consumer, position } => {
            advance_cursor(report, consumer, position)
        }
        IngestRequest::Comment { id, author, text } => {
            take_item(&mut items, id)?;
            Ok(PreparedWrite::event(BoardEvent::MessageAdded {
                id,
                message: Comment {
                    id: format!("message:{}", report.line_count + 1),
                    author,
                    text,
                    at: Some(unix_ms()),
                    source: None,
                },
            }))
        }
        IngestRequest::PostMessage { id, message } => {
            messages::post(&take_item(&mut items, id)?, message)
        }
        IngestRequest::MarkRead {
            id,
            reader,
            message_id,
        } => messages::mark_read(
            &take_item(&mut items, id)?,
            &report.envelopes,
            reader,
            message_id,
        ),
        IngestRequest::SetParent {
            id,
            parent,
            position,
        } => {
            let mut item = take_item(&mut items, id)?;
            set_parent(&items, &mut item, parent, &position)?;
            Ok(PreparedWrite::store(item, IngestReply::Done))
        }
        IngestRequest::SetStatus { id, status } => {
            let mut item = take_item(&mut items, id)?;
            item.status = status;
            Ok(PreparedWrite::store(item, IngestReply::Done))
        }
        IngestRequest::SetClosed {
            id,
            is_closed,
            status,
        } => {
            let mut item = take_item(&mut items, id)?;
            item.is_closed = is_closed;
            if let Some(status) = status {
                item.status = status;
            }
            Ok(PreparedWrite::store(item, IngestReply::Done))
        }
        IngestRequest::SetDependencies { id, depends_on } => {
            let mut item = take_item(&mut items, id)?;
            validate_dependencies(&items, id, &depends_on)?;
            item.depends_on = depends_on;
            Ok(PreparedWrite::store(item, IngestReply::Done))
        }
        IngestRequest::MoveItem { id, position } => {
            let mut item = take_item(&mut items, id)?;
            item.rank = compute_rank(&items, item.parent, &position)?;
            let reply = IngestReply::Rank(item.rank.clone());
            Ok(PreparedWrite::store(item, reply))
        }
        IngestRequest::Edit { id, title, body } => {
            let mut item = take_item(&mut items, id)?;
            if let Some(title) = title {
                item.title = title;
            }
            if let Some(body) = body {
                item.body = body;
            }
            Ok(PreparedWrite::store(item, IngestReply::Done))
        }
        IngestRequest::BindSession {
            id,
            session_id,
            review,
        } => bind_session(take_item(&mut items, id)?, session_id, review),
    }
}

fn advance_cursor(
    report: &ReadReport,
    consumer: String,
    position: u64,
) -> Result<PreparedWrite, LogError> {
    if position > report.line_count {
        return Err(invalid("Cursor exceeds durable log"));
    }
    let old = report
        .envelopes
        .iter()
        .filter_map(|e| match &e.event {
            BoardEvent::CursorAdvanced {
                consumer: c,
                position,
            } if c == &consumer => Some(*position),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    if position <= old {
        return Ok(PreparedWrite::unchanged(IngestReply::Done));
    }
    Ok(PreparedWrite::event(BoardEvent::CursorAdvanced {
        consumer,
        position,
    }))
}

fn bind_session(
    mut item: Item,
    session_id: String,
    review: bool,
) -> Result<PreparedWrite, LogError> {
    if session_id.trim().is_empty() {
        return Err(invalid("Session ID must not be empty"));
    }
    let slot = if review {
        &mut item.review_session_id
    } else {
        &mut item.session_id
    };
    if slot.as_deref() == Some(session_id.as_str()) || (!review && slot.is_some()) {
        return Ok(PreparedWrite::unchanged(IngestReply::Item(item)));
    }
    *slot = Some(session_id);
    Ok(PreparedWrite::store(item.clone(), IngestReply::Item(item)))
}
