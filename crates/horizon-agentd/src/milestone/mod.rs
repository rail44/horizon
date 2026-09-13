//! Concurrent board execution with durable reservations and verified integration.

mod git;
mod launch;
mod monitor;
pub(crate) mod roles;

use crate::session::AgentdState;
use horizon_agent::contract::{Command, SessionId};
use horizon_board::workflow::{eligible_work, ordered_items, Mutation, Report, Work};
use horizon_board::{Item, Store};
use monitor::Monitor;

struct IntegrationJob {
    id: u64,
    commit: String,
    handle: tokio::task::JoinHandle<Result<git::MergeOutcome, String>>,
}
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn spawn(state: Arc<AgentdState>, root: Option<PathBuf>) {
    let Some(root) = root else {
        return;
    };
    tokio::spawn(async move {
        state.wait_until_resume_ready().await;
        if let Err(error) = run(state, root).await {
            eprintln!("horizon-agentd: board coordinator: {error}");
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
        .map_err(|e| format!("Cannot acquire board coordinator lease: {e}"))?;
    Ok(file)
}

fn read_items(store: &Store) -> Result<HashMap<u64, Item>, String> {
    Ok(store
        .list(None, true)
        .map_err(|e| e.to_string())?
        .items
        .into_iter()
        .map(|i| (i.id, i))
        .collect())
}

pub(super) async fn update(store: &Store, id: u64, mutation: Mutation) -> Result<Item, String> {
    for _ in 0..4 {
        let item = store
            .show(id)
            .map_err(|e| e.to_string())?
            .ok_or("Board item disappeared")?;
        let revision = item.workflow.as_ref().map_or(0, |f| f.revision);
        match store.workflow(id, revision, mutation.clone()).await {
            Ok(item) => return Ok(item),
            Err(e) if e.to_string().contains("changed; reload") => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Board kept changing while saving the operation".into())
}

pub(super) fn session_id(value: &str) -> Result<SessionId, String> {
    uuid::Uuid::parse_str(value)
        .map(SessionId::from_uuid)
        .map_err(|e| e.to_string())
}

fn stop_reason(items: &HashMap<u64, Item>, id: u64) -> Option<String> {
    let item = items.get(&id)?;
    let flow = item.workflow.as_ref()?;
    if flow.paused || horizon_board::is_closed_status(&item.status) {
        return Some("The owner paused or closed this work".into());
    }
    if let Some(parent) = item.parent.and_then(|p| items.get(&p)) {
        if horizon_board::is_closed_status(&parent.status)
            || parent.workflow.as_ref().is_some_and(|p| p.paused)
        {
            return Some("The milestone was paused or closed".into());
        }
        if horizon_board::workflow::task_waits_for_decision(items, id) {
            return Some("An unresolved decision now affects this task".into());
        }
    }
    None
}

async fn finish_monitor(
    state: &Arc<AgentdState>,
    store: &Store,
    root: &std::path::Path,
    running: &mut Monitor,
) -> Result<(), String> {
    let item = store
        .show(running.item)
        .map_err(|e| e.to_string())?
        .ok_or("Active item disappeared")?;
    let flow = item
        .workflow
        .as_ref()
        .ok_or("Active workflow disappeared")?;
    let Some(active) = &flow.active else {
        state.unsubscribe_from_session(running.session);
        return Ok(());
    };
    if running.failure.is_none() {
        let check = match &active.report {
            Some(Report::Task { commit, .. }) => flow
                .worker
                .as_ref()
                .ok_or("Task has no worktree")
                .map_err(str::to_string)
                .and_then(git::clean_head)
                .and_then(|head| {
                    if &head == commit {
                        Ok(())
                    } else {
                        Err("Reported commit is not the task's clean HEAD".into())
                    }
                })
                .and_then(|()| {
                    git::check_scope(
                        root,
                        flow.worker.as_ref().unwrap(),
                        &flow.task.as_ref().ok_or("Missing task scope")?.scope,
                    )
                }),
            Some(Report::Verification { verification: v }) => {
                if v.checks
                    .iter()
                    .any(|command| !running.successful_checks.contains(command))
                {
                    Err(
                        "Verification named a command without a successful execution in this turn"
                            .into(),
                    )
                } else {
                    flow.verifier
                        .as_ref()
                        .ok_or("Verification has no worktree")
                        .map_err(str::to_string)
                        .and_then(git::clean_head)
                        .and_then(|head| {
                            if head != v.commit {
                                return Err(
                                    "The verified worktree changed before completion".into()
                                );
                            }
                            if flow.is_milestone()
                                && flow.integration.as_ref().is_some_and(|i| {
                                    git::run(root, &["rev-parse", "refs/heads/main"])
                                        .ok()
                                        .as_ref()
                                        != Some(&i.base)
                                })
                            {
                                return Err("Main changed during milestone verification".into());
                            }
                            Ok(())
                        })
                }
            }
            _ => Ok(()),
        };
        if let Err(error) = check {
            running.failure = Some(error);
        }
    }
    let mutation = if let Some(reason) = &running.failure {
        Mutation::Interrupt {
            token: running.token.clone(),
            reason: reason.clone(),
        }
    } else {
        Mutation::Finish {
            token: running.token.clone(),
        }
    };
    match update(store, running.item, mutation).await {
        Ok(_) => {}
        Err(error)
            if matches!(active.work, Work::Plan | Work::Discuss { .. })
                && !error.contains("I/O") =>
        {
            // Results or owner replies may invalidate a provisional plan.
            update(
                store,
                running.item,
                Mutation::Interrupt {
                    token: running.token.clone(),
                    reason: error,
                },
            )
            .await?;
            update(store, running.item, Mutation::Replan).await?;
        }
        Err(error) => return Err(error),
    }
    if running.failure.as_deref() == Some("Main changed during milestone verification") {
        update(
            store,
            running.item,
            Mutation::Reverify {
                reason: running.failure.clone().unwrap(),
            },
        )
        .await?;
    }
    state.unsubscribe_from_session(running.session);
    Ok(())
}

async fn run(state: Arc<AgentdState>, root: PathBuf) -> Result<(), String> {
    let store = Store::from_dir(&root).map_err(|e| e.to_string())?;
    let _lease = lease(&store)?;
    for item in read_items(&store)?.values() {
        if let Some(active) = item.workflow.as_ref().and_then(|w| w.active.as_ref()) {
            update(
                &store,
                item.id,
                Mutation::Restart {
                    token: active.token.clone(),
                },
            )
            .await?;
        }
    }
    let mut monitors: HashMap<u64, Monitor> = HashMap::new();
    let mut launching: HashMap<u64, tokio::task::JoinHandle<Result<Monitor, String>>> =
        HashMap::new();
    let mut integrating: Option<IntegrationJob> = None;
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    loop {
        interval.tick().await;
        let finished: Vec<_> = launching
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| *id)
            .collect();
        for id in finished {
            match launching.remove(&id).unwrap().await {
                Ok(Ok(monitor)) => {
                    monitors.insert(id, monitor);
                }
                result => eprintln!(
                    "horizon-agentd: board launch #{id}: {}",
                    match result {
                        Ok(Err(e)) => e,
                        Err(e) => e.to_string(),
                        _ => unreachable!(),
                    }
                ),
            }
        }
        let items = match read_items(&store) {
            Ok(items) => items,
            Err(e) => {
                eprintln!("horizon-agentd: board read: {e}");
                continue;
            }
        };
        let mut finished = Vec::new();
        for (id, running) in &mut monitors {
            if let Some(reason) = stop_reason(&items, *id) {
                if !running.cancelled {
                    state.send_command(running.session, Command::Cancel { request_id: None });
                    running.cancelled = true;
                    running.failure = Some(reason);
                }
            }
            running.poll(&state);
            if running.done {
                match finish_monitor(&state, &store, &root, running).await {
                    Ok(()) => finished.push(*id),
                    Err(e) => eprintln!("horizon-agentd: board result #{id}: {e}"),
                }
            } else if items
                .get(id)
                .and_then(|i| i.workflow.as_ref())
                .and_then(|w| w.active.as_ref())
                .is_some_and(|a| a.attention != running.attention)
            {
                let _ = update(
                    &store,
                    *id,
                    Mutation::Attention {
                        token: running.token.clone(),
                        message: running.attention.clone(),
                    },
                )
                .await;
            }
        }
        for id in finished {
            monitors.remove(&id);
        }
        if integrating
            .as_ref()
            .is_some_and(|job| job.handle.is_finished())
        {
            let IntegrationJob { id, commit, handle } = integrating.take().unwrap();
            let mutation = match handle.await {
                Ok(Ok(git::MergeOutcome::Integrated)) => Mutation::Integrated { commit },
                Ok(Ok(git::MergeOutcome::Stale)) => Mutation::Reverify {
                    reason: "Main advanced; verify the newly combined state".into(),
                },
                result => {
                    let error = match result {
                        Ok(Err(e)) => e,
                        Err(e) => e.to_string(),
                        _ => unreachable!(),
                    };
                    eprintln!("horizon-agentd: board integration #{id}: {error}");
                    // Preserve the candidate and expose a task-local problem.
                    Mutation::IntegrationFailed { reason: error }
                }
            };
            if let Err(e) = update(&store, id, mutation).await {
                eprintln!("horizon-agentd: integration persistence #{id}: {e}");
            }
        }
        let items = match read_items(&store) {
            Ok(items) => items,
            Err(error) => {
                eprintln!("horizon-agentd: board read: {error}");
                continue;
            }
        };
        let order = ordered_items(&items);
        if integrating.is_none() {
            for item in &order {
                let Some(flow) = &item.workflow else {
                    continue;
                };
                if flow.task.is_none()
                    || flow.integrated.is_some()
                    || flow.problem.is_some()
                    || stop_reason(&items, item.id).is_some()
                {
                    continue;
                }
                let Some(worker) = &flow.worker else {
                    continue;
                };
                let candidate = if let Some(candidate) = &flow.merging {
                    candidate.clone()
                } else {
                    let (Some(v), Some(candidate)) = (&flow.verification, &flow.integration) else {
                        continue;
                    };
                    if !v.decisions.is_empty() || !v.evidence.iter().all(|e| e.satisfied) {
                        continue;
                    }
                    if update(&store, item.id, Mutation::BeginIntegration)
                        .await
                        .is_err()
                    {
                        continue;
                    }
                    candidate.clone()
                };
                let (source, worker) = (root.clone(), worker.clone());
                integrating = Some(IntegrationJob {
                    id: item.id,
                    commit: candidate.head.clone(),
                    handle: tokio::task::spawn_blocking(move || {
                        git::integrate(&source, &worker, &candidate)
                    }),
                });
                break;
            }
        }
        for item in order {
            if monitors.contains_key(&item.id) || launching.contains_key(&item.id) {
                continue;
            }
            let Some(work) = eligible_work(&items, item.id) else {
                continue;
            };
            // Reserve in priority order; slow worktree/provider startup happens
            // concurrently after the writer rechecks board-wide eligibility.
            match launch::reserve(&store, item.id, work).await {
                Ok((reserved, session, token, work)) => {
                    let (state, root) = (state.clone(), root.clone());
                    launching.insert(
                        item.id,
                        tokio::spawn(async move {
                            launch::prepare(state, root, reserved, session, token, work).await
                        }),
                    );
                }
                Err(e) if e.contains("no longer eligible") => {}
                Err(e) => eprintln!("horizon-agentd: board reserve #{}: {e}", item.id),
            }
        }
    }
}
