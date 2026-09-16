//! The board write path, moved from `horizon-board`'s `Store` to logd
//! (`docs/logd-design.md` v1). The exclusive-flock + read-fold + id/rank
//! computation + append sequence is unchanged; it just lives in the daemon
//! now instead of in each short-lived client process.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use horizon_board::wire::{IngestReply, IngestRequest, LogError};
use horizon_board::{
    fold, rank_between, read_events, sorted_by_rank, BoardEvent, Envelope, Item, Position,
    ReadReport, SCHEMA, VERSION,
};

/// Advisory exclusive lock via `flock(2)`. Held until the file is dropped
/// (the kernel releases it on close). Used across the read-fold-append
/// sequence so concurrent clients serialise on the same events file.
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn make_envelope(event: BoardEvent) -> Envelope {
    Envelope {
        schema: SCHEMA.to_string(),
        version: VERSION,
        at: unix_ms(),
        event,
    }
}

/// Opens the file for writing (create + append), acquires an exclusive lock,
/// and reads the current event log. The lock is held until the returned
/// `File` is dropped.
fn open_locked(path: &Path) -> Result<(File, ReadReport), LogError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_err)?;
    lock_exclusive(&file).map_err(io_err)?;
    let report = read_events(path).map_err(io_err)?;
    Ok((file, report))
}

fn append(file: &mut File, env: &Envelope) -> Result<(), LogError> {
    serde_json::to_writer(&mut *file, env).map_err(json_err)?;
    file.write_all(b"\n").map_err(io_err)?;
    file.flush().map_err(io_err)?;
    Ok(())
}

fn io_err(e: std::io::Error) -> LogError {
    LogError::Io(e.to_string())
}

fn json_err(e: serde_json::Error) -> LogError {
    LogError::Io(e.to_string())
}

fn invalid(text: &str) -> LogError {
    LogError::InvalidOperation(text.into())
}
fn compute_rank(
    items: &HashMap<u64, Item>,
    parent: Option<u64>,
    position: &Position,
) -> Result<String, LogError> {
    let siblings: HashMap<_, _> = items
        .iter()
        .filter(|(_, i)| i.parent == parent)
        .map(|(id, i)| (*id, i.clone()))
        .collect();
    let sorted = sorted_by_rank(&siblings);
    let (lo, hi) = match position {
        Position::Top => (None, sorted.first().map(|i| i.rank.as_str())),
        Position::Bottom => (sorted.last().map(|i| i.rank.as_str()), None),
        Position::After(id) | Position::Before(id) => {
            let index = sorted
                .iter()
                .position(|i| i.id == *id)
                .ok_or_else(|| invalid("Reorder target must be another sibling"))?;
            if matches!(position, Position::After(_)) {
                (
                    Some(sorted[index].rank.as_str()),
                    sorted.get(index + 1).map(|i| i.rank.as_str()),
                )
            } else {
                (
                    index.checked_sub(1).map(|n| sorted[n].rank.as_str()),
                    Some(sorted[index].rank.as_str()),
                )
            }
        }
    };
    rank_between(lo, hi).ok_or(LogError::RankExhausted)
}
fn validate_dependencies(
    items: &HashMap<u64, Item>,
    id: u64,
    dependencies: &[u64],
) -> Result<(), LogError> {
    let mut unique = std::collections::HashSet::new();
    for dependency in dependencies {
        if !unique.insert(dependency) {
            return Err(invalid("Duplicate dependency"));
        }
        let mut pending = vec![*dependency];
        let mut seen = std::collections::HashSet::new();
        while let Some(next) = pending.pop() {
            if next == id {
                return Err(invalid("Dependency cycle"));
            }
            if seen.insert(next) {
                let item = items.get(&next).ok_or(LogError::ItemNotFound(next))?;
                pending.extend(&item.depends_on);
            }
        }
    }
    Ok(())
}
/// Serializes validation and append under the board's exclusive file lock.
pub fn perform(path: &Path, request: IngestRequest) -> Result<(IngestReply, Vec<u64>), LogError> {
    let (mut file, report) = open_locked(path)?;
    if report.corrupt_count > 0 || report.skipped_count > 0 || report.torn_trailing {
        return Err(invalid("Board contains unreadable or legacy records; import or repair an isolated copy before writing"));
    }
    let mut items = fold(&report.envelopes);
    let seq = report.line_count + 1;
    let (event, reply) = match request {
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
            let rank = compute_rank(&items, parent, &position)?;
            let item = Item {
                id,
                title,
                body,
                parent,
                rank,
                ..Item::default()
            };
            (
                BoardEvent::ItemStored {
                    id,
                    item: item.clone(),
                },
                IngestReply::Item(item),
            )
        }
        IngestRequest::AdvanceCursor { consumer, position } => {
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
                return Ok((IngestReply::Done, vec![]));
            }
            (
                BoardEvent::CursorAdvanced { consumer, position },
                IngestReply::Done,
            )
        }
        request => {
            let id = match &request {
                IngestRequest::SetParent { id, .. }
                | IngestRequest::Comment { id, .. }
                | IngestRequest::SetStatus { id, .. }
                | IngestRequest::MoveItem { id, .. }
                | IngestRequest::Edit { id, .. }
                | IngestRequest::SetCompleted { id, .. }
                | IngestRequest::SetDependencies { id, .. }
                | IngestRequest::BindSession { id, .. }
                | IngestRequest::PostMessage { id, .. }
                | IngestRequest::MarkRead { id, .. } => *id,
                _ => unreachable!(),
            };
            let mut item = items.remove(&id).ok_or(LogError::ItemNotFound(id))?;
            let mut reply = IngestReply::Done;
            let special = match request {
                IngestRequest::Comment { author, text, .. } => Some(BoardEvent::MessageAdded {
                    id,
                    message: horizon_board::Comment {
                        id: format!("message:{seq}"),
                        author,
                        text,
                        at: Some(unix_ms()),
                        source: None,
                    },
                }),
                IngestRequest::PostMessage { message, .. } => {
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
                        return Ok((IngestReply::Done, vec![]));
                    }
                    Some(BoardEvent::MessageAdded { id, message })
                }
                IngestRequest::MarkRead {
                    reader, message_id, ..
                } => {
                    if !item.comments.iter().any(|message| message.id == message_id) {
                        return Err(invalid("Read message must identify an existing message"));
                    }
                    let mut current: Option<&str> = None;
                    for envelope in &report.envelopes {
                        if let BoardEvent::ReadAdvanced {
                            id: task,
                            reader: owner,
                            message_id: seen,
                        } = &envelope.event
                        {
                            if *task == id
                                && owner == &reader
                                && horizon_board::read_position_advances(&item, current, seen)
                            {
                                current = Some(seen);
                            }
                        }
                    }
                    if !horizon_board::read_position_advances(&item, current, &message_id) {
                        return Ok((IngestReply::Done, vec![]));
                    }
                    Some(BoardEvent::ReadAdvanced {
                        id,
                        reader,
                        message_id,
                    })
                }
                IngestRequest::SetParent {
                    parent, position, ..
                } => {
                    let mut ancestor = parent;
                    let mut seen = std::collections::HashSet::new();
                    while let Some(next) = ancestor {
                        if next == id || !seen.insert(next) {
                            return Err(invalid("Parent cycle"));
                        }
                        ancestor = items.get(&next).ok_or(LogError::ItemNotFound(next))?.parent;
                    }
                    item.rank = compute_rank(&items, parent, &position)?;
                    item.parent = parent;
                    None
                }
                IngestRequest::SetStatus { status, .. } => {
                    item.status = status;
                    None
                }
                IngestRequest::SetCompleted { completed, .. } => {
                    item.completed = completed;
                    None
                }
                IngestRequest::SetDependencies { depends_on, .. } => {
                    validate_dependencies(&items, id, &depends_on)?;
                    item.depends_on = depends_on;
                    None
                }
                IngestRequest::MoveItem { position, .. } => {
                    item.rank = compute_rank(&items, item.parent, &position)?;
                    reply = IngestReply::Rank(item.rank.clone());
                    None
                }
                IngestRequest::Edit { title, body, .. } => {
                    if let Some(title) = title {
                        item.title = title;
                    }
                    if let Some(body) = body {
                        item.body = body;
                    }
                    None
                }
                IngestRequest::BindSession {
                    session_id, review, ..
                } => {
                    if session_id.trim().is_empty() {
                        return Err(invalid("Session ID must not be empty"));
                    }
                    let slot = if review {
                        &mut item.review_session_id
                    } else {
                        &mut item.session_id
                    };
                    if slot.as_deref() == Some(session_id.as_str()) || (!review && slot.is_some()) {
                        return Ok((IngestReply::Item(item), vec![]));
                    }
                    *slot = Some(session_id);
                    reply = IngestReply::Item(item.clone());
                    None
                }
                _ => unreachable!(),
            };
            (
                special.unwrap_or(BoardEvent::ItemStored { id, item }),
                reply,
            )
        }
    };
    append(&mut file, &make_envelope(event))?;
    Ok((reply, vec![seq]))
}
