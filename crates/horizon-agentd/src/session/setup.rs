//! Per-session construction, resolved once on the session's own thread
//! before its provider starts: the confinement root, the configured
//! `[grants]` trees, the skill-discovery root, and the isolated worktree.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use horizon_agent::config::AgentToolsConfig;
use horizon_agent::contract::{Error as AgentError, Event, SessionId};
use horizon_agent::tools::{RecallContext, ToolSessionState};
use horizon_agent::wire::{AgentWireEvent, WorkspaceRootResolved};

use super::events::send_session_event;
use super::state::AgentdState;
use crate::worktree;

/// Builds a session's file-tool confinement root (`tools::state::
/// ToolSessionState::workspace_root`): an explicit `workspace_root` --
/// carried by a fresh `wire::SessionNew`, when the caller supplied one --
/// takes precedence over `ToolSessionState::for_current_dir`'s default of
/// this process's own cwd. Resumed sessions also carry the validated root
/// recovered from their event-log context. Pulled out of
/// [`super::run::run_session`] as its own function purely so this Some/None dispatch is
/// unit-testable without spinning up a whole session thread.
pub(super) fn tool_session_state_for(
    workspace_root: Option<PathBuf>,
    tools: AgentToolsConfig,
    recall: RecallContext,
) -> ToolSessionState {
    match workspace_root {
        Some(root) => ToolSessionState::for_root(root, tools, recall),
        None => ToolSessionState::for_current_dir(tools, recall),
    }
}

/// The `[grants]` trees this session's project entitles it to, as sandbox
/// grants ready to inject (`docs/containment-denial-narrow-grants-design.md`'s
/// 2026-07-26 decision).
///
/// Two steps, both refusable: resolve `workspace_root` to its project (the
/// repository toplevel -- a derived worktree resolves to the repository it
/// came from, see `worktree::project_root`), then look that project up in
/// the user's config. A session with no workspace root, no repository, or
/// no matching `[[grants.project]]` entry gets nothing extra, which is
/// exactly the behavior that existed before this section did.
///
/// Every tree becomes a `ReadWrite` `DirectoryTree` grant -- the shape the
/// decision fixes -- and is revalidated here so a config entry naming a
/// directory that has since been deleted, replaced, or become over-broad
/// never reaches a sandbox policy.
pub(super) fn configured_filesystem_grants(
    state: &Arc<AgentdState>,
    workspace_root: Option<&Path>,
) -> Vec<horizon_sandbox::FilesystemGrant> {
    let Some(project_root) = workspace_root.and_then(worktree::project_root) else {
        return Vec::new();
    };
    grants_for_project(&state.project_grants, &project_root)
}

/// The `[grants]` `network` entries' direct-connect endpoints this
/// session's project entitles it to, resolved the same way
/// [`configured_filesystem_grants`] resolves trees: look up the session's
/// project root in the user's config and return every matching entry's
/// endpoints (the `network` entries that dispatched as an `ip:port` shape,
/// see `horizon_config::grants`' module doc). A session with no workspace
/// root, no repository, or no matching `[[grants.project]]` entry gets
/// nothing extra. These are threaded into the sandbox's
/// `NetworkPolicy::Proxied` so the seccomp-notify enforcement layer allows
/// direct connects to them alongside the session proxy (e.g. sccache on
/// `127.0.0.1:4226`).
pub(super) fn configured_loopback_connect(
    state: &Arc<AgentdState>,
    workspace_root: Option<&Path>,
) -> Vec<std::net::SocketAddr> {
    let Some(project_root) = workspace_root.and_then(worktree::project_root) else {
        return Vec::new();
    };
    horizon_config::grants::loopback_connect_for_project(&state.project_grants, &project_root)
}

/// The `[grants]` `network` entries' domain names this session's project
/// entitles it to -- the counterpart to [`configured_loopback_connect`],
/// resolved the same way, for the `network` entries that dispatched as a
/// domain name rather than an `ip:port` shape. Pre-seeded into this
/// session's `SessionDomainPolicy` at spawn
/// (`horizon_agent::tools::SessionDomainPolicy::with_allowed`) so a
/// project-trusted domain never needs a judge/approval round trip through
/// the session's network proxy; the runtime grant flow (approve-on-denial)
/// still applies on top for anything not listed here.
pub(super) fn configured_domains(
    state: &Arc<AgentdState>,
    workspace_root: Option<&Path>,
) -> Vec<String> {
    let Some(project_root) = workspace_root.and_then(worktree::project_root) else {
        return Vec::new();
    };
    horizon_config::grants::domains_for_project(&state.project_grants, &project_root)
}

/// The pure half of [`configured_filesystem_grants`], split out so the
/// config-entry-to-sandbox-grant mapping is testable without a session,
/// a daemon, or a config file.
fn grants_for_project(
    project_grants: &[horizon_config::ProjectGrant],
    project_root: &Path,
) -> Vec<horizon_sandbox::FilesystemGrant> {
    horizon_config::grants::trees_for_project(project_grants, project_root)
        .into_iter()
        .map(|path| horizon_sandbox::FilesystemGrant {
            path,
            access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
            scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
        })
        .filter(|grant| match horizon_sandbox::revalidate_grant(grant) {
            Ok(()) => true,
            Err(error) => {
                eprintln!(
                    "horizon-agentd: ignoring configured grant {} for project {}: {error}",
                    grant.path.display(),
                    project_root.display()
                );
                false
            }
        })
        .collect()
}

/// Returns the directory repository-local skills must be discovered from.
/// This deliberately mirrors `SessionEnvironment::for_workspace_root`: an
/// explicit, host-resolved session root wins, otherwise both the prompt and
/// `skill.read` fall back to the daemon's current directory (and finally
/// `/`). Keeping the fallback in a named helper makes the two production
/// skill registries' common root directly testable.
pub(super) fn skill_discovery_root(workspace_root: Option<&Path>) -> PathBuf {
    workspace_root
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Resolves and creates this session's isolated worktree (`docs/
/// session-relationship-design.md` decisions 2-3), returning the directory
/// its file tools should actually be confined to, plus whether isolation
/// actually succeeded -- the latter is what `ToolSessionState::
/// with_isolated_worktree` needs (`docs/agent-approval-design.md`'s tier 1:
/// the per-call trust predicate's isolation input must reflect the real
/// outcome, never merely the request). Runs on the session's own dedicated
/// thread, before `tool_session_state_for` -- a few tens of milliseconds of
/// blocking `git` subprocess calls at session-start time, the same shape
/// `state.wait_for_duckdb_store()` just above already accepts for this
/// thread. Degrades gracefully on any failure (no git repo found, no
/// commits yet, ...): falls back to `workspace_root` (today's
/// shared-directory behavior) and records no lineage edge, since isolation
/// didn't actually happen -- matching decision 2's "the edge exists only
/// via isolation" for the *actual* outcome, not merely the request. A
/// `contract::Event::Error` is also emitted so the failure is visible in
/// the session's own transcript rather than only agentd's stderr.
///
/// On success, also pushes a live `Control::WorkspaceRootResolved`
/// announcement (mirroring `resolve_and_announce_session_model`'s shape) so
/// a UI connected for this whole session's lifetime sees the authoritative
/// root/parent immediately, not just via a later resume/reload sweep -- see
/// that `Control` variant's own doc comment.
pub(super) fn resolve_and_create_isolated_worktree(
    state: &Arc<AgentdState>,
    session_id: SessionId,
    spawn_source_session_id: Option<SessionId>,
    workspace_root: Option<PathBuf>,
) -> (Option<PathBuf>, bool) {
    let fallback_dir = workspace_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let parent_info =
        spawn_source_session_id.and_then(|source_id| state.session_directory(source_id));
    let source_dir = worktree::resolve_isolation_source(parent_info, fallback_dir);

    match worktree::create_isolated_worktree(&source_dir, session_id.as_uuid()) {
        Ok(info) => {
            let root = info.path.clone();
            state.record_isolated_worktree(session_id, spawn_source_session_id, info);
            send_session_event(
                state,
                session_id,
                AgentWireEvent::WorkspaceRootResolved(WorkspaceRootResolved {
                    workspace_root: root.clone(),
                    parent_session_id: spawn_source_session_id,
                }),
            );
            (Some(root), true)
        }
        Err(error) => {
            eprintln!(
                "horizon-agentd: failed to create isolated worktree for {session_id:?}: {error}"
            );
            send_session_event(
                state,
                session_id,
                AgentWireEvent::Event(Event::Error(AgentError {
                    message: format!(
                        "failed to create an isolated worktree ({error}); continuing without \
                         isolation"
                    ),
                })),
            );
            (workspace_root, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::SessionEntry;
    use crate::session::test_support::state_with_rig_config;
    use crate::session::Connection;
    use crossbeam_channel::{unbounded, Sender};
    use horizon_agent::contract::{Command, ProviderId};

    /// A `SessionNew.workspace_root` of `Some(dir)` must confine the
    /// session's file tools to `dir`, not this process's cwd.
    #[test]
    fn tool_session_state_for_uses_the_given_directory_when_some() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .expect("canonicalize temp dir");
        let state = tool_session_state_for(
            Some(dir.clone()),
            AgentToolsConfig::default(),
            RecallContext::default(),
        );
        assert_eq!(state.workspace_root(), Some(dir.as_path()));
    }

    /// `None` (today's only value Horizon actually sends -- see
    /// `wire::SessionNew::workspace_root`'s doc comment) must keep behaving
    /// exactly as before this field existed: confined to this process's own
    /// cwd.
    #[test]
    fn tool_session_state_for_falls_back_to_the_process_cwd_when_none() {
        let expected_root = std::env::current_dir()
            .and_then(|dir| dir.canonicalize())
            .expect("canonicalize process cwd");
        let state =
            tool_session_state_for(None, AgentToolsConfig::default(), RecallContext::default());
        assert_eq!(state.workspace_root(), Some(expected_root.as_path()));
    }

    /// The tool-side skill registry must prefer the session's resolved root
    /// over the daemon cwd, matching the provider's prompt-side registry.
    #[test]
    fn skill_discovery_root_uses_the_session_workspace_when_present() {
        let session_root = PathBuf::from("/session-specific-workspace");
        assert_eq!(
            skill_discovery_root(Some(&session_root)),
            session_root,
            "skill.read must not discover from the daemon cwd"
        );
    }

    #[test]
    fn skill_discovery_root_falls_back_to_the_process_cwd() {
        let expected = std::env::current_dir().expect("read process cwd");
        assert_eq!(skill_discovery_root(None), expected);
    }

    // --- [grants]: project-scoped tree grants ---------------------------

    #[test]
    fn a_configured_tree_becomes_a_read_write_directory_grant_for_its_project() {
        let tree = tempfile::tempdir().expect("create temp tree");
        let canonical_tree = std::fs::canonicalize(tree.path()).unwrap();
        let (entries, warnings) = horizon_config::grants::resolve(
            &[horizon_config::RawProjectGrant {
                root: "/src/project".to_string(),
                trees: vec![canonical_tree.display().to_string()],
                network: Vec::new(),
            }],
            Some(std::path::Path::new("/home/someone")),
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");

        assert_eq!(
            grants_for_project(&entries, std::path::Path::new("/src/project")),
            vec![horizon_sandbox::FilesystemGrant {
                path: canonical_tree,
                access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
                scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            }]
        );
    }

    #[test]
    fn a_project_with_no_configured_entry_gets_no_grants() {
        let (entries, _) = horizon_config::grants::resolve(
            &[horizon_config::RawProjectGrant {
                root: "/src/project".to_string(),
                trees: vec!["/src/cache".to_string()],
                network: Vec::new(),
            }],
            None,
        );

        assert!(grants_for_project(&entries, std::path::Path::new("/src/elsewhere")).is_empty());
    }

    #[test]
    fn a_configured_tree_that_no_longer_exists_is_dropped_before_it_reaches_a_policy() {
        let tree = tempfile::tempdir().expect("create temp tree");
        let path = std::fs::canonicalize(tree.path()).unwrap();
        let (entries, _) = horizon_config::grants::resolve(
            &[horizon_config::RawProjectGrant {
                root: "/src/project".to_string(),
                trees: vec![path.display().to_string()],
                network: Vec::new(),
            }],
            None,
        );
        assert_eq!(
            grants_for_project(&entries, std::path::Path::new("/src/project")).len(),
            1
        );

        drop(tree);

        assert!(
            grants_for_project(&entries, std::path::Path::new("/src/project")).is_empty(),
            "a stale config entry must not become a sandbox grant"
        );
    }

    // --- [grants]: project-scoped `network` domain pre-seeding ----------
    //
    // `configured_domains` itself is a thin two-line wrapper over
    // `horizon_config::grants::domains_for_project` (root lookup, dedup --
    // already covered by that function's own tests), so what's worth
    // exercising here is the actual spawn-time effect: a domain configured
    // in `[[grants.project]]` `network` ends up allowed in the
    // `SessionDomainPolicy` `session::run::run_session` seeds with it.

    #[test]
    fn a_configured_domain_is_allowed_in_the_pre_seeded_session_domain_policy() {
        let (entries, warnings) = horizon_config::grants::resolve(
            &[horizon_config::RawProjectGrant {
                root: "/src/project".to_string(),
                trees: Vec::new(),
                network: vec!["build-cache.internal".to_string()],
            }],
            None,
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");

        let domains = horizon_config::grants::domains_for_project(
            &entries,
            std::path::Path::new("/src/project"),
        );
        let policy = horizon_agent::tools::SessionDomainPolicy::with_allowed(domains);

        assert!(policy.is_allowed("build-cache.internal"));
        assert!(!policy.is_allowed("other.example"));
    }

    // --- Live workspace-root announcement (real git, in temp repositories) --
    //
    // `resolve_and_create_isolated_worktree` shells out to real `git` via
    // `worktree::create_isolated_worktree`, so exercising its announcement
    // needs a real scratch repo -- mirrors `worktree.rs`'s own real-git test
    // convention, including its `EnclosingRepoGuard` hermeticity canary and
    // `GIT_*` env scrubbing (see that module's doc comment / backlog 53) to
    // guard against a leaked `GIT_DIR` operating on the enclosing repository
    // instead of the temp one.
    mod isolation_announcement {
        use std::path::{Path, PathBuf};

        use super::*;

        fn scrub_git_env(cmd: &mut std::process::Command) {
            for (key, _) in std::env::vars() {
                if key.starts_with("GIT_") {
                    cmd.env_remove(key);
                    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
                    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
                }
            }
        }

        fn git(dir: &Path, args: &[&str]) {
            let mut cmd = std::process::Command::new("git");
            cmd.arg("-C").arg(dir).args(args);
            scrub_git_env(&mut cmd);
            let status = cmd
                .status()
                .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
            assert!(status.success(), "git {args:?} failed in {}", dir.display());
        }

        fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
            let mut cmd = std::process::Command::new("git");
            cmd.arg("-C").arg(dir).args(args);
            scrub_git_env(&mut cmd);
            let output = cmd.output().map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned());
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }

        fn init_repo(dir: &Path) {
            git(dir, &["init", "-q", "-b", "main"]);
        }

        /// Commits with a deterministic `Test` identity supplied *per
        /// invocation* via `-c user.*` -- never written to any git config.
        /// Backlog 53: persisting it with `git config user.*` meant that
        /// under a leaked absolute `GIT_DIR` (see `scrub_git_env`) the write
        /// landed in the *enclosing* repo's shared config and re-authored
        /// every later commit; a `-c` override touches no config and still
        /// stamps both author and committer. Mirrors `worktree.rs`.
        fn commit_file(dir: &Path, name: &str, contents: &str, message: &str) {
            std::fs::write(dir.join(name), contents).unwrap();
            git(dir, &["add", name]);
            git(
                dir,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-q",
                    "-m",
                    message,
                ],
            );
        }

        fn scratch_repo() -> tempfile::TempDir {
            let dir = tempfile::tempdir().expect("create temp dir");
            init_repo(dir.path());
            commit_file(dir.path(), "README.md", "root\n", "root commit");
            dir
        }

        #[derive(Debug, PartialEq, Eq)]
        struct EnclosingRepoState {
            bare: String,
            status: String,
            horizon_branches: String,
            /// Backlog 53: catches a test's `Test <test@example.com>` identity
            /// leaking into the enclosing repo's shared config (a `git config
            /// user.*` write under a leaked `GIT_DIR`), which would silently
            /// re-author every later commit. Empty when unset; merged config
            /// (`--get`), so it reflects what commits would actually use.
            user_name: String,
            user_email: String,
        }

        fn enclosing_repo_root() -> PathBuf {
            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            let mut cmd = std::process::Command::new("git");
            cmd.arg("-C").arg(manifest_dir).args([
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
            ]);
            scrub_git_env(&mut cmd);
            let output = cmd
                .output()
                .expect("discovering the enclosing repo's root should never fail");
            assert!(
                output.status.success(),
                "git rev-parse --show-toplevel in {} failed: {}",
                manifest_dir.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        }

        fn enclosing_repo_state(root: &Path) -> EnclosingRepoState {
            EnclosingRepoState {
                bare: run_git(root, &["config", "--get", "core.bare"])
                    .unwrap_or_else(|_| "false".to_string()),
                status: run_git(root, &["status", "--porcelain"]).unwrap_or_default(),
                horizon_branches: run_git(
                    root,
                    &["for-each-ref", "--format=%(refname)", "refs/heads/horizon"],
                )
                .unwrap_or_default(),
                user_name: run_git(root, &["config", "--get", "user.name"]).unwrap_or_default(),
                user_email: run_git(root, &["config", "--get", "user.email"]).unwrap_or_default(),
            }
        }

        struct EnclosingRepoGuard {
            root: PathBuf,
            before: EnclosingRepoState,
        }

        impl EnclosingRepoGuard {
            fn capture() -> Self {
                let root = enclosing_repo_root();
                let before = enclosing_repo_state(&root);
                Self { root, before }
            }
        }

        impl Drop for EnclosingRepoGuard {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    return;
                }
                let after = enclosing_repo_state(&self.root);
                assert_eq!(
                    self.before,
                    after,
                    "hermeticity canary: the enclosing repository at {} changed during \
                     a worktree test -- a git invocation escaped its TempDir scratch repo \
                     (see worktree.rs's scrub_git_env doc / backlog 53)",
                    self.root.display()
                );
            }
        }

        fn entry_with_root(
            inbound: Sender<Command>,
            replay: Sender<Sender<Vec<Event>>>,
            root: PathBuf,
        ) -> SessionEntry {
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: None,
                inbound,
                replay,
                parent_session_id: None,
                workspace_root: Some(root),
                worktree: None,
            }
        }

        /// The regression guard this whole announcement exists for: a
        /// successful isolated-worktree resolution must push a session-scoped
        /// `Control::WorkspaceRootResolved` carrying the *same* root/parent
        /// [`AgentdState::record_isolated_worktree`] just recorded on the
        /// entry -- so a UI connected for this session's whole lifetime sees
        /// the authoritative worktree path live, without waiting for a
        /// `session_list`/resume sweep.
        #[test]
        fn resolve_and_create_isolated_worktree_announces_the_resolved_root_live() {
            let _canary = EnclosingRepoGuard::capture();
            let state = state_with_rig_config(true, "test-model");
            let repo = scratch_repo();
            let session_id = SessionId::new();
            let mut outgoing_rx = Connection::new(state.clone()).subscribe_agent(session_id);
            let parent_id = SessionId::new();
            let (inbound_tx, _inbound_rx) = unbounded::<Command>();
            let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
            state.sessions.lock().unwrap().insert(
                session_id,
                entry_with_root(inbound_tx, replay_tx, repo.path().to_path_buf()),
            );

            let resolved = resolve_and_create_isolated_worktree(
                &state,
                session_id,
                Some(parent_id),
                Some(repo.path().to_path_buf()),
            );

            let (resolved, isolation_resolved) = resolved;
            assert!(isolation_resolved, "success path must report resolved=true");
            let root = resolved.expect("isolation against a real git repo should succeed");
            assert!(
                root.starts_with(repo.path().join(".horizon").join("worktrees")),
                "resolved root {} should be under .horizon/worktrees",
                root.display()
            );

            let sent = outgoing_rx
                .try_recv()
                .expect("a WorkspaceRootResolved event should have been sent");
            match sent {
                AgentWireEvent::WorkspaceRootResolved(payload) => {
                    assert_eq!(payload.workspace_root, root);
                    assert_eq!(payload.parent_session_id, Some(parent_id));
                }
                other => panic!("expected a WorkspaceRootResolved event, got: {other:?}"),
            }
        }

        /// The failure path degrades to a shared spawn and records no
        /// lineage edge (existing behavior) -- nothing to announce either,
        /// mirroring `Control::SkippedLines`'s "just don't send it"
        /// convention. A non-existent source directory is not a git repo at
        /// all, so worktree creation fails deterministically without needing
        /// a real corrupt-repo fixture.
        #[test]
        fn resolve_and_create_isolated_worktree_sends_nothing_when_isolation_fails() {
            let _canary = EnclosingRepoGuard::capture();
            let state = state_with_rig_config(true, "test-model");
            let not_a_repo = tempfile::tempdir().expect("create temp dir");
            let session_id = SessionId::new();
            let mut outgoing_rx = Connection::new(state.clone()).subscribe_agent(session_id);
            let (inbound_tx, _inbound_rx) = unbounded::<Command>();
            let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
            state.sessions.lock().unwrap().insert(
                session_id,
                entry_with_root(inbound_tx, replay_tx, not_a_repo.path().to_path_buf()),
            );

            let resolved = resolve_and_create_isolated_worktree(
                &state,
                session_id,
                None,
                Some(not_a_repo.path().to_path_buf()),
            );

            assert_eq!(
                resolved,
                (Some(not_a_repo.path().to_path_buf()), false),
                "a failed isolation must fall back to the plain workspace_root, resolved=false"
            );
            // An `Event::Error` is still sent (see the function's own doc
            // comment) -- drain it before asserting nothing else follows.
            let _ = outgoing_rx.try_recv();
            assert!(
                outgoing_rx.try_recv().is_err(),
                "nothing should be announced when isolation never actually happened"
            );
        }
    }
}
