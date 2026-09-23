//! Launching and watching the proposer sessions of a Mixture-of-Agents
//! pass (`docs/agent-moa-design.md`).
//!
//! A proposer is a `task` session in every structural respect: the same
//! [`ExplorationHost`] seam, the same explore role (read-only allowlist, a
//! call that would need a human refused rather than parked, iteration cap
//! with summarize-on-cap), and the same
//! event fold that decides when its answer is final
//! (`super::explore::watch_until_terminal`). Three differences:
//!
//! - There is no launching tool call, so nothing ties a proposer to its
//!   requester through `ToolCallRequested`/`ToolCallFinished`. The pass
//!   emits `Event::MoaPassStarted` to record the relation instead.
//! - Each member runs on its own `{provider, model}`, carried by
//!   [`ExplorationRequest`].
//! - A completion is recorded with
//!   `children::complete_without_notification`: the proposer stays
//!   addressable by `task_output`, but nothing is queued for injection into
//!   the aggregator's `rig_history`.
//!
//! No `TaskProgress` is forwarded: the aggregator's pane shows its ordinary
//! in-progress state while proposers run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam_channel::Sender;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::config::MoaMember;
use crate::contract::SessionId;

use super::explore::{children, ExplorationHost, ExplorationRequest};

/// Session id to spawn capability. Process-global because the thread that
/// runs a pass (the rig session loop) is not the thread `ToolSessionState`
/// is confined to, so the `Rc`-based per-session state cannot carry it
/// across — the same reason `explore::children` is global.
fn hosts() -> &'static Mutex<HashMap<SessionId, Arc<dyn ExplorationHost>>> {
    static HOSTS: OnceLock<Mutex<HashMap<SessionId, Arc<dyn ExplorationHost>>>> = OnceLock::new();
    HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_hosts() -> std::sync::MutexGuard<'static, HashMap<SessionId, Arc<dyn ExplorationHost>>> {
    hosts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Publishes `session_id`'s spawn capability for the pass to reach. `None`
/// (a session that cannot spawn peers at all) removes any previous entry.
pub fn register_exploration_host(session_id: SessionId, host: Option<Arc<dyn ExplorationHost>>) {
    match host {
        Some(host) => {
            lock_hosts().insert(session_id, host);
        }
        None => {
            lock_hosts().remove(&session_id);
        }
    }
}

pub fn unregister_exploration_host(session_id: SessionId) {
    lock_hosts().remove(&session_id);
}

pub(crate) fn exploration_host(session_id: SessionId) -> Option<Arc<dyn ExplorationHost>> {
    lock_hosts().get(&session_id).cloned()
}

/// One launched proposer. `session_id` is what the durable record, the
/// cancellation path, and `task_output` all address it by.
#[derive(Clone, Debug)]
pub(crate) struct LaunchedProposer {
    pub(crate) session_id: SessionId,
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// What one proposer produced. `text` is `None` whenever the proposer
/// contributed nothing usable — a provider error, an empty report, a cap
/// with nothing to show, or a member that could not be started — and
/// `failure` then carries the reason for the log.
#[derive(Clone, Debug)]
pub(crate) struct Proposal {
    pub(crate) member: LaunchedProposer,
    pub(crate) text: Option<String>,
    pub(crate) failure: Option<String>,
}

/// One pass's launch: the sessions that started, the members that could not
/// start (already resolved as failed proposals), and the channel the
/// watchers report on.
pub(crate) struct MoaLaunch {
    pub(crate) launched: Vec<LaunchedProposer>,
    pub(crate) unavailable: Vec<Proposal>,
    pub(crate) results: UnboundedReceiver<Proposal>,
    /// Releases each watcher's fold without waiting for its session.
    cancels: Vec<Sender<()>>,
}

impl MoaLaunch {
    /// Terminates every launched proposer session and releases its watcher.
    /// Unlike a `task` child, a proposer does not outlive the turn that
    /// launched it.
    pub(crate) fn abort(&self) {
        for cancel in &self.cancels {
            let _ = cancel.try_send(());
        }
        for proposer in &self.launched {
            if let Some(host) = children::take_host(proposer.session_id) {
                host.terminate(proposer.session_id);
            }
        }
    }
}

/// Starts one proposer session per member and returns without waiting; the
/// caller consumes [`MoaLaunch::results`] until every launched proposer has
/// reported. A member whose session cannot be started becomes a failed
/// proposal rather than an error for the pass.
pub(crate) fn launch(
    aggregator: SessionId,
    host: Arc<dyn ExplorationHost>,
    members: &[MoaMember],
    prompt: &str,
) -> MoaLaunch {
    let (results_tx, results) = unbounded_channel();
    let mut launched = Vec::new();
    let mut unavailable = Vec::new();
    let mut cancels = Vec::new();

    for (position, member) in members.iter().enumerate() {
        // A session on a key-less entry answers from the deterministic
        // fallback responder, which the event fold cannot tell apart from a
        // model's answer. Never launched, so that text can never become a
        // proposal.
        if !member.api_key_present {
            unavailable.push(Proposal {
                member: LaunchedProposer {
                    session_id: SessionId::new(),
                    provider: member.provider.clone(),
                    model: member.model.clone(),
                },
                text: None,
                failure: Some(member.unavailable_reason()),
            });
            continue;
        }
        let request = ExplorationRequest::for_proposer(
            prompt.to_string(),
            member.provider.clone(),
            member.model.clone(),
        );
        let started = match host.start(request) {
            Ok(started) => started,
            Err(message) => {
                unavailable.push(Proposal {
                    member: LaunchedProposer {
                        session_id: SessionId::new(),
                        provider: member.provider.clone(),
                        model: member.model.clone(),
                    },
                    text: None,
                    failure: Some(message),
                });
                continue;
            }
        };
        let proposer = LaunchedProposer {
            session_id: started.session_id,
            provider: member.provider.clone(),
            model: member.model.clone(),
        };
        // The label a `task_output` re-read shows, numbered as the injected
        // proposal block numbers them.
        let description = format!("MoA proposer {}", position + 1);
        let (cancel_tx, cancel_rx) = crossbeam_channel::bounded::<()>(1);
        children::register(
            aggregator,
            proposer.session_id,
            description.clone(),
            host.clone(),
            cancel_tx.clone(),
        );
        cancels.push(cancel_tx);
        launched.push(proposer.clone());

        let events = started.events;
        let results_tx = results_tx.clone();
        std::thread::spawn(move || {
            let outcome = super::explore::watch_until_terminal(&events, &cancel_rx, &mut |_| {});
            // `take_host`'s take-once semantics is what keeps this and
            // `MoaLaunch::abort` from terminating the same session twice.
            if let Some(host) = children::take_host(proposer.session_id) {
                host.terminate(proposer.session_id);
            }
            let usable = outcome.has_usable_report();
            let text = usable.then(|| outcome.report.clone().unwrap_or_default());
            let output = outcome.into_output(proposer.session_id, &description);
            let failure = (!usable).then(|| {
                output
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the proposer produced no usable answer")
                    .to_string()
            });
            children::complete_without_notification(proposer.session_id, output);
            let _ = results_tx.send(Proposal {
                member: proposer,
                text,
                failure,
            });
        });
    }

    MoaLaunch {
        launched,
        unavailable,
        results,
        cancels,
    }
}
