//! A serial milestone coordinator. Durable reservations precede session launch;
//! a report and a real turn boundary are both required to advance the plan.

mod monitor;
pub(crate) mod roles;

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract::{Command, SessionId};
use horizon_agent::roles::RoleId;
use horizon_board::workflow::{Mutation, Work, Worker};
use horizon_board::{is_closed_status, Item, Store};

use crate::session::{spawn_session_thread, AgentdState};
use crate::worktree;
use monitor::Monitor;

pub(crate) fn spawn(state: Arc<AgentdState>, root: Option<PathBuf>) {
    let Some(root) = root else {
        return;
    };
    tokio::spawn(async move {
        state.wait_until_resume_ready().await;
        if let Err(error) = run(state, root).await {
            eprintln!("horizon-agentd: milestone coordinator: {error}");
        }
    });
}

fn lease(store: &Store) -> Result<File, String> {
    let path = store.path().with_file_name("milestone-coordinator.lock");
    std::fs::create_dir_all(path.parent().ok_or("Board path has no parent")?)
        .map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.try_lock()
        .map_err(|e| format!("Cannot acquire milestone coordinator lease: {e}"))?;
    Ok(file)
}

async fn run(state: Arc<AgentdState>, root: PathBuf) -> Result<(), String> {
    let store = Store::from_dir(&root).map_err(|e| e.to_string())?;
    let _lease = lease(&store)?;
    // Resume never restarts an interrupted model turn. Explicit retry first
    // inspects the preserved worktree; a restart must not duplicate work.
    for item in store.list(None, true).map_err(|e| e.to_string())?.items {
        if let Some(active) = item.workflow.as_ref().and_then(|f| f.active.as_ref()) {
            update(&store, item.id, Mutation::Interrupt {
                token: active.token.clone(),
                reason: "Agent runtime restarted during this attempt. The worktree is preserved; inspect it, then retry or revise the plan.".into(),
            }).await?;
        }
    }
    let mut monitor: Option<Monitor> = None;
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    loop {
        interval.tick().await;
        if let Some(running) = &mut monitor {
            let item = store
                .show(running.item)
                .map_err(|e| e.to_string())?
                .ok_or("Active milestone disappeared")?;
            let flow = item
                .workflow
                .as_ref()
                .ok_or("Active milestone disappeared")?;
            if (flow.paused || is_closed_status(&item.status)) && !running.cancelled {
                state.send_command(running.session, Command::Cancel { request_id: None });
                running.cancelled = true;
            }
            running.poll(&state);
            if running.done {
                let mutation = if let Some(reason) = running.failure.clone() {
                    Mutation::Interrupt {
                        token: running.token.clone(),
                        reason,
                    }
                } else {
                    Mutation::Finish {
                        token: running.token.clone(),
                    }
                };
                // Keep the monitor until persistence succeeds. A transient
                // logd failure must not lose the only observed turn boundary.
                match update(&store, running.item, mutation).await {
                    Ok(_) => {
                        state.unsubscribe_from_session(running.session);
                        monitor = None;
                    }
                    Err(error) => eprintln!("horizon-agentd: milestone result: {error}"),
                }
            } else if flow
                .active
                .as_ref()
                .is_some_and(|a| a.attention != running.attention)
            {
                let _ = update(
                    &store,
                    running.item,
                    Mutation::Attention {
                        token: running.token.clone(),
                        message: running.attention.clone(),
                    },
                )
                .await;
            }
            continue;
        }
        let items = match store.list(None, false) {
            Ok(items) => items.items,
            Err(error) => {
                eprintln!("horizon-agentd: milestone read: {error}");
                continue;
            }
        };
        // Board rank orders milestones; plan order and dependencies order
        // their tasks. At most one attempt is live for this project.
        let Some((item, work)) = items.into_iter().find_map(|item| {
            let work = item.workflow.as_ref()?.next_work()?;
            Some((item, work))
        }) else {
            continue;
        };
        match launch(&state, &store, &root, item, work).await {
            Ok(running) => monitor = Some(running),
            Err(error) => eprintln!("horizon-agentd: milestone launch: {error}"),
        }
    }
}

async fn update(store: &Store, id: u64, mutation: Mutation) -> Result<Item, String> {
    let item = store
        .show(id)
        .map_err(|e| e.to_string())?
        .ok_or("Milestone not found")?;
    let revision = item
        .workflow
        .as_ref()
        .ok_or("Item is not a milestone")?
        .revision;
    store
        .workflow(id, revision, mutation)
        .await
        .map_err(|e| e.to_string())
}

fn session_id(value: &str) -> Result<SessionId, String> {
    uuid::Uuid::parse_str(value)
        .map(SessionId::from_uuid)
        .map_err(|e| e.to_string())
}

async fn launch(
    state: &Arc<AgentdState>,
    store: &Store,
    root: &std::path::Path,
    item: Item,
    work: Work,
) -> Result<Monitor, String> {
    let flow = item.workflow.as_ref().ok_or("Item is not a milestone")?;
    let planner = work == Work::Plan;
    let session = if !planner {
        flow.worker
            .as_ref()
            .map(|w| session_id(&w.session))
            .transpose()?
            .unwrap_or_default()
    } else {
        SessionId::new()
    };
    let token = uuid::Uuid::new_v4().to_string();
    let item = store
        .workflow(
            item.id,
            flow.revision,
            Mutation::Start {
                token: token.clone(),
                session: session.as_uuid().to_string(),
                work: work.clone(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    match prepare_and_send(state, store, root, &item, session, &token, &work).await {
        Ok(subscription) => Ok(Monitor::new(item.id, token, subscription)),
        Err(reason) => {
            state.unsubscribe_from_session(session);
            update(
                store,
                item.id,
                Mutation::Interrupt {
                    token,
                    reason: reason.clone(),
                },
            )
            .await?;
            Err(reason)
        }
    }
}

async fn prepare_and_send(
    state: &Arc<AgentdState>,
    store: &Store,
    root: &std::path::Path,
    item: &Item,
    session: SessionId,
    token: &str,
    work: &Work,
) -> Result<crate::session::SessionSubscription, String> {
    let flow = item.workflow.as_ref().ok_or("Item is not a milestone")?;
    let planner = work == &Work::Plan;
    let mut restored = None;
    let mut directory = root.to_path_buf();
    if planner {
        if let Some(worker) = &flow.worker {
            directory = PathBuf::from(&worker.worktree);
        }
    } else if let Some(worker) = &flow.worker {
        directory = PathBuf::from(&worker.worktree);
        if state.session_directory(session) != Some((directory.clone(), true)) {
            return Err(format!("Implementation session {} is unavailable or no longer owns {}. Restore the session before retrying; existing work has not been replaced.", worker.session, worker.worktree));
        }
    } else {
        let source = root.to_path_buf();
        let info = tokio::task::spawn_blocking(move || {
            worktree::create_isolated_worktree(&source, session.as_uuid())
        })
        .await
        .map_err(|e| e.to_string())??;
        directory = info.path.clone();
        update(
            store,
            item.id,
            Mutation::SetWorker {
                worker: Worker {
                    session: session.as_uuid().to_string(),
                    worktree: info.path.to_string_lossy().into_owned(),
                    branch: info.branch.clone(),
                },
            },
        )
        .await?;
        restored = Some(info);
    }
    // Isolation is created before provider startup and never falls back to
    // writing in the source checkout when Git fails.
    let subscription = state.subscribe_to_session(session);
    if !state.session_exists(session) {
        let provider = state.providers.lock().unwrap().default_provider_id();
        spawn_session_thread(
            state.clone(),
            session,
            provider,
            Some(RoleId(
                if planner {
                    roles::PLANNER
                } else {
                    roles::WORKER
                }
                .into(),
            )),
            Some(directory),
            None,
            false,
            restored,
            Vec::new(),
        );
    }
    let latest = store
        .show(item.id)
        .map_err(|e| e.to_string())?
        .ok_or("Milestone disappeared during launch")?;
    if is_closed_status(&latest.status) || latest.workflow.as_ref().is_none_or(|flow| flow.paused) {
        return Err("The milestone was paused or closed before accepting the assignment".into());
    }
    let assignment = serde_json::json!({
        "milestone": item.id, "title": item.title, "goal": item.body,
        "attempt": token, "work": work,
    });
    let prompt = format!("Milestone assignment: {assignment}\nRead board.read id={} for the current plan, owner answers, comments and prior results. {} Save the assigned result with board.report using id={} and attempt={token}. If resuming after interruption, inspect partial changes before doing more work.",
        item.id, if planner { "Investigate and save a plan." } else { "Implement and verify this task only. Preserve prior task changes." }, item.id);
    if !state.send_command(session, Command::UserMessage { text: prompt }) {
        return Err("Session exited before accepting the assignment".into());
    }
    Ok(subscription)
}
