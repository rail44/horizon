//! Resolves the cwd a newly spawned terminal should start in --
//! `docs/session-relationship-design.md` decision 3's "workspace_root
//! source": default to inheriting the spawn-source pane's cwd ("start
//! where I'm looking"), kind-agnostically per the source's kind, falling
//! back to a caller-supplied default when there is no source or its cwd
//! couldn't be determined.
//!
//! Only terminal creation consumes [`resolve_new_session_cwd`] today --
//! this module is the "terminal-cwd sourcing" prerequisite for the
//! sessiond migration (`docs/session-daemon-design.md`). Agent creation
//! still anchors every session to Horizon's own process cwd
//! (`agent::agentd_runtime::AgentdConnection::start_session`'s
//! `session_new_workspace_root` call) rather than to a resolved source --
//! lineage-sourcing the agent side itself is separate, remaining work (see
//! `docs/session-relationship-design.md`'s "Delivery" section).

use std::path::{Path, PathBuf};

use floem::prelude::*;

use crate::session::{Registry, SessionId};
use crate::workspace::{PaneKind, Workspace};

/// What kind of session [`resolve_spawn_cwd`] is deriving a cwd from, with
/// the kind-specific cwd already resolved (or found unavailable) by the
/// caller. Sampling a live pid or reading a workspace root is I/O, kept out
/// of `resolve_spawn_cwd` itself so the resolution *logic* stays a pure,
/// unit-testable function -- see this module's tests, which cover all
/// three branches (terminal source, agent source, no source) without a
/// live pid.
enum SpawnSource {
    /// The source pane hosts a terminal; `cwd` is `None` when the running
    /// shell's pid couldn't be resolved to a live cwd right now (process
    /// gone, permission denied, ...).
    Terminal { cwd: Option<PathBuf> },
    /// The source pane hosts an agent; `workspace_root` mirrors that
    /// session's `SessionNew.workspace_root`.
    Agent { workspace_root: Option<PathBuf> },
}

/// `docs/session-relationship-design.md` decision 3: the default cwd
/// source is "inherit the spawn-source pane's cwd"; `fallback` (the repo
/// root / Horizon's own launch cwd) applies both when there is no source
/// pane at all and when the source's cwd couldn't be determined.
fn resolve_spawn_cwd(source: Option<SpawnSource>, fallback: &Path) -> PathBuf {
    let sourced_cwd = match source {
        Some(SpawnSource::Terminal { cwd }) => cwd,
        Some(SpawnSource::Agent { workspace_root }) => workspace_root,
        None => None,
    };
    sourced_cwd.unwrap_or_else(|| fallback.to_path_buf())
}

/// The orchestration half: resolves `source_session_id`'s kind and
/// samples/looks up its cwd (the I/O `resolve_spawn_cwd` deliberately
/// stays free of), then defers to `resolve_spawn_cwd` for the actual
/// precedence. `source_session_id` is `None` for a spawn with no spawn-
/// source pane at all (e.g. Horizon's own startup sessions) -- the same
/// "no source" case `resolve_spawn_cwd` falls back on.
pub(crate) fn resolve_new_session_cwd(
    source_session_id: Option<SessionId>,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
) -> PathBuf {
    let fallback = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let source = source_session_id.and_then(|source_id| {
        let kind = workspace.with_untracked(|ws| ws.session_pane_kind(source_id))?;
        Some(match kind {
            PaneKind::Terminal => SpawnSource::Terminal {
                cwd: sessions.with_untracked(|registry| registry.terminal_cwd(source_id)),
            },
            PaneKind::Agent => SpawnSource::Agent {
                // No per-session `workspace_root` is cached on Horizon's
                // side yet (see this module's doc comment) -- every agent
                // session's `workspace_root` is `std::env::current_dir()`
                // at creation time today, and Horizon's own cwd never
                // changes at runtime (nothing calls
                // `std::env::set_current_dir`), so reading it again here
                // yields the same value `start_session` already sent.
                workspace_root: std::env::current_dir().ok(),
            },
        })
    });
    resolve_spawn_cwd(source, &fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherits_a_source_terminal_s_sampled_cwd() {
        let fallback = PathBuf::from("/fallback");
        let source_cwd = PathBuf::from("/home/owner/project");

        let resolved = resolve_spawn_cwd(
            Some(SpawnSource::Terminal {
                cwd: Some(source_cwd.clone()),
            }),
            &fallback,
        );

        assert_eq!(resolved, source_cwd);
    }

    #[test]
    fn inherits_a_source_agent_s_workspace_root() {
        let fallback = PathBuf::from("/fallback");
        let workspace_root = PathBuf::from("/home/owner/agent-workspace");

        let resolved = resolve_spawn_cwd(
            Some(SpawnSource::Agent {
                workspace_root: Some(workspace_root.clone()),
            }),
            &fallback,
        );

        assert_eq!(resolved, workspace_root);
    }

    #[test]
    fn falls_back_to_the_default_when_there_is_no_source() {
        let fallback = PathBuf::from("/fallback");

        assert_eq!(resolve_spawn_cwd(None, &fallback), fallback);
    }

    #[test]
    fn falls_back_to_the_default_when_the_source_s_cwd_is_unavailable() {
        let fallback = PathBuf::from("/fallback");

        assert_eq!(
            resolve_spawn_cwd(Some(SpawnSource::Terminal { cwd: None }), &fallback),
            fallback
        );
        assert_eq!(
            resolve_spawn_cwd(
                Some(SpawnSource::Agent {
                    workspace_root: None
                }),
                &fallback
            ),
            fallback
        );
    }

    #[test]
    fn resolve_new_session_cwd_inherits_a_source_terminal_s_live_cwd() {
        let workspace = Workspace::mvp();
        let source_session_id = workspace
            .active_terminal_session_id()
            .expect("mvp() starts with an active terminal session");
        let workspace = RwSignal::new(workspace);

        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut registry = Registry::default();
        // `std::process::id()` is always a live pid, so `Registry::
        // terminal_cwd` samples a real cwd -- this test's own, since that's
        // where the test process itself is running.
        registry.insert_terminal(source_session_id, tx, Some(std::process::id()));
        let sessions = RwSignal::new(registry);

        let expected = std::env::current_dir().expect("current dir must be readable in tests");
        let resolved = resolve_new_session_cwd(Some(source_session_id), workspace, sessions);

        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_new_session_cwd_falls_back_when_there_is_no_source_session() {
        let workspace = RwSignal::new(Workspace::mvp());
        let sessions = RwSignal::new(Registry::default());

        let expected = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let resolved = resolve_new_session_cwd(None, workspace, sessions);

        assert_eq!(resolved, expected);
    }
}
