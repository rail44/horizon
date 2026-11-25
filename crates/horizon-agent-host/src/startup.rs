//! Opening the session logs and resuming their sessions when the host
//! starts, off the path that answers the client: [`spawn_resume_task`].
//!
//! The host takes the directory of session logs for itself (a lock beside
//! it: one process hosts a data home's sessions at a time), splits the
//! single log every session shared before into it once, starts the writer,
//! and resumes every session whose latest state did not end it. After
//! that it catches the DuckDB projection up, which recall also does before
//! each query (`docs/agent-stdio-child-design.md`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use horizon_agent::config::AgentConfig;
use horizon_agent::persistence::event_log::{
    UnsupportedEventLogFormat, WriterHandle, session_ids, split_legacy_log,
};

use crate::session::{self, AgentdState};

/// What the caller of [`spawn_resume_task`] decides around startup.
pub struct StartupHooks {
    /// Runs before the session logs are opened.
    pub before_resume: Option<Box<dyn FnOnce() + Send>>,
    /// Runs when startup is refused (the logs cannot be taken or read);
    /// the host is then not ready and serves no session.
    pub refused: Box<dyn FnOnce(anyhow::Error) + Send>,
}

/// Takes the session logs, resumes their sessions and marks `state` ready,
/// on a blocking task that races the caller's accept loop.
pub fn spawn_resume_task(state: Arc<AgentdState>, agent_config: AgentConfig, hooks: StartupHooks) {
    let StartupHooks {
        before_resume,
        refused,
    } = hooks;
    tokio::task::spawn_blocking(move || {
        if let Some(before_resume) = before_resume {
            before_resume();
        }
        if let Err(error) = open_persistence(&state, &agent_config) {
            // No sessions have passed the readiness gate yet. Refuse logs
            // that cannot be taken rather than run without history.
            refused(error);
            return;
        }
        session::resume_persisted_sessions(&state);
        state.mark_resume_ready();
        catch_up_projection(&state);
    });
}

/// Catches the projection up with the session logs once the sessions are
/// resumed, so a rebuild (the first start after the logs were split, or a
/// new projection format: minutes over a long history) is not waited out
/// by a recall. A recall meanwhile waits for the file, then reports it
/// unavailable.
fn catch_up_projection(state: &AgentdState) {
    let Some(projection) = state.projection() else {
        return;
    };
    let start = std::time::Instant::now();
    match projection.catch_up() {
        Ok(()) => eprintln!(
            "horizon-agent: history search caught up in {:.1}s",
            start.elapsed().as_secs_f64()
        ),
        Err(error) => eprintln!("horizon-agent: catching history search up failed: {error:#}"),
    }
}

/// Takes the session logs' directory, splits a shared log into it, and
/// starts the writer.
fn open_persistence(state: &AgentdState, agent_config: &AgentConfig) -> Result<()> {
    let persistence = &agent_config.persistence;
    let dir = &persistence.sessions_dir;
    state.hold_host_lock(lock_sessions(dir)?);
    if let Some(legacy) = &persistence.legacy_event_log
        && legacy.exists()
    {
        split_shared_log(legacy, dir)?;
    }
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create agent session log directory {}", dir.display()))?;
    state.set_writer(Some(WriterHandle::open(dir)));
    Ok(())
}

/// Locks `<dir>.lock`, beside the directory, for as long as the returned
/// file is kept: another process hosting the same sessions is refused.
fn lock_sessions(dir: &Path) -> Result<std::fs::File> {
    let path = lock_path(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
            "another process hosts the agent sessions in {} (it holds {})",
            dir.display(),
            path.display()
        ),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("lock {}", path.display()))
        }
    }
}

fn lock_path(dir: &Path) -> PathBuf {
    let mut name = dir.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Splits the log every session shared into `dir`, then moves it aside.
/// When `dir` already holds sessions, an earlier start split it and was
/// stopped before moving it aside. A log of another version is moved
/// aside unsplit, as a format change always was: its sessions are not
/// resumed.
fn split_shared_log(legacy: &Path, dir: &Path) -> Result<()> {
    if session_ids(dir)?.is_empty() {
        match split_legacy_log(legacy, dir) {
            Ok(report) => eprintln!(
                "horizon-agent: split {} into {} session log(s) ({} line(s){})",
                legacy.display(),
                report.sessions,
                report.lines,
                if report.dropped_lines > 0 || report.ignored_partial_line {
                    format!(
                        "; left behind {} line(s) naming no session{}",
                        report.dropped_lines,
                        if report.ignored_partial_line {
                            " and a torn last line"
                        } else {
                            ""
                        }
                    )
                } else {
                    String::new()
                }
            ),
            Err(error) if error.is::<UnsupportedEventLogFormat>() => {
                eprintln!("horizon-agent: {error}");
                move_aside(legacy, "pre")?;
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
    move_aside(legacy, "split")
}

/// Renames `path` to `<path>.<label>-<milliseconds since the epoch>`.
fn move_aside(path: &Path, label: &str) -> Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".{label}-{stamp}"));
    let aside = PathBuf::from(aside);
    std::fs::rename(path, &aside)
        .with_context(|| format!("move {} aside to {}", path.display(), aside.display()))?;
    eprintln!(
        "horizon-agent: moved {} aside to {}",
        path.display(),
        aside.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(session: horizon_agent::contract::SessionId, version: u32) -> String {
        serde_json::json!({
            "schema": "horizon.agent.event_log",
            "version": version,
            "event_id": uuid::Uuid::new_v4().to_string(),
            "parent_id": null,
            "sequence": 0,
            "session_id": session,
            "turn_id": null,
            "provider_id": null,
            "role_id": null,
            "session_context": null,
            "event_kind": "state_changed",
            "event": {"StateChanged": "Terminated"},
            "provider_payload": null,
            "created_at_unix_ms": 1,
        })
        .to_string()
    }

    fn aside(home: &Path, label: &str) -> Vec<PathBuf> {
        std::fs::read_dir(home)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.to_string_lossy()
                    .contains(&format!("agent-events.jsonl.{label}-"))
            })
            .collect()
    }

    #[test]
    fn a_shared_log_is_split_into_session_logs_and_moved_aside() {
        let home = tempfile::tempdir().unwrap();
        let session = horizon_agent::contract::SessionId::new();
        let legacy = home.path().join("agent-events.jsonl");
        let original = format!("{}\n", line(session, 5));
        std::fs::write(&legacy, &original).unwrap();
        let dir = home.path().join("sessions");

        split_shared_log(&legacy, &dir).unwrap();

        assert_eq!(session_ids(&dir).unwrap(), [session]);
        let aside = aside(home.path(), "split");
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), original);
    }

    #[test]
    fn an_older_log_is_moved_aside_and_no_session_comes_from_it() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join("agent-events.jsonl");
        std::fs::write(
            &legacy,
            format!("{}\n", line(horizon_agent::contract::SessionId::new(), 1)),
        )
        .unwrap();
        let dir = home.path().join("sessions");

        split_shared_log(&legacy, &dir).unwrap();

        assert!(session_ids(&dir).unwrap().is_empty());
        assert_eq!(aside(home.path(), "pre").len(), 1);
        assert!(!legacy.exists());
    }

    #[test]
    fn a_second_process_cannot_take_the_same_sessions() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("sessions");
        let held = lock_sessions(&dir).unwrap();
        // Another process: a lock taken through another open file.
        let refused = lock_sessions(&dir).unwrap_err();
        assert!(
            format!("{refused:#}").contains("another process hosts"),
            "{refused:#}"
        );
        drop(held);
        assert!(lock_sessions(&dir).is_ok());
    }
}
