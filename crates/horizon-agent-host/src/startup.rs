//! Opening the event log and resuming its sessions when the host starts,
//! off the path that answers the client: [`spawn_resume_task`].
//!
//! **The DuckDB projection and readiness.** The event log's own background
//! writer thread (`horizon_agent::persistence::event_log::writer`) owns
//! the projection: `open_persistence` passes
//! `agent_config.persistence.duckdb_path` into
//! [`WriterHandle::open_silently`], and the writer keeps the opened `Store`
//! for the rest of the process's life, projecting every append live
//! (`docs/agent-duckdb-state-design.md`'s "Runtime Boundary" addendum).
//! Startup takes one of two modes (that doc's "Incremental startup"
//! addendum):
//!
//! - **Incremental.** The projection's format stamp is current and the
//!   log line at its cursor still matches. Opening the store, reading the
//!   log after the cursor, and catching that tail up into the projection
//!   all happen *before* `WriterInit::Ready`, so they are on the readiness
//!   path -- deliberately, because they are small and resume then reads
//!   the projection instead of the log
//!   ([`crate::session::resume_persisted_sessions_from_projection`]).
//! - **Full.** Anything else (first start, a stale stamp, a cursor that no
//!   longer matches the log). The whole log is read, `WriterInit::Ready`
//!   carries every record, and the full rebuild of the projection runs on
//!   its own thread after `Ready` -- off the readiness path, and off the
//!   writer too: appends and flushes (resume's fixup commits included) go
//!   through meanwhile, and the writer catches the rebuilt store up with
//!   them before sharing it. Readiness never waits on a full rebuild of a
//!   derived, non-authoritative read model, since this mode resumes
//!   sessions from the JSONL records directly. A drain during a rebuild
//!   abandons it; the next start rebuilds again.

use std::path::PathBuf;
use std::sync::Arc;

use horizon_agent::config::AgentConfig;
use horizon_agent::persistence::event_log::{LogCursor, Record, WriterHandle, WriterInit};
use horizon_agent::persistence::projection::duckdb::{DuckdbStoreHandle, SharedDuckdbStore};

use crate::session::{self, AgentdState};

/// A step a caller holds startup at, for as long as the call lasts.
pub type Gate = Arc<dyn Fn() + Send + Sync>;

/// What the caller of [`spawn_resume_task`] decides around startup.
pub struct StartupHooks {
    /// Runs before the event log is opened.
    pub before_resume: Option<Box<dyn FnOnce() + Send>>,
    /// Runs before a full rebuild of the projection.
    pub before_rebuild: Option<Gate>,
    /// Runs when startup is refused (the log cannot be opened or read);
    /// the host is then not ready and serves no session.
    pub refused: Box<dyn FnOnce(anyhow::Error) + Send>,
}

struct PersistenceStartup {
    writer: Option<WriterHandle>,
    /// Every record in the full mode; only the records after `read_after`
    /// in the incremental mode.
    records: Vec<Record>,
    /// `Some` in the incremental mode: the projection cursor the startup
    /// read began after.
    read_after: Option<LogCursor>,
    max_known_sequence: Option<u64>,
    skipped_lines_summary: Option<String>,
    duckdb_ready_rx: crossbeam_channel::Receiver<Option<DuckdbStoreHandle>>,
}

/// Opens this process's own event log writer, resumes every session found
/// in it, and marks `state` ready -- all on a background task that races
/// the accept loop `main` starts right after this returns, per the
/// module's bind-first fix. Decision 3 in the design doc ("the child owns
/// the event log and DuckDB projection") still holds: this uses this
/// binary's own config load, never Horizon's.
///
/// What readiness waits on depends on the writer's startup mode (see the
/// module doc). In the incremental mode the writer has already delivered
/// the caught-up store, so this task takes it, publishes it to
/// `duckdb_cell`, and resumes from it; if that fails before anything was
/// written it falls back to reading the whole log. In the full mode
/// `duckdb_cell` is populated by a *separate*, fire-and-forget
/// `spawn_blocking` task spawned after `mark_resume_ready` -- deliberately
/// not awaited by (or ordered against) `resume_persisted_sessions`/
/// `mark_resume_ready`, so the full rebuild never lands on the readiness
/// path.
pub fn spawn_resume_task(
    state: Arc<AgentdState>,
    agent_config: AgentConfig,
    duckdb_cell: SharedDuckdbStore,
    hooks: StartupHooks,
) {
    let StartupHooks {
        before_resume,
        before_rebuild,
        refused,
    } = hooks;
    tokio::task::spawn_blocking(move || {
        if let Some(before_resume) = before_resume {
            before_resume();
        }
        let PersistenceStartup {
            writer,
            records,
            read_after,
            max_known_sequence,
            skipped_lines_summary,
            duckdb_ready_rx,
        } = match open_persistence(&agent_config, before_rebuild.as_ref()) {
            Ok(persistence) => persistence,
            Err(error) => {
                // No sessions have passed the readiness gate yet. Refuse
                // a missing or incompatible log rather than run without history.
                refused(error);
                return;
            }
        };
        state.set_writer(writer);
        state.set_skipped_lines_summary(skipped_lines_summary);

        let Some(cursor) = read_after else {
            // Step 4: "agentd restart = read own log, rebuild rig_history,
            // mark turns that died mid-flight as cancelled ... sessions are
            // live again".
            session::resume_persisted_sessions(&state, records);
            state.mark_resume_ready();

            // Propagates the writer thread's own rebuild-or-open decision
            // into the shared cell whenever it lands --
            // `duckdb_ready_rx.recv()` blocks this dedicated propagator task,
            // never the resume/readiness path above (already completed by
            // the time this line runs). A disconnected channel (the writer's
            // startup itself failed, so it never reached the DuckDB step at
            // all) is treated the same as an explicit "no store".
            tokio::task::spawn_blocking(move || {
                let store = duckdb_ready_rx.recv().ok().flatten();
                duckdb_cell.set(store);
            });
            return;
        };

        // The incremental mode sends the store before `Ready`, so this
        // never waits.
        let store = duckdb_ready_rx.recv().ok().flatten();
        duckdb_cell.set(store.clone());
        let through_sequence =
            max_known_sequence.map_or(cursor.sequence, |sequence| sequence.max(cursor.sequence));
        let resumed = match &store {
            Some(store) => session::resume_persisted_sessions_from_projection(
                &state,
                store,
                &records,
                through_sequence,
            ),
            None => Err(anyhow::anyhow!("the projection is unavailable")),
        };
        if let Err(error) = resumed {
            eprintln!(
                "horizon-agentd: resuming from the DuckDB projection failed ({error:#}); \
                 reading the whole event log"
            );
            match read_whole_log(&state, &agent_config) {
                Ok(records) => session::resume_persisted_sessions(&state, records),
                Err(error) => {
                    refused(error);
                    return;
                }
            }
        }
        state.mark_resume_ready();
    });
}

/// The incremental mode's fallback: everything the writer has made durable,
/// read the same way the full mode's startup read does.
fn read_whole_log(state: &AgentdState, agent_config: &AgentConfig) -> anyhow::Result<Vec<Record>> {
    if let Some(writer) = state.writer() {
        writer.flush()?;
    }
    Ok(
        horizon_agent::persistence::event_log::read(&agent_config.persistence.event_log_path)?
            .records,
    )
}

/// Opens this process's own event log writer, using this binary's own
/// config load (never Horizon's). Blocks the calling (background, per
/// [`spawn_resume_task`]) task on the writer's startup read -- simpler than
/// threading the `WriterInit` channel further, and no longer something the
/// accept loop waits on (see the module doc). Also hands back the startup
/// read's records (empty if persistence is disabled or there's nothing yet;
/// only the tail after the projection cursor in the incremental mode, with
/// that cursor in `read_after`) so the caller can resume every session
/// ([`session::resume_persisted_sessions`] or
/// [`session::resume_persisted_sessions_from_projection`]), and the
/// human-readable
/// skipped-lines summary (if any corrupt/torn lines were found) so
/// [`spawn_resume_task`] can stash it on [`AgentdState`] for the hub's
/// `initialize` to forward to a connecting client as `_horizon/session_event`
/// -- restoring the step-3 trim recorded in
/// `docs/agent-runtime-split-design.md`
/// ("Skipped-lines status reporting is omitted").
///
/// Opens via [`WriterHandle::open_silently`] rather than [`WriterHandle::
/// open`]: this function already prints its own `horizon-agentd`-prefixed
/// skipped-lines summary just below, so the shared writer module's own
/// generic summary line would otherwise double up in agentd's stderr.
/// `open_silently`'s `duckdb_path` argument is this binary's only real
/// production use of the live DuckDB projection (see the module doc): the
/// event log's own writer thread either catches the projection up from its
/// cursor before `Ready` or rebuilds it after `Ready`, and then keeps that
/// `Store` open for the rest of this process's life.
///
/// The fourth return value is `open_silently`'s own second receiver: the
/// writer thread's DuckDB decision, delivered exactly once: before `Ready`
/// in the incremental mode, once the rebuilt store has caught up in the
/// full mode (see [`spawn_resume_task`], its only consumer). A disconnected
/// channel
/// (writer startup itself failed)
/// is indistinguishable from -- and handled the same as -- an explicit
/// `None`.
fn open_persistence(
    agent_config: &AgentConfig,
    before_rebuild: Option<&Gate>,
) -> anyhow::Result<PersistenceStartup> {
    match open_persistence_once(agent_config, before_rebuild) {
        Err(error)
            if error.is::<horizon_agent::persistence::event_log::UnsupportedEventLogFormat>() =>
        {
            eprintln!("horizon-agentd: {error}");
            move_old_history_aside(agent_config)?;
            open_persistence_once(agent_config, before_rebuild)
        }
        opened => opened,
    }
}

/// Moves a log this build cannot read, and the DuckDB projection built from
/// it (with its WAL), aside under a `.pre-<time>` name, so the daemon starts
/// with a fresh history. The old sessions are not resumed.
fn move_old_history_aside(agent_config: &AgentConfig) -> anyhow::Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let log = &agent_config.persistence.event_log_path;
    let mut paths = vec![log.clone()];
    if let Some(duckdb) = &agent_config.persistence.duckdb_path {
        paths.push(duckdb.clone());
        let mut wal = duckdb.clone().into_os_string();
        wal.push(".wal");
        paths.push(PathBuf::from(wal));
    }
    for path in paths.into_iter().filter(|path| path.exists()) {
        let mut aside = path.clone().into_os_string();
        aside.push(format!(".pre-{stamp}"));
        let aside = PathBuf::from(aside);
        std::fs::rename(&path, &aside)?;
        eprintln!(
            "horizon-agentd: moved {} aside to {}",
            path.display(),
            aside.display()
        );
    }
    Ok(())
}

fn open_persistence_once(
    agent_config: &AgentConfig,
    before_rebuild: Option<&Gate>,
) -> anyhow::Result<PersistenceStartup> {
    let before_rebuild = before_rebuild.map(|gate| {
        let gate = gate.clone();
        Box::new(move || gate()) as horizon_agent::persistence::event_log::BeforeRebuild
    });
    let (writer, init_rx, duckdb_rx) = WriterHandle::open_silently(
        &agent_config.persistence.event_log_path,
        agent_config.persistence.duckdb_path.clone(),
        before_rebuild,
    );
    match init_rx.recv() {
        Ok(WriterInit::Ready(report)) => {
            let skipped_summary = report.skipped_summary();
            if let Some(summary) = &skipped_summary {
                eprintln!(
                    "horizon-agentd: {summary} while opening {}",
                    agent_config.persistence.event_log_path.display()
                );
            }
            Ok(PersistenceStartup {
                writer: Some(writer),
                records: report.records,
                read_after: report.read_after,
                max_known_sequence: report.max_known_sequence,
                skipped_lines_summary: skipped_summary,
                duckdb_ready_rx: duckdb_rx,
            })
        }
        Ok(WriterInit::Failed(error)) => Err(error),
        Err(error) => Err(anyhow::anyhow!(
            "Event log writer exited before startup: {error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_log_is_moved_aside_and_a_fresh_one_starts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut record = state_record(horizon_agent::contract::SessionId::new(), 7);
        record.version = 1;
        let original = format!("{}\n", serde_json::to_string(&record).unwrap());
        std::fs::write(&path, &original).unwrap();
        let mut config = crate::session::test_support::test_config();
        config.persistence.event_log_path = path.clone();
        let Ok(startup) = open_persistence(&config, None) else {
            panic!("an older log is moved aside rather than refusing startup");
        };
        assert!(startup.records.is_empty(), "the fresh log holds nothing");
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().contains("events.jsonl.pre-"))
            .collect();
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), original);
    }

    #[test]
    fn failed_log_open_refuses_startup_instead_of_running_without_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::session::test_support::test_config();
        config.persistence.event_log_path = dir.path().to_path_buf();
        assert!(open_persistence(&config, None).is_err());
    }

    fn state_record(session_id: horizon_agent::contract::SessionId, sequence: u64) -> Record {
        Record {
            schema: "horizon.agent.event_log".to_string(),
            version: 5,
            event_id: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            sequence,
            session_id,
            turn_id: None,
            provider_id: None,
            role_id: None,
            session_context: None,
            event_kind: "state_changed".to_string(),
            event: horizon_agent::contract::Event::StateChanged(
                horizon_agent::contract::SessionState::Running,
            ),
            provider_payload: None,
            created_at_unix_ms: sequence + 1,
        }
    }
}
