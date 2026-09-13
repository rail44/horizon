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

/// Computes the rank for a new/moved item at `position`, given the current
/// folded items.
fn compute_rank(items: &HashMap<u64, Item>, position: &Position) -> Result<String, LogError> {
    let sorted = sorted_by_rank(items);
    match position {
        Position::Top => {
            let hi = sorted.first().map(|i| i.rank.as_str());
            rank_between(None, hi).ok_or(LogError::RankExhausted)
        }
        Position::Bottom => {
            let lo = sorted.last().map(|i| i.rank.as_str());
            rank_between(lo, None).ok_or(LogError::RankExhausted)
        }
        Position::After(id) => {
            let item = items.get(id).ok_or(LogError::ItemNotFound(*id))?;
            let idx = sorted
                .iter()
                .position(|i| i.id == *id)
                .expect("item in map but not in sorted list");
            let lo = Some(item.rank.as_str());
            let hi = sorted.get(idx + 1).map(|i| i.rank.as_str());
            rank_between(lo, hi).ok_or(LogError::RankExhausted)
        }
        Position::Before(id) => {
            let item = items.get(id).ok_or(LogError::ItemNotFound(*id))?;
            let idx = sorted
                .iter()
                .position(|i| i.id == *id)
                .expect("item in map but not in sorted list");
            let lo = if idx > 0 {
                Some(sorted[idx - 1].rank.as_str())
            } else {
                None
            };
            let hi = Some(item.rank.as_str());
            rank_between(lo, hi).ok_or(LogError::RankExhausted)
        }
    }
}

/// Dispatches one `IngestRequest` to the matching write operation against
/// `path`. Each operation opens the file with an exclusive lock, reads-folds,
/// computes, appends, and flushes — the same atomic sequence the library's
/// `Store` used to do in-process.
///
/// Returns the reply plus the 1-based line numbers (seqs) of every line
/// appended, in order. The caller (the hub) fans these out as subscribe pokes
/// so live subscribers learn that new events landed. The seq is the line index
/// in the JSONL file — the durable cursor a consumer catches up from.
pub fn perform(path: &Path, request: IngestRequest) -> Result<(IngestReply, Vec<u64>), LogError> {
    match request {
        IngestRequest::Workflow {
            id,
            expected_revision,
            mutation,
        } => {
            let (mut file, report) = open_locked(path)?;
            let mut items = fold(&report.envelopes);
            let item = items.get_mut(&id).ok_or(LogError::ItemNotFound(id))?;
            if matches!(
                mutation,
                horizon_board::workflow::Mutation::Enable
                    | horizon_board::workflow::Mutation::Start { .. }
            ) && horizon_board::is_closed_status(&item.status)
            {
                return Err(LogError::InvalidWorkflow(
                    "Reopen the item before enabling milestone execution".into(),
                ));
            }
            let revision = item.workflow.as_ref().map_or(0, |flow| flow.revision);
            if revision != expected_revision {
                return Err(LogError::InvalidWorkflow(
                    "The milestone changed; reload and try again".into(),
                ));
            }
            let workflow = horizon_board::workflow::apply(item.workflow.as_deref(), mutation)
                .map_err(LogError::InvalidWorkflow)?;
            let env = make_envelope(BoardEvent::WorkflowChanged {
                id,
                workflow: Box::new(workflow.clone()),
                title: None,
                body: None,
            });
            append(&mut file, &env)?;
            if !horizon_board::is_closed_status(&item.status) {
                item.status = workflow.item_status().into();
            }
            item.workflow = Some(Box::new(workflow));
            Ok((IngestReply::Item(item.clone()), vec![report.line_count + 1]))
        }
        IngestRequest::Add {
            title,
            body,
            parent,
            position,
        } => {
            let (mut file, report) = open_locked(path)?;
            let mut seq = report.line_count;
            let items = fold(&report.envelopes);
            let id = report.max_id.map_or(1, |m| m + 1);
            let rank = compute_rank(&items, &position)?;

            let env = make_envelope(BoardEvent::ItemCreated {
                id,
                title: title.clone(),
                body: body.clone(),
                rank: rank.clone(),
            });
            seq += 1;
            append(&mut file, &env)?;
            let mut seqs = vec![seq];

            if let Some(pid) = parent {
                let upd = make_envelope(BoardEvent::ItemUpdated {
                    id,
                    status: None,
                    rank: None,
                    assignee: None,
                    parent: Some(Some(pid)),
                    depends_on: None,
                    links: None,
                    title: None,
                    body: None,
                });
                seq += 1;
                append(&mut file, &upd)?;
                seqs.push(seq);
            }

            Ok((
                IngestReply::Item(Item {
                    id,
                    title,
                    body,
                    rank,
                    parent,
                    ..Item::default()
                }),
                seqs,
            ))
        }
        IngestRequest::Comment { id, author, text } => {
            let (mut file, report) = open_locked(path)?;
            let items = fold(&report.envelopes);
            if !items.contains_key(&id) {
                return Err(LogError::ItemNotFound(id));
            }
            let env = make_envelope(BoardEvent::CommentAdded { id, author, text });
            let seq = report.line_count + 1;
            append(&mut file, &env)?;
            Ok((IngestReply::Done, vec![seq]))
        }
        IngestRequest::SetStatus { id, status } => {
            let (mut file, report) = open_locked(path)?;
            let items = fold(&report.envelopes);
            if !items.contains_key(&id) {
                return Err(LogError::ItemNotFound(id));
            }
            let env = make_envelope(BoardEvent::ItemUpdated {
                id,
                status: Some(status),
                rank: None,
                assignee: None,
                parent: None,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            });
            let seq = report.line_count + 1;
            append(&mut file, &env)?;
            Ok((IngestReply::Done, vec![seq]))
        }
        IngestRequest::Assign { id, who } => {
            let (mut file, report) = open_locked(path)?;
            let items = fold(&report.envelopes);
            if !items.contains_key(&id) {
                return Err(LogError::ItemNotFound(id));
            }
            let env = make_envelope(BoardEvent::ItemUpdated {
                id,
                status: None,
                rank: None,
                assignee: Some(who),
                parent: None,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            });
            let seq = report.line_count + 1;
            append(&mut file, &env)?;
            Ok((IngestReply::Done, vec![seq]))
        }
        IngestRequest::MoveItem { id, position } => {
            let (mut file, report) = open_locked(path)?;
            let items = fold(&report.envelopes);
            if !items.contains_key(&id) {
                return Err(LogError::ItemNotFound(id));
            }
            let rank = compute_rank(&items, &position)?;
            let env = make_envelope(BoardEvent::ItemUpdated {
                id,
                status: None,
                rank: Some(rank.clone()),
                assignee: None,
                parent: None,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            });
            let seq = report.line_count + 1;
            append(&mut file, &env)?;
            Ok((IngestReply::Rank(rank), vec![seq]))
        }
        IngestRequest::Edit { id, title, body } => {
            let (mut file, report) = open_locked(path)?;
            let items = fold(&report.envelopes);
            if !items.contains_key(&id) {
                return Err(LogError::ItemNotFound(id));
            }
            if let Some(mut workflow) = items[&id].workflow.clone() {
                if workflow.active.is_some() {
                    return Err(LogError::InvalidWorkflow("Pause the milestone and wait for its session to stop before changing the goal".into()));
                }
                workflow.revision += 1;
                workflow.plan_requested = true;
                workflow.problem = None;
                let env = make_envelope(BoardEvent::WorkflowChanged {
                    id,
                    workflow,
                    title,
                    body,
                });
                append(&mut file, &env)?;
                return Ok((IngestReply::Done, vec![report.line_count + 1]));
            }
            let env = make_envelope(BoardEvent::ItemUpdated {
                id,
                status: None,
                rank: None,
                assignee: None,
                parent: None,
                depends_on: None,
                links: None,
                title,
                body,
            });
            let seq = report.line_count + 1;
            append(&mut file, &env)?;
            Ok((IngestReply::Done, vec![seq]))
        }
        IngestRequest::Claim { who } => {
            let (mut file, report) = open_locked(path)?;
            let items = fold(&report.envelopes);
            let sorted = sorted_by_rank(&items);

            let found = sorted
                .into_iter()
                .find(|i| i.status == "ready" && i.assignee.is_empty() && i.workflow.is_none());

            let Some(mut item) = found.cloned() else {
                return Ok((IngestReply::MaybeItem(None), vec![]));
            };

            let env = make_envelope(BoardEvent::ItemUpdated {
                id: item.id,
                status: Some("in-progress".to_string()),
                rank: None,
                assignee: Some(who.clone()),
                parent: None,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            });
            let seq = report.line_count + 1;
            append(&mut file, &env)?;

            item.status = "in-progress".to_string();
            item.assignee = who;
            Ok((IngestReply::MaybeItem(Some(item)), vec![seq]))
        }
    }
}
