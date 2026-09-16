//! Explicit task-session requests, using the generic durable session outbox.

use std::path::Path;
use std::sync::{Arc, Mutex};

use horizon_agent::contract::{Command, Event, SessionId, SessionInput, ToolCallRequest};
use horizon_agent::persistence::event_log;
use horizon_agent::roles::RoleId;
use horizon_board::Store;
use serde::Deserialize;
use serde_json::{json, Value};

use super::roles;
use super::ReplyAddress;
use crate::session::{resume_session, spawn_session_thread, AgentdState};

static START_SESSION: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
enum Operation {
    Consult {
        id: u64,
        text: String,
    },
    Implement {
        id: u64,
        base: String,
    },
    Review {
        id: u64,
        base: String,
        tip: String,
        checks: String,
    },
    Send {
        session_id: String,
        text: String,
        reply_to: Option<String>,
    },
}

pub(crate) async fn operate(
    state: Arc<AgentdState>,
    store: &Store,
    root: &Path,
    caller: SessionId,
    request: &ToolCallRequest,
) -> Result<(Value, Vec<Event>), String> {
    let operation: Operation =
        serde_json::from_value(request.input.clone().into()).map_err(|e| e.to_string())?;
    let source = format!(
        "tool:{}:{}",
        caller.as_uuid(),
        request
            .occurrence_id
            .as_ref()
            .map(|id| id.0.as_str())
            .unwrap_or(&request.call_id.0)
    );
    match operation {
        Operation::Consult { id, text } => {
            let target = task_session(&state, store, root, id, false).await?;
            let organizer = organizer_session(&state, root)?;
            let input = SessionInput {
                id: source,
                origin: format!("session:{}", caller.as_uuid()),
                text: format!(
                    "Task #{id}. Board organizer session: {}.\n\n{text}",
                    organizer.as_uuid()
                ),
                reply_to: Some(ReplyAddress::board(root, id)?),
                resume_work: false,
            };
            let events = queue(target, input)?;
            Ok((
                json!({"session_id":target.as_uuid().to_string(),"queued":true}),
                events,
            ))
        }
        Operation::Implement { id, base } => {
            own_task(store, id, caller)?;
            if base.trim().is_empty() {
                return Err("An explicit base commit is required".into());
            }
            if !state.send_command(caller, Command::ActivateWorktree { base }) {
                return Err("Task session stopped before environment activation".into());
            }
            Ok((json!({"requested":true}), Vec::new()))
        }
        Operation::Review {
            id,
            base,
            tip,
            checks,
        } => {
            own_task(store, id, caller)?;
            let (working_root, isolated) = state
                .session_directory(caller)
                .ok_or("Task environment is unavailable")?;
            if !isolated {
                return Err(
                    "Activate the task worktree before requesting implementation review".into(),
                );
            }
            let base = commit(&working_root, &base)?;
            let tip = commit(&working_root, &tip)?;
            let records = records(&state)?;
            let identity = records
                .iter()
                .rev()
                .find_map(|record| match &record.event {
                    Event::EnvironmentActivated(identity) if record.session_id == caller => {
                        Some(identity)
                    }
                    _ => None,
                })
                .ok_or("Task has no recorded starting base")?;
            if identity.base != base {
                return Err("Review base differs from the task's recorded starting commit".into());
            }
            if commit(&working_root, "HEAD")? != tip {
                return Err("Review target must match the task worktree's current tip".into());
            }
            let reply_to = ReplyAddress::review_result(root, id, caller)?;
            let target = reviewer_session(&state, store, root, id, caller, &tip).await?;
            let events = queue(target, SessionInput {
                id: source,
                origin: format!("session:{}", caller.as_uuid()),
                text: format!("Review task #{id} from {base} to {tip}. Checks: {checks}\nYour dedicated worktree is pinned to this exact target tip. Read the task's requirements and consultation with board.read. Inspect the full range and independently verify it without modifying the implementation. Return findings to the requesting task session."),
                reply_to: Some(reply_to),
                resume_work: false,
            })?;
            Ok((
                json!({"session_id":target.as_uuid().to_string(),"queued":true}),
                events,
            ))
        }
        Operation::Send {
            session_id,
            text,
            reply_to,
        } => {
            let target = parse_session(&session_id)?;
            same_project(&state, root, target)?;
            let reply_to = reply_to
                .map(|id| {
                    parse_session(&id).and_then(|id| {
                        same_project(&state, root, id)?;
                        Ok(ReplyAddress::session(id))
                    })
                })
                .transpose()?;
            let events = queue(
                target,
                SessionInput {
                    id: source,
                    origin: format!("session:{}", caller.as_uuid()),
                    text,
                    reply_to,
                    resume_work: false,
                },
            )?;
            Ok((json!({"queued":true}), events))
        }
    }
}

async fn reviewer_session(
    state: &Arc<AgentdState>,
    store: &Store,
    root: &Path,
    task: u64,
    caller: SessionId,
    tip: &str,
) -> Result<SessionId, String> {
    if state.writer().is_none() {
        return Err("Review sessions require event persistence".into());
    }
    let project = crate::worktree::project_root(root).ok_or("Board is not in a Git repository")?;
    let provider = state
        .providers
        .lock()
        .map_err(|error| error.to_string())?
        .default_provider_id();
    let (target, worktree) = review_snapshot(&project, tip)?;
    spawn_session_thread(
        state.clone(),
        target,
        provider,
        Some(RoleId(roles::REVIEWER.into())),
        Some(worktree.path.clone()),
        Some(caller),
        false,
        Some(worktree),
        Vec::new(),
    );
    if let Err(error) = store
        .bind_review_session(task, &target.as_uuid().to_string())
        .await
    {
        state.send_command(target, Command::Shutdown);
        return Err(error.to_string());
    }
    Ok(target)
}

fn review_snapshot(
    project: &Path,
    tip: &str,
) -> Result<(SessionId, crate::worktree::WorktreeInfo), String> {
    let target = SessionId::new();
    let worktree = crate::worktree::create_isolated_worktree_at(project, target.as_uuid(), tip)?;
    Ok((target, worktree))
}

fn own_task(store: &Store, id: u64, caller: SessionId) -> Result<(), String> {
    let item = store
        .show(id)
        .map_err(|e| e.to_string())?
        .ok_or("Task does not exist")?;
    if item.session_id.as_deref() != Some(caller.as_uuid().to_string().as_str()) {
        return Err("This session is not the task's associated session".into());
    }
    Ok(())
}
fn queue(target: SessionId, input: SessionInput) -> Result<Vec<Event>, String> {
    if input.text.trim().is_empty() {
        return Err("A message must contain text".into());
    }
    Ok(vec![Event::SessionInputSent {
        session_id: target,
        input,
    }])
}

pub(super) async fn task_session(
    state: &Arc<AgentdState>,
    store: &Store,
    root: &Path,
    id: u64,
    resume: bool,
) -> Result<SessionId, String> {
    let candidate = SessionId::new().as_uuid().to_string();
    let item = store
        .bind_session(id, &candidate)
        .await
        .map_err(|e| e.to_string())?;
    let bound = item
        .session_id
        .ok_or("Task session binding was not stored")?;
    let target = parse_session(&bound)?;
    ensure_started(state, target, root, roles::TASK, resume)?;
    Ok(target)
}

pub(crate) fn organizer_session(
    state: &Arc<AgentdState>,
    root: &Path,
) -> Result<SessionId, String> {
    let _guard = START_SESSION.lock().map_err(|e| e.to_string())?;
    let project = crate::worktree::project_root(root).ok_or("Board is not in a Git repository")?;
    let live = crate::session::Connection::new(state.clone())
        .session_list()
        .into_iter()
        .find(|session| {
            session
                .role_id
                .as_ref()
                .is_some_and(|role| role.0 == roles::ORGANIZER)
                && session
                    .workspace_root
                    .as_deref()
                    .and_then(crate::worktree::project_root)
                    .as_ref()
                    == Some(&project)
        });
    if let Some(session) = live {
        return Ok(session.session_id);
    }
    let records = records(state)?;
    let existing = records
        .iter()
        .rev()
        .find(|record| {
            record
                .role_id
                .as_ref()
                .is_some_and(|r| r.0 == roles::ORGANIZER)
                && record
                    .session_context
                    .as_ref()
                    .and_then(|c| c.workspace_root.as_deref())
                    .and_then(crate::worktree::project_root)
                    .as_ref()
                    == Some(&project)
        })
        .map(|record| record.session_id);
    let id = existing.unwrap_or_else(SessionId::new);
    start_locked(state, id, &project, roles::ORGANIZER, true, &records)?;
    Ok(id)
}
fn ensure_started(
    state: &Arc<AgentdState>,
    id: SessionId,
    root: &Path,
    role: &str,
    resume: bool,
) -> Result<(), String> {
    if state.session_exists(id) {
        return Ok(());
    }
    let _guard = START_SESSION.lock().map_err(|e| e.to_string())?;
    let records = records(state)?;
    start_locked(state, id, root, role, resume, &records)
}
fn start_locked(
    state: &Arc<AgentdState>,
    id: SessionId,
    root: &Path,
    role: &str,
    resume: bool,
    records: &[event_log::Record],
) -> Result<(), String> {
    if state.session_exists(id) {
        return Ok(());
    }
    if records.iter().any(|record| record.session_id == id) {
        if !resume {
            return Err("The task session has been explicitly ended".into());
        }
        return resume_session(state, id);
    }
    if state.writer().is_none() {
        return Err("Task sessions require event persistence".into());
    }
    let provider = state
        .providers
        .lock()
        .map_err(|e| e.to_string())?
        .default_provider_id();
    spawn_session_thread(
        state.clone(),
        id,
        provider,
        Some(RoleId(role.into())),
        Some(root.to_path_buf()),
        None,
        false,
        None,
        Vec::new(),
    );
    Ok(())
}
pub(super) fn records(state: &Arc<AgentdState>) -> Result<Vec<event_log::Record>, String> {
    let writer = state.writer().ok_or("Session persistence is unavailable")?;
    writer.flush().map_err(|e| e.to_string())?;
    let path = state
        .agent_config
        .lock()
        .map_err(|e| e.to_string())?
        .persistence
        .event_log_path
        .clone();
    event_log::read(path)
        .map(|report| report.records)
        .map_err(|e| e.to_string())
}
pub(super) fn same_project(
    state: &Arc<AgentdState>,
    root: &Path,
    target: SessionId,
) -> Result<(), String> {
    let directory = state
        .session_directory(target)
        .map(|(root, _)| root)
        .or_else(|| {
            records(state)
                .ok()?
                .into_iter()
                .rev()
                .find(|record| record.session_id == target)
                .and_then(|r| r.session_context)
                .and_then(|c| c.workspace_root)
        });
    let actual = directory.as_deref().and_then(crate::worktree::project_root);
    let expected = crate::worktree::project_root(root);
    if expected.is_none() || actual != expected {
        return Err("The recipient must belong to this board's project".into());
    }
    Ok(())
}
pub(super) fn parse_session(id: &str) -> Result<SessionId, String> {
    uuid::Uuid::parse_str(id)
        .map(SessionId::from_uuid)
        .map_err(|e| format!("Invalid session identity: {e}"))
}
fn commit(root: &Path, reference: &str) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .arg("-C")
        .arg(root)
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn git(root: &Path, arguments: &[&str]) {
        let mut command = std::process::Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=Review Test",
                "-c",
                "user.email=review@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn organizer_open_reuses_and_resumes_without_submitting_work() {
        use horizon_agent::contract::{Command, Event, SessionState};
        use horizon_agent::persistence::event_log::{WriterHandle, WriterInit};
        roles::register();
        let repository = tempfile::tempdir().unwrap();
        git(repository.path(), &["init", "-q", "-b", "main"]);
        let state = crate::session::test_support::state_with_rig_config(false, "mock");
        let path = repository.path().join("agent-events.jsonl");
        state
            .agent_config
            .lock()
            .unwrap()
            .persistence
            .event_log_path = path.clone();
        let (writer, ready) = WriterHandle::open(&path);
        assert!(matches!(ready.recv().unwrap(), WriterInit::Ready(_)));
        state.set_writer(Some(writer));
        let connection = crate::session::Connection::new(state.clone());
        let first = connection
            .ensure_board_organizer(repository.path().into())
            .unwrap();
        assert_eq!(
            connection
                .ensure_board_organizer(repository.path().into())
                .unwrap(),
            first
        );
        let stop = || {
            state.send_command(first, Command::Shutdown);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while state.session_exists(first) {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        stop();
        assert_eq!(
            connection
                .ensure_board_organizer(repository.path().into())
                .unwrap(),
            first
        );
        stop();
        let history = records(&state).unwrap();
        assert!(history.iter().all(|record| record.session_id == first));
        assert!(history
            .iter()
            .any(|record| matches!(record.event, Event::SessionResumed)));
        assert!(history
            .iter()
            .any(|record| matches!(record.event, Event::StateChanged(SessionState::Terminated))));
        assert!(!history.iter().any(|record| matches!(
            record.event,
            Event::InputAccepted(_)
                | Event::InputStarted(_)
                | Event::MessageCommitted(horizon_agent::contract::Message {
                    role: horizon_agent::contract::MessageRole::User,
                    ..
                })
        )));
    }

    #[test]
    fn review_requests_get_distinct_worktrees_pinned_to_the_requested_tip() {
        let repository = tempfile::tempdir().unwrap();
        git(repository.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repository.path().join("implementation"), "review target").unwrap();
        git(repository.path(), &["add", "implementation"]);
        git(repository.path(), &["commit", "-qm", "target"]);
        let tip = commit(repository.path(), "HEAD").unwrap();
        let (first, first_tree) = review_snapshot(repository.path(), &tip).unwrap();
        std::fs::write(
            repository.path().join("implementation"),
            "later implementation",
        )
        .unwrap();
        git(repository.path(), &["commit", "-qam", "later"]);
        let (second, second_tree) = review_snapshot(repository.path(), &tip).unwrap();
        assert_ne!(first, second);
        assert_ne!(first_tree.path, second_tree.path);
        for tree in [&first_tree, &second_tree] {
            assert_eq!(commit(&tree.path, "HEAD").unwrap(), tip);
            assert_eq!(
                std::fs::read_to_string(tree.path.join("implementation")).unwrap(),
                "review target"
            );
            assert!(crate::worktree::remove_worktree_if_clean(tree));
        }
        assert_eq!(
            std::fs::read_to_string(repository.path().join("implementation")).unwrap(),
            "later implementation"
        );
    }
}
