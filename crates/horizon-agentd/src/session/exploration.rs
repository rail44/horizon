//! `horizon-agent`'s `task` seam, implemented against this
//! daemon's own session hosting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use horizon_agent::contract::{
    ApprovalRequest, Command, OccurrenceId, ProviderId, SessionId, TaskProgress,
};
use horizon_agent::hosting::AgentWireEvent;

use super::events::send_session_event;
use super::spawn::spawn_session_thread;
use super::state::{AgentdState, lock_unpoisoned};

/// `horizon-agent`'s `task` seam (`docs/agent-explore-design.md`),
/// implemented against this daemon's own session hosting: spawn a peer
/// session, subscribe to its events, terminate it. One is built per
/// requesting session in [`super::run::run_session`] and installed on its
/// `ToolSessionState`, carrying that session's provider and resolved
/// workspace root so an exploration always runs where its requester does.
///
/// **Peer, not child** (decision 2): the exploration is spawned with no
/// spawn source in the requester's resolved root, so it shares the
/// requester's exact working tree -- including a worktree the requester
/// entered, whose uncommitted state is precisely the view mid-task
/// exploration needs -- and records no derivation edge.
pub(super) struct AgentdExplorationHost {
    pub(super) state: Arc<AgentdState>,
    /// The session this host was installed on -- every task it launches is
    /// reported back to *this* session's attached client.
    pub(super) requester_id: SessionId,
    /// The requesting session's provider, so an exploration is answered by
    /// the same model family the requester is talking to.
    pub(super) provider_id: ProviderId,
    /// The requesting session's own resolved root -- a requester that entered
    /// a worktree has its exploration read that worktree and not the
    /// daemon's cwd.
    pub(super) workspace_root: Option<PathBuf>,
    /// Whether the requesting session is one nobody watches: its children's
    /// approvals then have nobody to be shown to.
    pub(super) requester_unattended: bool,
    /// Every child approval shown through the requester and not yet
    /// settled, by child -- withdrawn when the child ends.
    pub(super) forwarded: Mutex<HashMap<SessionId, Vec<OccurrenceId>>>,
}

impl horizon_agent::tools::ExplorationHost for AgentdExplorationHost {
    fn start(
        &self,
        start: horizon_agent::tools::TaskStart,
    ) -> Result<horizon_agent::tools::StartedExploration, String> {
        let session_id = SessionId::new();
        let horizon_agent::tools::TaskStart {
            prompt,
            access,
            filesystem_grants,
            domains,
        } = start;
        // A write child works in a worktree made for it from the
        // requester's `HEAD`, and records the requester as its parent
        // (`docs/agent-subagent-design.md` decisions 3 and 11); a read child
        // shares the requester's own root as a peer.
        let (role, workspace_root, parent, worktree) = match access {
            horizon_agent::tools::TaskAccess::Read => (
                horizon_agent::roles::EXPLORE_ROLE_ID,
                self.workspace_root.clone(),
                None,
                None,
            ),
            horizon_agent::tools::TaskAccess::Write => {
                let root = self
                    .workspace_root
                    .as_deref()
                    .ok_or("a write task needs a workspace in a git repository")?;
                if crate::worktree::has_uncommitted_changes(root)? {
                    return Err("your working tree has changes that are not committed; a \
                         write task starts from your HEAD, so commit them first"
                        .to_string());
                }
                let info = crate::worktree::create_isolated_worktree_at(
                    root,
                    session_id.as_uuid(),
                    "HEAD",
                )?;
                (
                    horizon_agent::roles::WRITE_TASK_ROLE_ID,
                    Some(info.path.clone()),
                    Some(self.requester_id),
                    Some(info),
                )
            }
        };
        let branch = worktree.as_ref().map(|info| info.branch.clone());
        lock_unpoisoned(&self.state.task_seeds).insert(
            session_id,
            crate::session::state::TaskSeed {
                filesystem_grants,
                domains,
            },
        );
        // Subscribe first, spawn second -- the ordering requirement
        // `super::subscription` documents: the subscription has to exist
        // before the session's thread can emit anything.
        let subscription = self.state.subscribe_to_session(session_id);
        spawn_session_thread(
            self.state.clone(),
            session_id,
            self.provider_id.clone(),
            Some(horizon_agent::roles::RoleId(role.to_string())),
            workspace_root,
            parent,
            worktree,
            Vec::new(),
        );
        if !self
            .state
            .send_command(session_id, Command::UserMessage { text: prompt })
        {
            self.state.unsubscribe_from_session(session_id);
            return Err("the task session ended before it could be asked".to_string());
        }
        Ok(horizon_agent::tools::StartedExploration {
            session_id: subscription.session_id,
            events: subscription.events,
            branch,
        })
    }

    fn send(&self, session_id: SessionId, prompt: String) -> Result<(), String> {
        // The child's session loop queues a message that arrives mid-turn and
        // takes it once that turn ends, so a follow-up never interrupts.
        if self
            .state
            .send_command(session_id, Command::UserMessage { text: prompt })
        {
            Ok(())
        } else {
            Err("the task session has ended".to_string())
        }
    }

    fn forward_approval(&self, child: SessionId, request: ApprovalRequest) -> bool {
        if self.requester_unattended {
            return false;
        }
        lock_unpoisoned(&self.forwarded)
            .entry(child)
            .or_default()
            .push(request.occurrence_id.clone());
        send_session_event(
            &self.state,
            self.requester_id,
            AgentWireEvent::TaskApproval {
                task_session_id: child,
                request,
            },
        );
        true
    }

    fn settle_approval(&self, child: SessionId, occurrence: OccurrenceId) {
        let known = lock_unpoisoned(&self.forwarded)
            .get_mut(&child)
            .and_then(|pending| {
                let index = pending.iter().position(|held| *held == occurrence)?;
                Some(pending.remove(index))
            })
            .is_some();
        if known {
            self.settle(child, occurrence);
        }
    }

    fn terminate(&self, session_id: SessionId) {
        let pending = lock_unpoisoned(&self.forwarded)
            .remove(&session_id)
            .unwrap_or_default();
        for occurrence in pending {
            self.settle(session_id, occurrence);
        }
        self.state.unsubscribe_from_session(session_id);
        self.state.send_command(session_id, Command::Shutdown);
    }

    fn wait_stopped(&self, session_id: SessionId, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if !lock_unpoisoned(&self.state.sessions).contains_key(&session_id)
                && horizon_agent::tools::drain_session_work(session_id, std::time::Duration::ZERO)
            {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn forward_progress(&self, _child: SessionId, progress: TaskProgress) {
        // Ephemeral by design: silently dropped when no client is attached
        // right now, and never persisted (see `AgentWireEvent::TaskProgress`).
        // The payload already carries the child's id.
        send_session_event(
            &self.state,
            self.requester_id,
            AgentWireEvent::TaskProgress(progress),
        );
    }
}

impl AgentdExplorationHost {
    fn settle(&self, child: SessionId, occurrence_id: OccurrenceId) {
        send_session_event(
            &self.state,
            self.requester_id,
            AgentWireEvent::TaskApprovalSettled {
                task_session_id: child,
                occurrence_id,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::host_tools::AgentdHostTools;
    use crate::session::test_support::test_state;
    use crossbeam_channel::unbounded;
    use horizon_agent::config::AgentToolsConfig;
    use horizon_agent::contract::{Event, ToolCallId, ToolCallRequest};
    use horizon_agent::live::LiveState;
    use horizon_agent::tools::{
        Execution, RecallContext, ToolCompletion, ToolSessionState, execute_agent_tool,
        register_session_runtime, unregister_session_runtime,
    };
    use std::time::Duration;

    /// A real explore-role session whose read leaves the workspace root.
    /// The read asks for an approval, which the child's requester shows to
    /// its human (`docs/agent-subagent-design.md` decision 4); denied, the
    /// read resolves as an error result the model can read, the turn
    /// carries on and the session still delivers a report.
    ///
    /// End to end on the deterministic fallback provider (no API key, so no
    /// network): the `fs_read path:` line drives a genuine `fs_read`
    /// through the tool pipeline, and the rig turn loop folds whatever
    /// result comes back.
    #[test]
    fn a_childs_out_of_root_read_asks_and_once_denied_the_session_still_reports() {
        let workspace = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "horizon-unattended-explore-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let outside = workspace.parent().unwrap().join(format!(
            "horizon-unattended-outside-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("elsewhere.txt");
        std::fs::write(&outside_file, "not yours\n").unwrap();

        let state = crate::session::test_support::state_with_rig_config(false, "m-fallback");
        let host = AgentdExplorationHost {
            state: state.clone(),
            requester_id: SessionId::new(),
            provider_id: lock_unpoisoned(&state.providers).default_provider_id(),
            workspace_root: Some(workspace.clone()),
            requester_unattended: false,
            forwarded: Default::default(),
        };
        let started = horizon_agent::tools::ExplorationHost::start(
            &host,
            horizon_agent::tools::TaskStart {
                prompt: format!("fs_read path: {}", outside_file.display()),
                access: horizon_agent::tools::TaskAccess::Read,
                filesystem_grants: Vec::new(),
                domains: Vec::new(),
            },
        )
        .expect("the explore session starts");

        let mut collected = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let Ok(event) = started.events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            let done = matches!(event, Event::TurnEnded(_));
            // What the human answering through the requester sends back.
            if let Event::ApprovalRequested(request) = &event {
                assert!(state.send_command(
                    started.session_id,
                    Command::DenyToolCall {
                        identity: request.identity(),
                        reason: None,
                    },
                ));
            }
            collected.push(event);
            if done {
                break;
            }
        }

        assert!(
            collected
                .iter()
                .any(|event| matches!(event, Event::ToolCallRequested(request) if request.tool_id == "fs_read")),
            "the session must have attempted the read: {collected:?}"
        );
        assert!(
            collected
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_))),
            "the read must ask rather than be refused unseen: {collected:?}"
        );
        let refusal = collected
            .iter()
            .find_map(|event| match event {
                Event::ToolCallFinished(result) => Some(result.clone()),
                _ => None,
            })
            .expect("the read resolves with a result of its own");
        assert!(refusal.is_error() || refusal.is_denied(), "{refusal:?}");
        assert!(
            collected
                .iter()
                .any(|event| matches!(event, Event::TurnEnded(reason) if *reason == horizon_agent::contract::TurnEndReason::Completed)),
            "the turn must finish rather than park: {collected:?}"
        );
        let report = collected
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::MessageCommitted(message)
                    if message.role == horizon_agent::contract::MessageRole::Assistant =>
                {
                    Some(message.text.clone())
                }
                _ => None,
            })
            .expect("the session still delivers a final message");
        assert!(!report.trim().is_empty(), "{report}");

        horizon_agent::tools::ExplorationHost::terminate(&host, started.session_id);
        std::fs::remove_dir_all(workspace).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    fn call(
        state: &Arc<crate::session::AgentdState>,
        tool_state: &ToolSessionState,
        requester_id: SessionId,
        call_id: &str,
        tool_id: &str,
        input: serde_json::Value,
    ) -> serde_json::Value {
        let execution = execute_agent_tool(
            &AgentdHostTools {
                state: state.clone(),
            },
            tool_state,
            requester_id,
            &LiveState::with_disabled_persistence(),
            &ToolCallRequest {
                call_id: ToolCallId(call_id.to_string()),
                tool_id: tool_id.to_string(),
                input: input.into(),
                occurrence_id: horizon_agent::contract::OccurrenceId(
                    (ToolCallId(call_id.to_string())).0.clone(),
                ),
            },
        );
        let Ok(Execution::Applied(horizon_agent::tools::ToolUpdate::Finished { events, .. })) =
            execution
        else {
            panic!("`{tool_id}` resolves synchronously, got {execution:?}")
        };
        events
            .into_iter()
            .find_map(|event| match event {
                Event::ToolCallFinished(result) => Some(result.output.0),
                _ => None,
            })
            .expect("a ToolCallFinished event")
    }

    /// The whole `task` seam against the *real* daemon implementation
    /// rather than a stub: a `task` call spawns a genuine peer session
    /// here, its user message reaches that session's provider, its events
    /// come back through the [`crate::session::subscription`] seam, and the
    /// report is fetchable with `task_output`. The child stays open after
    /// its turn, so a follow-up `task` with its session id is answered by
    /// the same session (`docs/agent-subagent-design.md` decision 5); it is
    /// shut down, and its subscription released, when its requester goes
    /// away.
    ///
    /// The launch itself is asynchronous since 2026-07-28
    /// (`docs/agent-async-task-design.md`): the call returns a `started`
    /// receipt at once and nothing lands on `async_results` at all.
    /// Hermetic -- the child runs on the mock provider, so no network and
    /// no event log are involved.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(dir).args(args);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
        command.env("GIT_CONFIG_SYSTEM", "/dev/null");
        assert!(command.status().unwrap().success(), "git {args:?}");
    }

    /// `docs/agent-subagent-design.md` decisions 1, 3 and 11 against the
    /// real daemon: a write task is refused while the requester has changes
    /// not committed; once committed, the child runs in a worktree made for
    /// it from the requester's `HEAD`, on its own branch, records the
    /// requester as its parent, and its report names the branch.
    #[test]
    fn a_write_task_works_on_a_branch_of_its_own_from_the_requesters_head() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "root\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "root",
            ],
        );
        let head = {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "HEAD"])
                .env_remove("GIT_DIR")
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };

        let state = test_state();
        let requester_id = SessionId::new();
        let (results_tx, _results) = unbounded::<ToolCompletion>();
        let host: Arc<dyn horizon_agent::tools::ExplorationHost> =
            Arc::new(AgentdExplorationHost {
                state: state.clone(),
                requester_id,
                provider_id: ProviderId("builtin.agent.mock".to_string()),
                workspace_root: Some(root.clone()),
                requester_unattended: false,
                forwarded: Default::default(),
            });
        let tool_state = horizon_agent::tools::ToolSessionBuilder::for_current_dir(
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
        .with_exploration_host(Some(host))
        .build();
        register_session_runtime(
            requester_id,
            tool_state.clone(),
            LiveState::with_disabled_persistence(),
            results_tx,
        );
        let launch = |call_id: &str| {
            call(
                &state,
                &tool_state,
                requester_id,
                call_id,
                "task",
                serde_json::json!({
                    "description": "change the readme",
                    "prompt": "change the readme",
                    "access": "write",
                }),
            )
        };

        std::fs::write(root.join("README.md"), "uncommitted\n").unwrap();
        let refused = launch("write-dirty");
        assert_eq!(refused["is_error"], serde_json::json!(true), "{refused}");
        assert!(
            refused["message"]
                .as_str()
                .is_some_and(|message| message.contains("commit them first")),
            "{refused}"
        );
        git(&root, &["checkout", "--", "README.md"]);

        let launched = launch("write-clean");
        assert_eq!(
            launched["status"],
            serde_json::json!("started"),
            "{launched}"
        );
        let child_id =
            SessionId::from_uuid(launched["session_id"].as_str().unwrap().parse().unwrap());
        let slug = &child_id.as_uuid().to_string()[..8];
        let branch = format!("horizon/{slug}/work");
        {
            let sessions = state.sessions.lock().unwrap();
            let entry = sessions.get(&child_id).expect("the child runs");
            assert_eq!(
                entry.workspace_root.as_deref(),
                Some(root.join(".horizon/worktrees").join(slug).as_path())
            );
            assert_eq!(entry.parent_session_id, Some(requester_id));
            assert_eq!(
                entry
                    .worktree
                    .as_ref()
                    .map(|info| (info.branch.clone(), info.base.clone())),
                Some((branch.clone(), head.clone()))
            );
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let fetched = loop {
            let fetched = call(
                &state,
                &tool_state,
                requester_id,
                "write-output",
                "task_output",
                serde_json::json!({ "session_id": child_id.as_uuid().to_string() }),
            );
            if fetched["status"] == serde_json::json!("finished") {
                break fetched;
            }
            assert!(std::time::Instant::now() < deadline, "{fetched}");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(fetched["branch"], serde_json::json!(branch), "{fetched}");

        unregister_session_runtime(requester_id);
    }

    #[test]
    fn task_child_takes_a_follow_up_and_ends_with_its_requester() {
        let state = test_state();
        let requester_id = SessionId::new();
        let (results_tx, results_rx) = unbounded::<ToolCompletion>();
        let host: Arc<dyn horizon_agent::tools::ExplorationHost> =
            Arc::new(AgentdExplorationHost {
                state: state.clone(),
                requester_id,
                provider_id: ProviderId("builtin.agent.mock".to_string()),
                workspace_root: None,
                requester_unattended: false,
                forwarded: Default::default(),
            });
        let tool_state = horizon_agent::tools::ToolSessionBuilder::for_current_dir(
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
        .with_exploration_host(Some(host))
        .build();
        let live_state = LiveState::with_disabled_persistence();
        register_session_runtime(
            requester_id,
            tool_state.clone(),
            live_state.clone(),
            results_tx,
        );

        let launched = call(
            &state,
            &tool_state,
            requester_id,
            "task-e2e",
            "task",
            serde_json::json!({
                "description": "find the emit site",
                "prompt": "where is the emit site?",
            }),
        );
        assert_eq!(
            launched["status"],
            serde_json::json!("started"),
            "the launch must not block the requester's turn: {launched}"
        );
        let child_id = SessionId::from_uuid(
            launched["session_id"]
                .as_str()
                .expect("the spawned session id")
                .parse()
                .expect("a uuid"),
        );
        assert_ne!(child_id, requester_id);
        assert!(
            results_rx.try_recv().is_err(),
            "an asynchronous launch delivers nothing on the tool-completion channel"
        );

        let report_of = |call_id: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                let fetched = call(
                    &state,
                    &tool_state,
                    requester_id,
                    call_id,
                    "task_output",
                    serde_json::json!({ "session_id": child_id.as_uuid().to_string() }),
                );
                if fetched["status"] == serde_json::json!("finished") {
                    return fetched["report"].as_str().expect("a report").to_string();
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the task did not finish: {fetched}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let first = report_of("task-output-e2e");
        assert!(
            first.contains("where is the emit site?"),
            "the task must have answered the forwarded prompt, got: {first}"
        );
        assert!(
            state.sessions.lock().unwrap().contains_key(&child_id),
            "a child that only finished a turn stays open for a follow-up"
        );

        let followed = call(
            &state,
            &tool_state,
            requester_id,
            "task-follow-up-e2e",
            "task",
            serde_json::json!({
                "description": "and its caller",
                "prompt": "who calls it?",
                "session_id": child_id.as_uuid().to_string(),
            }),
        );
        assert_eq!(
            followed["status"],
            serde_json::json!("started"),
            "{followed}"
        );
        assert_eq!(
            followed["session_id"],
            serde_json::json!(child_id.as_uuid().to_string()),
            "a follow-up goes to the same session"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let second = loop {
            let report = report_of("task-output-follow-up-e2e");
            if report.contains("who calls it?") {
                break report;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the follow-up was not answered: {report}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(second.contains("who calls it?"), "{second}");

        unregister_session_runtime(requester_id);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while state.sessions.lock().unwrap().contains_key(&child_id)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !state.sessions.lock().unwrap().contains_key(&child_id),
            "the child is shut down with its requester"
        );
        assert!(
            !state.has_subscriber(child_id),
            "its event subscription must be released with it"
        );
    }
    #[test]
    fn child_retirement_can_reenter_the_session_table_after_its_parent_entry_is_gone() {
        use horizon_agent::tools::{ExplorationHost, StartedExploration};
        use std::sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        };
        struct ChildHost {
            state: Arc<AgentdState>,
            events: Mutex<Option<crossbeam_channel::Sender<Event>>>,
            lock_was_free: AtomicBool,
        }
        impl ExplorationHost for ChildHost {
            fn start(
                &self,
                _: horizon_agent::tools::TaskStart,
            ) -> Result<StartedExploration, String> {
                let (tx, events) = unbounded();
                *self.events.lock().unwrap() = Some(tx);
                Ok(StartedExploration {
                    session_id: SessionId::new(),
                    events,
                    branch: None,
                })
            }
            fn send(&self, _: SessionId, _: String) -> Result<(), String> {
                unreachable!()
            }
            fn terminate(&self, _: SessionId) {
                // A real daemon host reacquires this lock in send_command.
                // Observe it without hanging the regression on the old code.
                self.lock_was_free
                    .store(self.state.sessions.try_lock().is_ok(), Ordering::SeqCst);
            }
        }
        let state = test_state();
        let parent = SessionId::new();
        let child_host = Arc::new(ChildHost {
            state: state.clone(),
            events: Mutex::new(None),
            lock_was_free: AtomicBool::new(false),
        });
        let tools = horizon_agent::tools::ToolSessionBuilder::for_current_dir(
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
        .with_exploration_host(Some(child_host.clone()))
        .build();
        let (tx, _) = unbounded();
        register_session_runtime(
            parent,
            tools.clone(),
            LiveState::with_disabled_persistence(),
            tx,
        );
        let launched = call(
            &state,
            &tools,
            parent,
            "reentrant-child",
            "task",
            serde_json::json!({ "description": "child", "prompt": "wait" }),
        );
        assert_eq!(launched["status"], "started");
        let host = AgentdExplorationHost {
            state: state.clone(),
            requester_id: SessionId::new(),
            provider_id: ProviderId("builtin.agent.mock".into()),
            workspace_root: None,
            requester_unattended: false,
            forwarded: Default::default(),
        };
        assert!(host.wait_stopped(parent, Duration::from_secs(2)));
        assert!(child_host.lock_was_free.load(Ordering::SeqCst));
        unregister_session_runtime(parent);
    }
}
