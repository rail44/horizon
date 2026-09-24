mod builder;
pub use builder::ToolSessionBuilder;

use std::{
    cell::RefCell,
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use crate::config::{AgentToolsConfig, BashToolConfig};
use crate::contract::SessionId;
use crate::frame::AgentFrame;
use crate::judge::JudgeHandle;
use crate::live::LiveState;
use crate::persistence::projection::duckdb::DuckdbStoreHandle;
use crate::skills::SkillRegistry;
use crate::tools::network::{SessionDomainPolicy, SessionNetworkProxy};
use crate::tools::ToolCompletion;

/// Where this session's persisted history lives, for the recall tools
/// (`tools::recall`) to search/read it: the session's own id (the tools'
/// default search/read scope) and the *shared* live DuckDB projection
/// handle (`persistence::projection::duckdb::SharedDuckdbStore::wait`'s
/// result -- see that type's doc comment). `tools::recall` locks this same
/// `Arc` per query; it must never open its own fresh `Store::open` of the
/// same path -- a second independent DuckDB instance against one file is
/// unsound (not just redundant) with `duckdb-rs`'s lack of a cross-instance
/// cache and DuckDB's relaxed durability, confirmed in production as a
/// fresh open reading zero rows for a session with real history. `None`
/// fields mean recall degrades to a clear error instead of a silent no-op
/// or a silent "search everything".
///
/// Only `horizon-agentd`'s real session construction site
/// (`session::run_session`) populates both fields. Every other
/// `ToolSessionState` construction site -- this crate's own tests
/// (`ToolSessionState::new`/`without_root`), and Horizon's UI-side
/// dummy-tool-state test helper in `src/agent/host_tools.rs` -- uses
/// `RecallContext::default()` and keeps behaving exactly as it did before
/// recall existed.
#[derive(Clone, Default)]
pub struct RecallContext {
    pub session_id: Option<SessionId>,
    pub store: Option<DuckdbStoreHandle>,
}

/// Per-session file-tool state: the workspace root every absolute path is
/// confined to, the mtimes recorded by `fs.read`/`fs.write`/`fs.edit` for
/// the staleness gate (`docs/agent-tools-design.md`, "Edit Semantics"), and
/// the resolved `[agent]` tool tuning (`agent::config::AgentToolsConfig`).
/// v1 confines every session to a single root: the process's current
/// directory, canonicalized at session start (see `for_current_dir`).
#[derive(Clone)]
pub struct ToolSessionState {
    inner: Rc<Inner>,
}

struct Inner {
    /// `None` means no root could be established for the session — every
    /// path resolution fails with an actionable error, rather than falling
    /// back to an over-broad root that would confine nothing.
    workspace_root: Option<PathBuf>,
    recorded_mtimes: RefCell<HashMap<PathBuf, SystemTime>>,
    /// Additive, session-local filesystem grants this session's sandboxed
    /// calls run with, snapshotted before each bash job is queued. Two
    /// sources feed it, and both are enforced identically: the trees
    /// `[grants]` names for this session's project, injected at spawn (see
    /// [`ToolSessionBuilder::with_filesystem_grants`]), and whatever a human
    /// or the judge approved after a containment denial (see
    /// [`ToolSessionState::approve_filesystem_grants`]). Stored as grants
    /// rather than as the denials that triggered them: an approved grant is
    /// generally *not* one attempted path's own resolution -- it is the
    /// shaped common ancestor of several
    /// (`horizon_sandbox::suggest_grants`), and a config-injected tree
    /// never had a denial at all.
    filesystem_grants: RefCell<Vec<horizon_sandbox::FilesystemGrant>>,
    /// The `bash` tool's tracked working directory
    /// (`docs/agent-tools-design.md`, "Bash Semantics"): a fresh process per
    /// call, with `cd` persisted across calls by the harness rather than a
    /// live shell. Unlike the rest of this struct, this is `Arc<Mutex<_>>`
    /// rather than `Rc<RefCell<_>>` — bash calls run on a dedicated
    /// background thread (see `tools::bash::exec`), so the handle has to be
    /// `Send`-able out of this otherwise UI-thread-confined struct. Bash is
    /// deliberately not confined to `workspace_root`; approval is its gate.
    bash_cwd: Arc<Mutex<PathBuf>>,
    /// Resolved `[agent]` tuning for the bash/fs tools, read once when this
    /// state is created (config is applied at startup only — see
    /// `AGENTS.md`). `Copy`, so cheap to store by value here and to clone
    /// out via `tools_config`/`bash_config`.
    tools: AgentToolsConfig,
    /// See [`RecallContext`].
    recall: RecallContext,
    /// This session's composed skill registry (`skills::SkillRegistry`) --
    /// embedded builtins plus any `.horizon/skills/` discovered from the
    /// session's cwd, per `skills`' module doc (v2). Empty
    /// ([`SkillRegistry::default`]) at every construction site except the
    /// one production call site (`horizon-agentd`'s `session::run_session`),
    /// which installs the real per-session registry via
    /// [`ToolSessionBuilder::with_skills`] right after construction --
    /// mirroring how [`RecallContext`] is threaded in, except this seat is
    /// set post-construction (via a builder method) rather than as a
    /// `for_current_dir` parameter, so that constructor's signature -- and
    /// every non-production caller of it (this crate's own tests, `tools::
    /// recall`'s tests, Horizon's UI-side dummy-tool-state test helper) --
    /// stays unchanged.
    skills: SkillRegistry,
    /// Host-resolved path to Horizon's single config file
    /// (`$HORIZON_CONFIG`, falling back to `$XDG_CONFIG_HOME/horizon/
    /// config.toml`, falling back to `~/.config/horizon/config.toml`), for
    /// `tools::config`'s `config.read`/`config.write`.
    /// This crate can't resolve that path itself (see `config`'s module
    /// doc -- it has no dependency on `horizon-config`), so it's injected
    /// post-construction the same way [`ToolSessionBuilder::with_skills`] injects the
    /// skill registry: the one production call site
    /// (`horizon-agentd`'s `session::run_session`, which resolves it via
    /// `horizon_config::resolved_path()`) calls
    /// [`ToolSessionBuilder::with_config_path`] right after construction.
    /// `None` everywhere else (this crate's own tests, Horizon's UI-side
    /// dummy-tool-state test helper), same as before this seam existed:
    /// `config.read`/`config.write` degrade to an actionable error instead
    /// of guessing a path.
    config_path: Option<PathBuf>,
    /// Whether `workspace_root` is an isolated worktree the daemon itself
    /// created for this session (`docs/session-relationship-design.md`),
    /// as opposed to a plain shared directory -- the per-call trust
    /// predicate's isolation input (`docs/agent-approval-design.md`'s tier
    /// 1: `policy::classify_call`). Deliberately *not* inferred from
    /// `workspace_root`'s path shape here (e.g. "lives under
    /// `.horizon/worktrees/`") -- the daemon already knows the real
    /// outcome of its own worktree creation (see `horizon-agentd`'s
    /// `resolve_and_create_isolated_worktree`), so this is threaded in
    /// after construction through [`ToolSessionBuilder`], rather than re-derived. `false` everywhere
    /// except the one production call site.
    isolated_worktree: bool,
    /// Whether this session has no client that could answer an approval
    /// prompt: the explore role's sessions (`task` children and
    /// Mixture-of-Agents proposers), which are never attached to a pane and
    /// whose only consumer is whoever is waiting for their final report. A
    /// call that would otherwise wait for a human resolves as a refused
    /// tool result instead, so the session keeps running
    /// (`tools::approval::unattended_refusal_result`). Threaded in after
    /// construction the same way [`Inner::isolated_worktree`] is: only
    /// `horizon-agentd`'s `session::run_session` knows the session's role.
    /// `false` everywhere except that call site.
    unattended: bool,
    /// This session's own network-proxy pair (`docs/agent-approval-
    /// design.md`'s "Staging" leg 4b -- `tools::network::
    /// SessionNetworkProxy`), if one was started for it. `None` means
    /// either this session isn't eligible for tier-1 sandboxed `bash` at
    /// all (not isolated, or no engaged sandbox), the proxy failed to bind,
    /// or this `ToolSessionState` is one of this crate's own test
    /// constructions -- either way, `tools::execution::execute_tier1_bash`
    /// falls back to `NetworkPolicy::Disabled`, exactly the pre-leg-4a
    /// behavior. `Arc` (not a bare value) so the handle is cheap to clone
    /// across this `Rc`-based struct's threading boundary onto the bash
    /// background thread (`tools::bash::exec::run_sandboxed` needs it to
    /// drain denied hosts) the same way `bash_cwd` already crosses that
    /// boundary. Installed through [`ToolSessionBuilder`]: the one production
    /// call site (`horizon-agentd`'s `session::run_session`) is the only
    /// place that knows whether this session is isolated with an engaged
    /// sandbox, the precondition for starting one at all.
    network: Option<Arc<SessionNetworkProxy>>,
    /// Loopback TCP endpoints this session's sandbox may connect to
    /// directly (e.g. sccache on `127.0.0.1:4226`), from the project's
    /// `[grants]` `loopback_connect` entries. Threaded into the
    /// `NetworkPolicy::Proxied` the sandbox spawn builds, alongside the
    /// session proxy address -- the seccomp-notify enforcement matches each
    /// by full `SocketAddr` equality. Empty at every construction site except
    /// `horizon-agentd`'s `session::run_session`, which injects it via
    /// [`ToolSessionBuilder::with_loopback_connect`] the same way filesystem grants are
    /// injected via [`ToolSessionBuilder::with_filesystem_grants`].
    loopback_connect: Vec<SocketAddr>,
    /// One domain-grant store shared by sandboxed proxy traffic and
    /// host-side web tools. It exists even when this session cannot start a
    /// sandbox proxy, so `web_fetch` never needs a separate policy model.
    domains: SessionDomainPolicy,
    /// This session's approved (or config-declared) macOS mach service
    /// grants (`docs/macos-containment-denial-reporting-design.md`):
    /// security services whose seatbelt deny an approval has lifted for
    /// every later sandboxed call in this session. Cross-platform field,
    /// populated only on macOS -- interactively via
    /// `ApprovalKind::MachServiceGrant`, or from the project's
    /// `[[grants.project]]` `mach_services` entries at spawn.
    mach_services: RefCell<Vec<String>>,
    /// This session's enforcing judge handle (`docs/agent-approval-
    /// design.md`'s "Judge design"), if one could be built for it. `None`
    /// means approval candidates go directly to the human (no
    /// `OPENAI_API_KEY`, no event-log writer, or -- every
    /// construction site in this crate's own tests except where a test
    /// explicitly installs one via [`ToolSessionBuilder::with_judge`]) -- see
    /// `JudgeHandle::new`. Injected post-construction the same way
    /// [`ToolSessionBuilder::with_network_proxy`] is: the one production call site
    /// (`horizon-agentd`'s `session::run_session`) is the only place that
    /// has both this session's resolved provider `base_url` and the
    /// process's event-log writer handle.
    judge: Option<Arc<JudgeHandle>>,
    /// This session's handle onto the daemon's spawn/subscribe/terminate
    /// capability for parallel exploration sessions (`tools::explore`,
    /// `docs/agent-explore-design.md`). Injected post-construction the same
    /// way [`Self::judge`]/[`Self::network`] are: only `horizon-agentd`'s
    /// `session::run_session` can host a peer session, and only it knows
    /// this session's workspace root and provider -- both of which the
    /// exploration must share. `None` (every construction site in this
    /// crate's own tests, and deliberately for an exploration session
    /// itself, which must not spawn further explorations) makes
    /// `task` resolve to an actionable error result rather than a
    /// silent no-op.
    exploration: Option<Arc<dyn crate::tools::explore::ExplorationHost>>,
    /// This session's handle onto the daemon's board read/comment capability
    /// (`tools::board`, `docs/board-keeper-design.md`). Injected
    /// post-construction the same way [`Self::exploration`] is: only
    /// `horizon-agentd`'s `session::run_session` can construct a
    /// `horizon_board::Store` (it knows the workspace root and the logd
    /// socket). `None` (every construction site in this crate's own tests)
    /// makes `board.read`/`board.comment` resolve to an actionable error
    /// rather than a silent no-op.
    board: Option<Arc<dyn crate::tools::board::BoardHost>>,
}

impl ToolSessionState {
    #[cfg(test)]
    fn with_root(
        workspace_root: Option<PathBuf>,
        tools: AgentToolsConfig,
        recall: RecallContext,
    ) -> Self {
        ToolSessionBuilder::with_root(workspace_root, tools, recall).build()
    }

    pub fn for_current_dir(tools: AgentToolsConfig, recall: RecallContext) -> Self {
        ToolSessionBuilder::for_current_dir(tools, recall).build()
    }

    pub fn for_root(
        workspace_root: PathBuf,
        tools: AgentToolsConfig,
        recall: RecallContext,
    ) -> Self {
        ToolSessionBuilder::for_root(workspace_root, tools, recall).build()
    }

    #[cfg(test)]
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self::with_root(
            Some(workspace_root),
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
    }

    /// A session with no usable workspace root: every file-tool path
    /// resolution returns an `is_error` result.
    #[cfg(test)]
    pub(crate) fn without_root() -> Self {
        Self::with_root(None, AgentToolsConfig::default(), RecallContext::default())
    }

    /// Whether this session's `workspace_root` is a daemon-created isolated
    /// worktree -- see [`Inner::isolated_worktree`]'s doc comment. `false`
    /// for every session that isn't (including one with no workspace root
    /// at all).
    pub(crate) fn is_isolated_worktree(&self) -> bool {
        self.inner.isolated_worktree
    }

    /// Whether an approval prompt raised for this session would reach
    /// nobody -- see [`Inner::unattended`]'s doc comment.
    pub(crate) fn is_unattended(&self) -> bool {
        self.inner.unattended
    }

    /// This session's approved (or config-declared) macOS mach service
    /// grants (`docs/macos-containment-denial-reporting-design.md`):
    /// security services whose seatbelt deny an approval has lifted for
    /// every later sandboxed call in this session.
    pub(crate) fn mach_services(&self) -> Vec<String> {
        self.inner.mach_services.borrow().clone()
    }

    /// Records approved mach service grants for this session, additively
    /// (same posture as [`Self::approve_filesystem_grants`]). Unlike
    /// filesystem grants these are not revalidated per spawn -- they are
    /// service names, not paths; the enforcement mapping itself is
    /// best-effort per spawn (`horizon_sandbox::security_service_grants`
    /// grants only the keychain database files that currently exist).
    pub(crate) fn approve_mach_services(&self, services: &[String]) {
        let mut approved = self.inner.mach_services.borrow_mut();
        for service in services {
            if !approved.contains(service) {
                approved.push(service.clone());
            }
        }
    }

    /// The grants merged into the next sandboxed spawn's policy: the
    /// revalidated filesystem grants plus, on macOS, the enforcement grants
    /// for this session's mach service set
    /// (`horizon_sandbox::security_service_grants`). Every
    /// `spawn_sandboxed` call site passes this instead of
    /// [`Self::filesystem_grants_snapshot`] so an approved service grant
    /// rides along on any sandboxed spawn, not just the retry that won it.
    pub(crate) fn effective_sandbox_grants(&self) -> Vec<horizon_sandbox::FilesystemGrant> {
        let services = self.mach_services();
        let grants = self.filesystem_grants_snapshot();
        #[cfg(target_os = "macos")]
        {
            let mut grants = grants;
            for grant in horizon_sandbox::security_service_grants(&services) {
                if !grants.contains(&grant) {
                    grants.push(grant);
                }
            }
            grants
        }
        #[cfg(not(target_os = "macos"))]
        {
            debug_assert!(services.is_empty());
            grants
        }
    }

    pub(crate) fn allow_domain(&self, domain: impl Into<String>) {
        self.inner.domains.allow(domain);
    }

    pub(crate) fn is_domain_allowed(&self, domain: &str) -> bool {
        self.inner.domains.is_allowed(domain)
    }

    pub(crate) fn domain_allowlist(&self) -> Arc<horizon_sandbox_proxy::Allowlist> {
        self.inner.domains.shared()
    }

    /// This session's own network-proxy pair, if one is running -- see
    /// [`Inner::network`]'s doc comment. What `tools::execution::
    /// execute_tier1_bash` passes into `bash::spawn_sandboxed`, and what
    /// `tools::approval`'s domain-denial-retry path mutates
    /// (`SessionNetworkProxy::allow_domain`) on approve.
    pub(crate) fn network_proxy(&self) -> Option<Arc<SessionNetworkProxy>> {
        self.inner.network.clone()
    }

    /// The loopback endpoints this session's sandbox may connect to directly
    /// -- see [`Inner::loopback_connect`]'s doc comment. What
    /// `tools::execution::execute_tier1_bash` passes into
    /// `bash::spawn_sandboxed`.
    pub(crate) fn loopback_connect(&self) -> Vec<SocketAddr> {
        self.inner.loopback_connect.clone()
    }

    /// This session's enforcing judge handle, if one is installed.
    pub(crate) fn judge_handle(&self) -> Option<Arc<JudgeHandle>> {
        self.inner.judge.clone()
    }

    /// This session's exploration host, if one is installed -- what
    /// `tools::explore::start` spawns and terminates through, and what
    /// `horizon-agentd` publishes to `tools::moa` for the session loop's
    /// own thread to reach.
    pub fn exploration_host(&self) -> Option<Arc<dyn crate::tools::explore::ExplorationHost>> {
        self.inner.exploration.clone()
    }

    /// Installs this session's board host after construction -- see
    /// [`Inner::board`]'s doc comment. Same construction-time-only safety
    /// contract as [`Self::with_exploration_host`].
    pub fn with_board_host(
        mut self,
        board: Option<Arc<dyn crate::tools::board::BoardHost>>,
    ) -> Self {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            inner.board = board;
        }
        self
    }

    /// This session's board host, if one is installed -- what
    /// `tools::board::execute_auto`/`execute_comment` read and write through.
    pub(crate) fn board_host(&self) -> Option<Arc<dyn crate::tools::board::BoardHost>> {
        self.inner.board.clone()
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.inner.workspace_root.as_deref()
    }

    /// The grants merged into the next sandboxed spawn's policy, each
    /// re-checked against the live filesystem first -- a grant whose target
    /// disappeared, changed kind, or became over-broad simply drops out
    /// instead of widening anything.
    /// Additional grants only: the implicit workspace root is held separately.
    /// Hosts validate this exact retained set before changing environments.
    pub fn retained_filesystem_grants(&self) -> Vec<horizon_sandbox::FilesystemGrant> {
        self.inner.filesystem_grants.borrow().clone()
    }

    pub(crate) fn filesystem_grants_snapshot(&self) -> Vec<horizon_sandbox::FilesystemGrant> {
        self.inner
            .filesystem_grants
            .borrow()
            .iter()
            .filter(|grant| horizon_sandbox::revalidate_grant(grant).is_ok())
            .cloned()
            .collect()
    }

    /// Adds approved grants to this session's list, after revalidating
    /// every one of them. Fails closed as a unit: if any grant no longer
    /// holds up, none are stored.
    pub(crate) fn approve_filesystem_grants(
        &self,
        grants: &[horizon_sandbox::FilesystemGrant],
    ) -> Result<(), horizon_sandbox::SandboxError> {
        for grant in grants {
            horizon_sandbox::revalidate_grant(grant)?;
        }
        let mut approved = self.inner.filesystem_grants.borrow_mut();
        for grant in grants {
            if !approved.contains(grant) {
                approved.push(grant.clone());
            }
        }
        Ok(())
    }

    /// The resolved `[agent]` tool tuning for this session (bash + fs
    /// knobs).
    pub(crate) fn tools_config(&self) -> AgentToolsConfig {
        self.inner.tools
    }

    /// Convenience accessor for just the bash slice of `tools_config` — the
    /// value threaded onto the bash background thread by
    /// `tools::approval::resolve_bash`.
    pub(crate) fn bash_config(&self) -> BashToolConfig {
        self.inner.tools.bash
    }

    /// This session's recall context (see [`RecallContext`]) -- cheap to
    /// clone (an `Option<SessionId>` and an `Option<Arc<Mutex<_>>>`).
    pub(crate) fn recall_context(&self) -> RecallContext {
        self.inner.recall.clone()
    }

    /// This session's composed skill registry (see [`Inner::skills`]) --
    /// what `tools::config`'s `skill.read` dispatch reads from.
    pub(crate) fn skill_registry(&self) -> &SkillRegistry {
        &self.inner.skills
    }

    /// The host-resolved config-file path (see [`Inner::config_path`]) --
    /// what `tools::config`'s `config.read`/`config.write` dispatch reads
    /// from.
    pub(crate) fn config_path(&self) -> Option<&Path> {
        self.inner.config_path.as_deref()
    }

    pub(crate) fn record_mtime(&self, path: PathBuf, mtime: SystemTime) {
        self.inner.recorded_mtimes.borrow_mut().insert(path, mtime);
    }

    pub(crate) fn recorded_mtime(&self, path: &Path) -> Option<SystemTime> {
        self.inner.recorded_mtimes.borrow().get(path).copied()
    }

    /// Clones out the shared handle to bash's tracked cwd, so the
    /// background thread that actually runs a bash call (`tools::bash::
    /// exec`) can read and update it without touching anything else on this
    /// `Rc`-based, UI-thread-confined struct.
    pub(crate) fn bash_cwd_handle(&self) -> Arc<Mutex<PathBuf>> {
        Arc::clone(&self.inner.bash_cwd)
    }

    #[cfg(test)]
    pub(crate) fn bash_cwd(&self) -> PathBuf {
        self.inner
            .bash_cwd
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The per-session runtime the approval UI needs: the tool state above, a
/// handle to the session's live frame/event-log sink so a Horizon-executed
/// approval (`fs.write`/`fs.edit`/`bash`) can fold its result in exactly the
/// way an auto-allowed tool call does (see `agent::tools::approval`), and
/// the sender side of the channel a `bash` call's eventual result is
/// delivered back to the UI thread on (see `tools::bash::BashCompletion`).
#[derive(Clone)]
pub(crate) struct SessionRuntime {
    pub tool_state: ToolSessionState,
    pub live_state: LiveState,
    pub async_results: crossbeam_channel::Sender<ToolCompletion>,
}

thread_local! {
    // LiveState's inner Rc<RefCell<..>> is confined to a single thread, so
    // this registry is too. It bridges `horizon-agentd`'s session loop
    // (`crates/horizon-agentd/src/session.rs`, where a session's runtime
    // is created via `register_session_runtime`) and the approve/deny
    // command handling on that same thread, which don't otherwise share
    // scope.
    static SESSION_RUNTIMES: RefCell<HashMap<SessionId, SessionRuntime>> =
        RefCell::new(HashMap::new());
}

pub fn register_session_runtime(
    session_id: SessionId,
    tool_state: ToolSessionState,
    live_state: LiveState,
    async_results: crossbeam_channel::Sender<ToolCompletion>,
) {
    SESSION_RUNTIMES.with(|runtimes| {
        runtimes.borrow_mut().insert(
            session_id,
            SessionRuntime {
                tool_state,
                live_state,
                async_results,
            },
        );
    });
}

pub(crate) fn session_runtime(session_id: SessionId) -> Option<SessionRuntime> {
    SESSION_RUNTIMES.with(|runtimes| runtimes.borrow().get(&session_id).cloned())
}

/// `session_id`'s current live frame, if it has a registered runtime -- the
/// narrow read the judge needs (prior user messages for the judge's input,
/// `docs/agent-approval-design.md`'s "Input
/// restriction" bullet) without exposing the whole [`SessionRuntime`]
/// outside this module (only `tools::execution`, a sibling submodule,
/// reads `session_runtime` directly today).
pub(crate) fn live_frame_for_session(session_id: SessionId) -> Option<AgentFrame> {
    session_runtime(session_id).map(|runtime| runtime.live_state.frame())
}

/// Drops a terminated session's runtime so its tool state and live frame
/// stop accumulating, and so a stale approval click for it can no longer
/// find anything to execute against. Safe no-op for unknown ids (e.g.
/// terminal sessions, which never register).
pub fn unregister_session_runtime(session_id: SessionId) {
    crate::tools::bash::cancel_session(session_id);
    crate::tools::web::cancel_session(session_id);
    crate::tools::explore::cancel_session(session_id);
    crate::tools::moa::unregister_exploration_host(session_id);
    SESSION_RUNTIMES.with(|runtimes| {
        runtimes.borrow_mut().remove(&session_id);
    });
}
