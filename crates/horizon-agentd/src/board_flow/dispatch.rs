//! Reconcile durable board triggers and generic session delivery receipts.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::log::{Index, Pending, Tail};
use super::{operations, ReplyAddress};
use crate::session::AgentdState;
use horizon_agent::contract::{Command, InputResult, SessionId, SessionInput};
use horizon_board::{BoardEvent, Comment, Item, Store};

const CONSUMER: &str = "task-session-delivery";
struct Project {
    root: PathBuf,
    store: Store,
    cursor: u64,
    length: u64,
    pending: bool,
}
impl Project {
    fn finish_pass(
        &mut self,
        cursor: u64,
        waiting: bool,
        checkpoint: Result<(), String>,
    ) -> Result<(), String> {
        // Keep the old cursor on failure: replay checks durable input receipts
        // and message identities, so retrying cannot duplicate delivery.
        checkpoint?;
        self.cursor = cursor;
        self.pending = waiting;
        Ok(())
    }
}
pub(super) struct Runner {
    state: Arc<AgentdState>,
    tail: Tail,
    index: Index,
    projects: HashMap<PathBuf, Project>,
    acknowledging: HashSet<(SessionId, String)>,
    registered: HashSet<PathBuf>,
}
impl Runner {
    pub(super) fn new(state: Arc<AgentdState>, path: PathBuf) -> Self {
        Self {
            state,
            tail: Tail::new(path),
            index: Index::default(),
            projects: HashMap::new(),
            acknowledging: HashSet::new(),
            registered: HashSet::new(),
        }
    }
    pub(super) async fn tick(&mut self) -> Result<(), String> {
        for record in self.tail.read()? {
            if let Some(root) = record
                .role_id
                .as_ref()
                .filter(|role| {
                    matches!(
                        role.0.as_str(),
                        super::roles::ORGANIZER | super::roles::TASK | super::roles::REVIEWER
                    )
                })
                .and(record.session_context.as_ref())
                .and_then(|context| context.workspace_root.clone())
            {
                self.state.register_board(root);
            }
            self.index.fold(record);
        }
        for candidate in self.state.board_projects() {
            if self.registered.contains(&candidate) {
                continue;
            }
            let root = candidate.clone();
            let Some(root) = crate::worktree::project_root(&root) else {
                continue;
            };
            let Ok(store) = Store::from_dir(&root) else {
                continue;
            };
            if !self.projects.contains_key(store.path()) {
                let cursor = store.cursor(CONSUMER).map_err(|e| e.to_string())?;
                self.projects.insert(
                    store.path().into(),
                    Project {
                        root,
                        store,
                        cursor,
                        length: 0,
                        pending: true,
                    },
                );
            }
            self.registered.insert(candidate);
        }
        let mut error = self.deliver_outputs().await.err();
        for project in self.projects.values_mut() {
            if let Err(e) = process_project(&self.state, &self.index, project).await {
                error = Some(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
    async fn deliver_outputs(&mut self) -> Result<(), String> {
        self.acknowledging
            .retain(|key| self.index.pending.contains_key(key));
        let pending = self.index.ordered_pending();
        let mut error = None;
        let mut waiting_recipients = HashSet::new();
        for ((source, id), pending) in pending {
            let recipient = recipient_key(&pending);
            if recipient
                .as_ref()
                .is_some_and(|key| waiting_recipients.contains(key))
            {
                continue;
            }
            if self.acknowledging.contains(&(source, id.clone()))
                && self.state.session_exists(source)
            {
                continue;
            }
            let done = match self.deliver_one(source, &id, pending).await {
                Ok(done) => done,
                Err(message) => {
                    if let Some(key) = recipient {
                        waiting_recipients.insert(key);
                    }
                    error = Some(message);
                    continue;
                }
            };
            if !done {
                if let Some(key) = recipient {
                    waiting_recipients.insert(key);
                }
            }
            if done {
                match self.state.acknowledge_delivery(source, id.clone()) {
                    Ok(()) => {
                        self.acknowledging.insert((source, id));
                    }
                    Err(message) => error = Some(message),
                }
            }
        }
        error.map_or(Ok(()), Err)
    }
    async fn deliver_one(
        &self,
        source: SessionId,
        id: &str,
        pending: Pending,
    ) -> Result<bool, String> {
        Ok(match pending {
            Pending::Send { target, input, .. } => self.deliver_input(target, input),
            Pending::Answer { outcome, at, .. } => match outcome.reply_to.as_deref() {
                None => true,
                Some(address) => {
                    let address: ReplyAddress = serde_json::from_str(address)
                        .map_err(|e| format!("Invalid stored reply destination: {e}"))?;
                    let (author, text) = match outcome.outcome {
                        InputResult::Success { text } => {
                            (format!("session:{}", source.as_uuid()), text)
                        }
                        InputResult::Failure { message } => ("system".into(), message),
                        InputResult::Interrupted => {
                            ("system".into(), "Session interrupted.".into())
                        }
                    };
                    match address {
                        ReplyAddress::Board { root, task } => {
                            self.source_project(source, &root)?;
                            let store = Store::from_dir(&root).map_err(|e| e.to_string())?;
                            store
                                .post_message(
                                    task,
                                    Comment {
                                        id: format!("session:{}:{id}", source.as_uuid()),
                                        author,
                                        text,
                                        at: Some(at),
                                        source: Some(format!("session:{}:{id}", source.as_uuid())),
                                    },
                                )
                                .await
                                .map_err(|e| e.to_string())?;
                            true
                        }
                        ReplyAddress::Session { id: target } => {
                            let target = operations::parse_session(&target)?;
                            self.deliver_input(
                                target,
                                SessionInput {
                                    id: format!("session:{}:{id}", source.as_uuid()),
                                    origin: author,
                                    text,
                                    reply_to: None,
                                    resume_work: false,
                                },
                            )
                        }
                        ReplyAddress::ReviewResult {
                            id: target,
                            root,
                            task,
                        } => {
                            let target = operations::parse_session(&target)?;
                            self.source_project(source, &root)?;
                            self.source_project(target, &root)?;
                            self.deliver_input(
                                target,
                                SessionInput {
                                    id: format!("session:{}:{id}", source.as_uuid()),
                                    origin: author,
                                    text,
                                    reply_to: Some(ReplyAddress::board(&root, task)?),
                                    resume_work: false,
                                },
                            )
                        }
                    }
                }
            },
        })
    }
    fn source_project(&self, source: SessionId, expected: &std::path::Path) -> Result<(), String> {
        let recorded = self.index.projects.get(&source).cloned().or_else(|| {
            self.index
                .sessions
                .get(&source)
                .and_then(|record| record.session_context.as_ref())
                .and_then(|context| context.workspace_root.as_deref())
                .and_then(crate::worktree::project_root)
        });
        if recorded.as_deref() != Some(expected) {
            return Err("Reply destination differs from the source session's project".into());
        }
        Ok(())
    }
    fn deliver_input(&self, target: SessionId, input: SessionInput) -> bool {
        if self.index.accepted.contains(&(target, input.id.clone())) {
            return true;
        }
        self.state
            .send_command(target, Command::SessionInput(input));
        false
    }
}

/// Wait for the first durable receipt before sending a later request to the
/// same recipient. An offline recipient never stalls unrelated destinations.
fn recipient_key(pending: &Pending) -> Option<String> {
    match pending {
        Pending::Send { target, .. } => Some(format!("session:{}", target.as_uuid())),
        Pending::Answer { outcome, .. } => {
            match serde_json::from_str::<ReplyAddress>(outcome.reply_to.as_deref()?).ok()? {
                ReplyAddress::Session { id } | ReplyAddress::ReviewResult { id, .. } => {
                    Some(format!("session:{id}"))
                }
                ReplyAddress::Board { root, task } => {
                    Some(format!("board:{}:{task}", root.display()))
                }
            }
        }
    }
}

async fn process_project(
    state: &Arc<AgentdState>,
    index: &Index,
    project: &mut Project,
) -> Result<(), String> {
    let length = std::fs::metadata(project.store.path())
        .map(|m| m.len())
        .unwrap_or(0);
    if length == project.length && !project.pending {
        return Ok(());
    }
    project.length = length;
    // Every fallible exit remains runnable even if the file does not change.
    // Only a successful scan and checkpoint may mark this project clean.
    project.pending = true;
    let mut cursor = project.cursor;
    let mut waiting = false;
    let report = project.store.events().map_err(|e| e.to_string())?;
    if report.corrupt_count > 0 || report.skipped_count > 0 || report.torn_trailing {
        return Err(format!(
            "Board {} requires valid migrated data before task delivery",
            project.root.display()
        ));
    }
    let mut known: HashMap<u64, Item> = HashMap::new();
    let mut persist = None;
    for (sequence, envelope) in report.sequences.iter().zip(&report.envelopes) {
        let previous = match &envelope.event {
            BoardEvent::ItemStored { id, item } | BoardEvent::ImportedItem { id, item } => {
                known.insert(*id, item.clone())
            }
            _ => None,
        };
        if *sequence <= cursor {
            continue;
        }
        let business = matches!(
            &envelope.event,
            BoardEvent::ItemStored { .. } | BoardEvent::MessageAdded { .. }
        );
        let delivered = match handle_event(
            state,
            index,
            project,
            *sequence,
            &envelope.event,
            previous.as_ref(),
        )
        .await
        {
            Ok(done) => done,
            Err(error) => {
                if let Some(task) = task_id(&envelope.event) {
                    let key = format!("board-error:{}:{sequence}", project.store.path().display());
                    project
                        .store
                        .post_message(
                            task,
                            Comment {
                                id: key.clone(),
                                author: "system".into(),
                                text: error,
                                at: Some(envelope.at),
                                source: Some(key),
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    true
                } else {
                    return Err(error);
                }
            }
        };
        if !delivered {
            waiting = true;
            break;
        }
        cursor = *sequence;
        if business {
            persist = Some(*sequence);
        }
    }
    let checkpoint = match persist {
        Some(position) => project
            .store
            .advance_cursor(CONSUMER, position)
            .await
            .map_err(|e| e.to_string()),
        None => Ok(()),
    };
    project.finish_pass(cursor, waiting, checkpoint)
}

async fn handle_event(
    state: &Arc<AgentdState>,
    index: &Index,
    project: &Project,
    sequence: u64,
    event: &BoardEvent,
    previous: Option<&Item>,
) -> Result<bool, String> {
    let event_id = format!("board:{}:{sequence}", project.store.path().display());
    match event {
        BoardEvent::ItemStored { id, .. } if previous.is_none() => {
            let organizer = operations::organizer_session(state, &project.root)?;
            Ok(receipt(state,index,organizer,SessionInput{id:event_id,origin:"board".into(),
                text:format!("Task #{id} was registered. Organize its priority and dependencies, then select useful task consultations."),reply_to:None,resume_work:false}))
        }
        BoardEvent::MessageAdded { id, message } if message.author == "owner" => {
            let task =
                operations::task_session(state, &project.store, &project.root, *id, true).await?;
            let organizer = operations::organizer_session(state, &project.root)?;
            Ok(receipt(
                state,
                index,
                task,
                SessionInput {
                    id: format!(
                        "board-message:{}:{}",
                        project.store.path().display(),
                        message.id
                    ),
                    origin: "owner".into(),
                    text: format!(
                        "Task #{id}. Board organizer session: {}.\n\n{}",
                        organizer.as_uuid(),
                        message.text
                    ),
                    reply_to: Some(ReplyAddress::board(&project.root, *id)?),
                    resume_work: true,
                },
            ))
        }
        BoardEvent::ItemStored { id, item }
            if item.is_closed && previous.is_some_and(|item| !item.is_closed) =>
        {
            let mut done = true;
            let all = project
                .store
                .list(None, true)
                .map_err(|e| e.to_string())?
                .items;
            for dependent in all
                .iter()
                .filter(|item| !item.is_closed && item.depends_on.contains(id))
            {
                if let Some(session) = dependent.session_id.as_deref() {
                    let target = operations::parse_session(session)?;
                    // Explicitly ended sessions stay ended. A later owner post resumes
                    // them and the task skill rereads the current prerequisites.
                    if state.session_exists(target) {
                        done &= receipt(state,index,target,SessionInput{id:format!("{event_id}:dependent:{}",dependent.id),origin:"board".into(),
                            text:format!("Prerequisite #{id} was closed. It may have been completed or withdrawn. Reread task #{} and all prerequisites before deciding what to do.",dependent.id),reply_to:None,resume_work:false});
                    }
                }
            }
            let organizer = operations::organizer_session(state, &project.root)?;
            done &= receipt(state,index,organizer,SessionInput{id:format!("{event_id}:organizer"),origin:"board".into(),
                text:format!("Task #{id} was closed. It may have been completed or withdrawn. Reassess the current priorities, prerequisites, and useful parallel work."),reply_to:None,resume_work:false});
            Ok(done)
        }
        _ => Ok(true),
    }
}
pub(super) fn receipt(
    state: &Arc<AgentdState>,
    index: &Index,
    target: SessionId,
    input: SessionInput,
) -> bool {
    if index.accepted.contains(&(target, input.id.clone())) {
        return true;
    }
    state.send_command(target, Command::SessionInput(input));
    false
}
fn task_id(event: &BoardEvent) -> Option<u64> {
    match event {
        BoardEvent::ItemStored { id, .. } | BoardEvent::MessageAdded { id, .. } => Some(*id),
        _ => None,
    }
}

#[cfg(test)]
mod review_tests;

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[tokio::test]
    async fn failed_read_retries_without_another_board_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.jsonl");
        let valid = format!(
            "{}\n",
            serde_json::to_string(&horizon_board::Envelope {
                schema: horizon_board::SCHEMA.into(),
                version: horizon_board::VERSION,
                at: 0,
                event: BoardEvent::ImportHighWater { id: 42 }
            })
            .unwrap()
        );
        std::fs::write(&path, "x".repeat(valid.len() - 1) + "\n").unwrap();
        let mut project = Project {
            root: dir.path().into(),
            store: Store::at(path.clone()),
            cursor: 0,
            length: 0,
            pending: true,
        };
        let state = crate::session::test_support::state_with_rig_config(false, "test");
        let index = Index::default();
        assert!(process_project(&state, &index, &mut project).await.is_err());
        assert!(project.pending);
        assert_eq!(project.cursor, 0);
        // Same byte length: recovery alone must trigger another read.
        std::fs::write(&path, valid).unwrap();
        process_project(&state, &index, &mut project).await.unwrap();
        assert_eq!(project.cursor, 1);
        assert!(!project.pending);
        assert!(
            state.board_projects().is_empty(),
            "Import must not start task work"
        );
    }

    #[test]
    fn failed_checkpoint_keeps_previous_cursor_and_retry_eligibility() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = Project {
            root: dir.path().into(),
            store: Store::at(dir.path().join("board.jsonl")),
            cursor: 7,
            length: 100,
            pending: true,
        };
        assert!(project
            .finish_pass(12, false, Err("temporary logd failure".into()))
            .is_err());
        assert_eq!(project.cursor, 7);
        assert!(project.pending);
        project.finish_pass(12, false, Ok(())).unwrap();
        assert_eq!(project.cursor, 12);
        assert!(!project.pending);
    }

    #[tokio::test]
    async fn pending_receipt_orders_one_recipient_without_blocking_another() {
        let state = crate::session::test_support::state_with_rig_config(false, "test");
        let source = SessionId::new();
        let target = SessionId::new();
        let other = SessionId::new();
        let receiver = state.install_test_session(target);
        let other_receiver = state.install_test_session(other);
        let dir = tempfile::tempdir().unwrap();
        let mut runner = Runner::new(state, dir.path().join("agent.jsonl"));
        for (id, sequence, recipient) in [
            ("request", 1, target),
            ("correction", 2, target),
            ("unrelated", 3, other),
        ] {
            runner.index.pending.insert(
                (source, id.into()),
                Pending::Send {
                    sequence,
                    target: recipient,
                    input: SessionInput {
                        id: id.into(),
                        origin: "source".into(),
                        text: id.into(),
                        reply_to: None,
                        resume_work: false,
                    },
                },
            );
        }
        runner.deliver_outputs().await.unwrap();
        assert!(
            matches!(receiver.try_recv().unwrap(),Command::SessionInput(input) if input.id=="request")
        );
        assert!(
            receiver.try_recv().is_err(),
            "Correction waits for request receipt"
        );
        assert!(
            matches!(other_receiver.try_recv().unwrap(),Command::SessionInput(input) if input.id=="unrelated")
        );
        runner.index.accepted.insert((target, "request".into()));
        // A missing source writer prevents acknowledgement in this isolated
        // test, but an accepted first request must still release its successor.
        assert!(runner.deliver_outputs().await.is_err());
        assert!(
            matches!(receiver.try_recv().unwrap(),Command::SessionInput(input) if input.id=="correction")
        );
    }
}
