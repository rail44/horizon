//! Session-owned background bash jobs: a receipt, a durable log, and a
//! completion notification. The launch call does not wait for the process.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::contract::SessionId;
use crate::tools::input::BashJob;
use crate::tools::output::{BashOutput, Response};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobState {
    Running,
    Finished,
    Failed,
    Cancelled,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

pub(crate) struct Entry {
    owner: SessionId,
    id: Uuid,
    call_id: crate::contract::ToolCallId,
    occurrence_id: crate::contract::OccurrenceId,
    command: String,
    cwd: PathBuf,
    log_path: PathBuf,
    started_at: Instant,
    started_at_unix_ms: i64,
    state: JobState,
    cancel: Option<Box<dyn FnOnce() + Send>>,
    result: Option<BashOutput>,
}

impl Entry {
    fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "job_id": self.id.to_string(),
            "state": self.state.as_str(),
            "command": self.command,
            "cwd": self.cwd.display().to_string(),
            "log_path": self.log_path.display().to_string(),
            "started_at_unix_ms": self.started_at_unix_ms,
            "elapsed_ms": self.started_at.elapsed().as_millis() as u64,
            "exit_code": self.result.as_ref().and_then(BashOutput::exit_code),
            "summary": self.result.as_ref().map(BashOutput::summary),
            "output": self.result.as_ref().map(|result| result.output.clone()),
            "truncated": self.result.as_ref().map(|result| result.truncated),
            "output_file": self.result.as_ref().and_then(|result| result.output_file.clone()),
        })
    }
}

#[derive(Default)]
struct Registry {
    entries: HashMap<Uuid, Arc<Mutex<Entry>>>,
    pending: HashMap<SessionId, Vec<String>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn log_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(horizon_sandbox::SCRATCH_DIR_NAME)
        .join(format!("bash-job-{}.log", Uuid::new_v4()))
}

/// Register before launching the process, so an immediate cancel cannot lose
/// its identity. The returned id is the only handle exposed to the model.
pub(crate) fn register(
    owner: SessionId,
    call_id: &crate::contract::ToolCallId,
    occurrence_id: &crate::contract::OccurrenceId,
    command: String,
    cwd: PathBuf,
    log_path: PathBuf,
) -> (Uuid, Arc<Mutex<Entry>>) {
    let call_id = call_id.clone();
    let occurrence_id = occurrence_id.clone();
    let id = Uuid::new_v4();
    let entry = Arc::new(Mutex::new(Entry {
        owner,
        id,
        call_id,
        occurrence_id,
        command,
        cwd,
        log_path,
        started_at: Instant::now(),
        started_at_unix_ms: unix_epoch_ms(),
        state: JobState::Running,
        cancel: None,
        result: None,
    }));
    lock(registry()).entries.insert(id, entry.clone());
    (id, entry)
}

/// Install the actual process kill. A cancel that already won runs it now.
/// Called by the bash worker once its child exists, so `bash.cancel` reaches
/// the same process-tree kill a timeout and a turn cancel use.
pub(crate) fn attach_cancel(
    call_id: &crate::contract::ToolCallId,
    occurrence_id: &crate::contract::OccurrenceId,
    cancel: impl FnOnce() + Send + 'static,
) {
    let Some(id) = job_id_for(call_id, occurrence_id) else {
        cancel();
        return;
    };
    let Some(entry) = lock(registry()).entries.get(&id).cloned() else {
        cancel();
        return;
    };
    let cancel = {
        let mut guard = lock(&entry);
        if guard.state == JobState::Running && guard.cancel.is_none() {
            guard.cancel = Some(Box::new(cancel));
            None
        } else {
            Some(Box::new(cancel))
        }
    };
    if let Some(cancel) = cancel {
        cancel();
    }
}

fn owned_entry(owner: SessionId, id: Uuid) -> Option<Arc<Mutex<Entry>>> {
    let entry = lock(registry()).entries.get(&id).cloned()?;
    let owned = lock(&entry).owner == owner;
    owned.then_some(entry)
}

pub(crate) fn status(owner: SessionId, id: Uuid) -> Option<serde_json::Value> {
    owned_entry(owner, id).map(|entry| lock(&entry).status())
}

/// The launch receipt is deliberately small; output is fetched separately.
pub(crate) fn receipt(id: Uuid, entry: &Arc<Mutex<Entry>>) -> serde_json::Value {
    let guard = lock(entry);
    serde_json::json!({
        "job_id": id.to_string(),
        "state": guard.state.as_str(),
        "log_path": guard.log_path.display().to_string(),
        "message": "Started in the background. Use bash.output for its current state and log; its completion will arrive as a notification.",
    })
}

pub(crate) fn cancel(owner: SessionId, id: Uuid) -> Option<serde_json::Value> {
    let entry = owned_entry(owner, id)?;
    let cancel = {
        let mut guard = lock(&entry);
        if guard.state == JobState::Running {
            guard.state = JobState::Cancelled;
        }
        guard.cancel.take()
    };
    if let Some(cancel) = cancel {
        cancel();
    }
    status(owner, id)
}

/// Store the real terminal result before publishing the notification. A
/// cancelled job retains its actual output and exit information.
pub(crate) fn finish(id: Uuid, state: JobState, result: &BashOutput) {
    let Some(entry) = lock(registry()).entries.get(&id).cloned() else {
        return;
    };
    let (owner, id, log_path, summary) = {
        let mut guard = lock(&entry);
        if guard.state != JobState::Running && guard.state != JobState::Cancelled {
            return;
        }
        if guard.state != JobState::Cancelled {
            guard.state = state;
        }
        guard.cancel = None;
        guard.result = Some(result.clone());
        (
            guard.owner,
            guard.id,
            guard.log_path.clone(),
            result.summary(),
        )
    };
    let output = if let Some(path) = &result.output_file {
        std::fs::read(path).unwrap_or_else(|_| result.output.as_bytes().to_vec())
    } else {
        result.output.as_bytes().to_vec()
    };
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&log_path, output) {
        eprintln!(
            "background bash: could not write {}: {error}",
            log_path.display()
        );
    }
    let notification = format!(
        "A background bash job has finished. This is a system notification, not a message from the user. job_id: {id}; state: {}; summary: {summary}; log: {}. Use bash.output to read it.",
        entry_state(owner, id).as_str(),
        log_path.display()
    );
    lock(registry())
        .pending
        .entry(owner)
        .or_default()
        .push(notification);
    crate::tools::explore::wake(owner);
}

fn entry_state(owner: SessionId, id: Uuid) -> JobState {
    owned_entry(owner, id)
        .map(|entry| lock(&entry).state)
        .unwrap_or(JobState::Failed)
}

/// Start tracking a job and prepare the call that will run it. The caller
/// launches that call through the ordinary bash path (sandbox policy, FIFO,
/// process kill) and replaces it with the returned receipt.
pub(crate) fn launch(
    owner: SessionId,
    identity: &crate::contract::ToolCallIdentity,
    tool_state: &crate::tools::ToolSessionState,
    workspace_root: &Path,
    command: &str,
    result_tx: crossbeam_channel::Sender<crate::tools::ToolCompletion>,
) -> crate::tools::output::Response {
    let cwd = tool_state
        .bash_cwd_handle()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let (id, entry) = register(
        owner,
        &identity.call_id,
        &identity.occurrence_id,
        command.to_string(),
        cwd,
        log_path(workspace_root),
    );
    // The job's own completion marks the entry; the registry keeps the call
    // receipt separate from the ordinary foreground result.
    let _ = result_tx;
    crate::tools::output::Response::external(receipt(id, &entry))
}

/// The job a finished background call belongs to. Identity is the call's
/// own, so no ambiguity with an unrelated call id remains.
pub(crate) fn job_id_for(
    call_id: &crate::contract::ToolCallId,
    occurrence_id: &crate::contract::OccurrenceId,
) -> Option<Uuid> {
    lock(registry())
        .entries
        .iter()
        .find(|(_, entry)| {
            let entry = lock(entry);
            &entry.call_id == call_id && &entry.occurrence_id == occurrence_id
        })
        .map(|(id, _)| *id)
}

/// The session loop drains this alongside finished task children. No polling
/// is needed: the process's terminal event itself schedules the next round.
pub(crate) fn take_notifications(owner: SessionId) -> Vec<String> {
    lock(registry()).pending.remove(&owner).unwrap_or_default()
}

pub(crate) fn output(owner: SessionId, input: &BashJob) -> Response {
    let Ok(id) = Uuid::parse_str(&input.job_id) else {
        return crate::tools::output::error("bash.output requires a valid job id");
    };
    let Some(entry) = owned_entry(owner, id) else {
        return crate::tools::output::error("no bash job with that id belongs to this session");
    };
    let mut value = lock(&entry).status();
    let log_path = lock(&entry).log_path.clone();
    let contents = std::fs::read_to_string(&log_path).unwrap_or_default();
    let (shown, truncated) = crate::transcript::truncate_chars(&contents, 4_000);
    if let Some(object) = value.as_object_mut() {
        object.insert("output".into(), shown.into());
        object.insert("log_truncated".into(), truncated.into());
    }
    Response::external(value)
}

pub(crate) fn cancel_tool(owner: SessionId, input: &BashJob) -> Response {
    let Ok(id) = Uuid::parse_str(&input.job_id) else {
        return crate::tools::output::error("bash.cancel requires a valid job id");
    };
    match cancel(owner, id) {
        Some(value) => Response::external(value),
        None => crate::tools::output::error("no bash job with that id belongs to this session"),
    }
}

/// Stop tracking this session's jobs after teardown. Logs remain on disk.
pub(crate) fn remove_session(owner: SessionId) {
    let mut registry = lock(registry());
    registry
        .entries
        .retain(|_, entry| lock(entry).owner != owner);
    registry.pending.remove(&owner);
}

fn unix_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::output::BashTermination;

    #[test]
    fn jobs_are_scoped_to_their_owner_and_keep_their_log() {
        let owner = SessionId::new();
        let other = SessionId::new();
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("job.log");
        let call_id = crate::contract::ToolCallId("call".into());
        let occurrence_id = crate::contract::OccurrenceId("occ".into());
        let (id, entry) = register(
            owner,
            &call_id,
            &occurrence_id,
            "echo done".into(),
            root.path().into(),
            log,
        );
        assert_eq!(job_id_for(&call_id, &occurrence_id), Some(id));
        let _ = Arc::strong_count(&entry);
        assert_eq!(status(owner, id).unwrap()["state"], "running");
        assert!(status(other, id).is_none());
        let finished = BashOutput {
            termination: BashTermination::Exited { exit_code: 0 },
            message: None,
            output: "done".into(),
            truncated: false,
            output_file: None,
            note: None,
        };
        finish(id, JobState::Finished, &finished);
        assert_eq!(status(owner, id).unwrap()["exit_code"], 0);
        assert_eq!(
            output(
                owner,
                &BashJob {
                    job_id: serde_json::from_value(serde_json::json!(id.to_string()))
                        .expect("job id parses as text")
                }
            )
            .to_json()["output"],
            "done"
        );
        assert_eq!(take_notifications(owner).len(), 1);
        remove_session(owner);
    }
}
