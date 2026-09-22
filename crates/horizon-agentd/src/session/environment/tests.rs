use super::*;
use crate::session::connection::Connection;
use crate::session::state::SessionEntry;
use crossbeam_channel::unbounded;
use horizon_agent::persistence::event_log::{self, WriterHandle, WriterInit};

fn git(root: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn activation_publishes_matching_durable_context_and_rolls_back_if_persistence_fails() {
    for persistence_available in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "initial"]);
        let root = root.canonicalize().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (writer, ready) = WriterHandle::open(&log_path);
        assert!(matches!(ready.recv().unwrap(), WriterInit::Ready(_)));
        let state = crate::session::test_support::judge_test_state();
        state.set_writer(Some(writer.clone()));
        let config = lock_unpoisoned(&state.agent_config).clone();
        let session_id = SessionId::new();
        let provider_id = ProviderId("builtin.agent.mock".into());
        let environment = SessionEnvironment {
            state: &state,
            session_id,
            provider_id: &provider_id,
            role_id: None,
            agent_config: &config,
        };
        let PreparedEnvironment {
            mut tool_state,
            context,
        } = environment.prepare(
            EnvironmentLocation {
                workspace_root: Some(root.clone()),
                parent_session_id: None,
                isolated: false,
                trusted: false,
            },
            &[],
        );
        let live_state = if persistence_available {
            LiveState::with_event_log_context_and_history(
                session_id,
                Some(provider_id.clone()),
                None,
                writer.clone(),
                Some(context),
                Vec::new(),
            )
        } else {
            LiveState::with_disabled_persistence()
        };
        let (commands, responses) = unbounded();
        let (replay, _replay_rx) = unbounded();
        let (results, _results_rx) = unbounded();
        lock_unpoisoned(&state.sessions).insert(
            session_id,
            SessionEntry {
                provider_id: provider_id.clone(),
                role_id: None,
                model: None,
                selection: None,
                inbound: commands.clone(),
                replay,
                parent_session_id: None,
                workspace_root: Some(root.clone()),
                worktree: None,
            },
        );
        let mut outgoing = Connection::new(state.clone()).subscribe_agent(session_id);
        register_session_runtime(
            session_id,
            tool_state.clone(),
            live_state.clone(),
            results.clone(),
        );
        environment.activate("HEAD", &live_state, &mut tool_state, &results, &commands);
        let response = responses.try_recv().unwrap();
        // No extra flush: the provider acknowledgement must already imply durability.
        let records = event_log::read(&log_path).unwrap().records;
        if persistence_available {
            let Command::EnvironmentPrepared {
                workspace_root,
                trusted_project,
            } = response
            else {
                panic!("activation failed: {response:?}");
            };
            assert!(!trusted_project);
            assert_ne!(workspace_root, root);
            assert_eq!(tool_state.workspace_root(), Some(workspace_root.as_path()));
            let worktree = lock_unpoisoned(&state.sessions)[&session_id]
                .worktree
                .clone()
                .unwrap();
            assert_eq!(worktree.path, workspace_root);
            let record = records
                .iter()
                .find(|record| matches!(record.event, Event::EnvironmentActivated(_)))
                .expect("activation must be durable before its acknowledgement");
            assert_eq!(record.session_id, session_id);
            let context = record.session_context.as_ref().unwrap();
            assert_eq!(context.workspace_root.as_ref(), Some(&workspace_root));
            assert!(context.isolated_worktree);
            assert_eq!(context.parent_session_id, None);
            assert!(
                matches!(outgoing.try_recv().unwrap(), AgentWireEvent::Event(Event::EnvironmentActivated(identity))
                if identity == worktree.identity())
            );
            // A second activation must leave the current environment intact.
            environment.activate("HEAD", &live_state, &mut tool_state, &results, &commands);
            assert!(
                matches!(responses.try_recv().unwrap(), Command::EnvironmentActivationFailed { message }
                if message == "Session already owns a worktree")
            );
            assert_eq!(tool_state.workspace_root(), Some(workspace_root.as_path()));
        } else {
            assert!(
                matches!(response, Command::EnvironmentActivationFailed { message }
                if message == "Session persistence is unavailable")
            );
            assert_eq!(tool_state.workspace_root(), Some(root.as_path()));
            assert!(lock_unpoisoned(&state.sessions)[&session_id]
                .worktree
                .is_none());
            assert!(!root
                .join(".horizon/worktrees")
                .join(crate::worktree::short_slug(session_id.as_uuid()))
                .exists());
            assert!(records.is_empty());
            assert!(outgoing.try_recv().is_err());
        }
        assert!(responses.try_recv().is_err());
        horizon_agent::tools::unregister_session_runtime(session_id);
    }
}

#[test]
fn environment_rebuild_retains_additional_grants_without_granting_the_old_root() {
    let dirs = tempfile::tempdir().unwrap();
    let old_root = dirs.path().join("consultation");
    let new_root = dirs.path().join("implementation");
    let dependency = dirs.path().join("dependency");
    for root in [&old_root, &new_root, &dependency] {
        std::fs::create_dir(root).unwrap();
    }
    let grant = horizon_sandbox::FilesystemGrant {
        path: dependency.canonicalize().unwrap(),
        access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
        scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
        excluded_subpaths: vec![],
    };
    let state = super::super::test_support::judge_test_state();
    let config = lock_unpoisoned(&state.agent_config).clone();
    let PreparedEnvironment {
        tool_state: tools,
        context,
    } = SessionEnvironment {
        state: &state,
        session_id: SessionId::new(),
        provider_id: &ProviderId("builtin.agent.mock".into()),
        role_id: None,
        agent_config: &config,
    }
    .prepare(
        EnvironmentLocation {
            workspace_root: Some(new_root.clone()),
            parent_session_id: None,
            isolated: false,
            trusted: false,
        },
        &[grant.clone(), grant.clone()],
    );
    assert_eq!(tools.workspace_root(), Some(new_root.as_path()));
    assert_eq!(
        tools.retained_filesystem_grants(),
        std::slice::from_ref(&grant)
    );
    assert_eq!(context.filesystem_grants, [grant]);
    assert!(!context
        .filesystem_grants
        .iter()
        .any(|grant| grant.path == old_root));
}
