use super::{git, monitor::Monitor, roles, session_id, update};
use crate::session::{spawn_session_thread, AgentdState, SessionSubscription};
use crate::worktree;
use horizon_agent::contract::{Command, SessionId};
use horizon_agent::roles::RoleId;
use horizon_board::workflow::{Mutation, Work, Worker};
use horizon_board::{Item, Store};
use std::path::PathBuf;
use std::sync::Arc;

pub(super) async fn reserve(
    store: &Store,
    id: u64,
    work: Work,
) -> Result<(Item, SessionId, String, Work), String> {
    let item = store
        .show(id)
        .map_err(|e| e.to_string())?
        .ok_or("Item disappeared")?;
    let session = if matches!(work, Work::Task { .. }) {
        item.workflow
            .as_ref()
            .and_then(|w| w.worker.as_ref())
            .map(|w| session_id(&w.session))
            .transpose()?
            .unwrap_or_default()
    } else if work == Work::Verify {
        item.workflow
            .as_ref()
            .and_then(|w| w.verifier.as_ref())
            .map(|w| session_id(&w.session))
            .transpose()?
            .unwrap_or_default()
    } else {
        SessionId::new()
    };
    let token = uuid::Uuid::new_v4().to_string();
    let item = update(
        store,
        id,
        Mutation::Start {
            token: token.clone(),
            session: session.as_uuid().to_string(),
            work: work.clone(),
        },
    )
    .await?;
    Ok((item, session, token, work))
}

pub(super) async fn prepare(
    state: Arc<AgentdState>,
    root: PathBuf,
    item: Item,
    session: SessionId,
    token: String,
    work: Work,
) -> Result<Monitor, String> {
    let store = Store::from_dir(&root).map_err(|e| e.to_string())?;
    match prepare_and_send(&state, &store, &root, &item, session, &token, &work).await {
        Ok(subscription) => Ok(Monitor::new(item.id, token, subscription)),
        Err(reason) => {
            state.unsubscribe_from_session(session);
            update(
                &store,
                item.id,
                Mutation::Interrupt {
                    token,
                    reason: reason.clone(),
                },
            )
            .await?;
            if work == Work::Verify
                && item.workflow.as_ref().is_some_and(|w| w.task.is_some())
                && reason.contains("git merge")
            {
                update(
                    &store,
                    item.id,
                    Mutation::Repair {
                        reason: reason.clone(),
                    },
                )
                .await?;
            }
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
) -> Result<SessionSubscription, String> {
    let flow = item.workflow.as_ref().ok_or("Workflow disappeared")?;
    let implements = matches!(work, Work::Task { .. });
    let verifies = work == &Work::Verify;
    let mut directory = root.to_path_buf();
    let mut restored = None;
    if implements || verifies {
        let existing = if implements {
            &flow.worker
        } else {
            &flow.verifier
        };
        let (source, prepared) = if verifies {
            if let Some(worker) = &flow.worker {
                let (root, worker) = (root.to_path_buf(), worker.clone());
                let source = PathBuf::from(&worker.worktree);
                let candidate = tokio::task::spawn_blocking(move || git::prepare(&root, &worker))
                    .await
                    .map_err(|e| e.to_string())??;
                (source, Some(candidate))
            } else {
                let source = git::main_checkout(root)?;
                let head = git::run(&source, &["rev-parse", "HEAD"])?;
                (
                    source,
                    Some(horizon_board::workflow::Integration {
                        base: head.clone(),
                        head,
                    }),
                )
            }
        } else {
            (git::main_checkout(root)?, None)
        };
        let owned = if let Some(worker) = existing {
            worker.clone()
        } else {
            let info = tokio::task::spawn_blocking(move || {
                worktree::create_isolated_worktree(&source, session.as_uuid())
            })
            .await
            .map_err(|e| e.to_string())??;
            let worker = Worker {
                session: session.as_uuid().to_string(),
                worktree: info.path.to_string_lossy().into_owned(),
                branch: info.branch.clone(),
            };
            let mutation = if implements {
                Mutation::SetWorker {
                    worker: worker.clone(),
                }
            } else {
                Mutation::SetVerifier {
                    worker: worker.clone(),
                }
            };
            update(store, item.id, mutation).await?;
            restored = Some(info);
            worker
        };
        directory = PathBuf::from(&owned.worktree);
        if restored.is_none() && state.session_directory(session) != Some((directory.clone(), true))
        {
            return Err("Session no longer owns its preserved worktree".into());
        }
        if let Some(candidate) = prepared {
            git::clean_head(&owned)?;
            git::run(
                &directory,
                &["merge", "--ff-only", "--no-edit", &candidate.head],
            )?;
            if git::clean_head(&owned)? != candidate.head {
                return Err("Verification checkout does not match the prepared commit".into());
            }
            update(
                store,
                item.id,
                Mutation::PrepareVerification {
                    base: candidate.base,
                    head: candidate.head,
                },
            )
            .await?;
        }
    }
    let subscription = state.subscribe_to_session(session);
    if !state.session_exists(session) {
        let provider = state.providers.lock().unwrap().default_provider_id();
        let role = if implements {
            roles::WORKER
        } else if verifies {
            roles::VERIFIER
        } else {
            roles::PLANNER
        };
        spawn_session_thread(
            state.clone(),
            session,
            provider,
            Some(RoleId(role.into())),
            Some(directory),
            None,
            false,
            restored,
            Vec::new(),
        );
    }
    let items = super::read_items(store)?;
    if let Some(reason) = super::stop_reason(&items, item.id) {
        return Err(reason);
    }
    let assignment = serde_json::json!({"item":item.id,"milestone":item.parent.unwrap_or(item.id),"title":item.title,"goal":item.body,"attempt":token,"work":work});
    let prompt = format!("Milestone assignment: {assignment}\nRead board.read id={} and the parent milestone, then the full board for dependencies, owner decisions and active change scopes. Perform only the assigned work and save board.report with id={} and attempt={token}. Inspect preserved work and failure history when retrying.",item.id,item.id);
    if !state.send_command(session, Command::UserMessage { text: prompt }) {
        return Err("Session exited before accepting the assignment".into());
    }
    Ok(subscription)
}
