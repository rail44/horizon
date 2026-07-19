use std::{
    cell::RefCell,
    collections::HashMap,
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
use crate::tools::bash::BashCompletion;
use crate::tools::network::SessionNetworkProxy;

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
/// Only `horizon-sessiond`'s real session construction site
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
    /// one production call site (`horizon-sessiond`'s `session::run_session`),
    /// which installs the real per-session registry via
    /// [`ToolSessionState::with_skills`] right after construction --
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
    /// post-construction the same way [`Self::with_skills`] injects the
    /// skill registry: the one production call site
    /// (`horizon-sessiond`'s `session::run_session`, which resolves it via
    /// `horizon_config::resolved_path()`) calls
    /// [`ToolSessionState::with_config_path`] right after construction.
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
    /// outcome of its own worktree creation (see `horizon-sessiond`'s
    /// `resolve_and_create_isolated_worktree`), so this is threaded in
    /// after construction the same way [`Self::with_skills`]/[`Self::
    /// with_config_path`] are, rather than re-derived. `false` everywhere
    /// except the one production call site.
    isolated_worktree: bool,
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
    /// boundary. Injected post-construction the same way [`Self::
    /// with_skills`]/[`Self::with_config_path`] are: the one production
    /// call site (`horizon-sessiond`'s `session::run_session`) is the only
    /// place that knows whether this session is isolated with an engaged
    /// sandbox, the precondition for starting one at all.
    network: Option<Arc<SessionNetworkProxy>>,
    /// This session's shadow-mode judge handle (`docs/agent-approval-
    /// design.md`'s "Judge design", implemented in shadow mode only --
    /// `crate::judge`'s module doc), if one could be built for it. `None`
    /// means the judge never fires for this session's boundary-crossing
    /// calls (no `OPENAI_API_KEY`, no event-log writer, or -- every
    /// construction site in this crate's own tests except where a test
    /// explicitly installs one via [`Self::with_judge`]) -- see
    /// `JudgeHandle::new`. Injected post-construction the same way
    /// [`Self::with_network_proxy`] is: the one production call site
    /// (`horizon-sessiond`'s `session::run_session`) is the only place that
    /// has both this session's resolved provider `base_url` and the
    /// process's event-log writer handle.
    judge: Option<Arc<JudgeHandle>>,
}

impl ToolSessionState {
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

    fn with_root(
        workspace_root: Option<PathBuf>,
        tools: AgentToolsConfig,
        recall: RecallContext,
    ) -> Self {
        // Bash's initial tracked cwd is "the workspace root"
        // (`docs/agent-tools-design.md`); if no root could be established,
        // fall back to the raw (non-canonicalized) current directory, and
        // failing that, `/` — bash still needs *some* starting directory
        // even when the file tools' stricter root requirement can't be met.
        let bash_cwd = workspace_root
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        Self {
            inner: Rc::new(Inner {
                workspace_root,
                recorded_mtimes: RefCell::new(HashMap::new()),
                bash_cwd: Arc::new(Mutex::new(bash_cwd)),
                tools,
                recall,
                skills: SkillRegistry::default(),
                config_path: None,
                isolated_worktree: false,
                network: None,
                judge: None,
            }),
        }
    }

    /// Installs this session's real skill registry after construction --
    /// the one production call site (`horizon-sessiond`'s
    /// `session::run_session`) uses this to attach the per-session
    /// [`SkillRegistry::discover`] result once it's built, without adding a
    /// parameter to [`Self::for_current_dir`] (see [`Inner::skills`]'s doc
    /// comment for why). Safe to call only right after construction, before
    /// this `ToolSessionState` has been cloned anywhere else -- `Rc::
    /// get_mut` silently does nothing if that invariant is violated (no
    /// other production caller does).
    pub fn with_skills(mut self, skills: SkillRegistry) -> Self {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            inner.skills = skills;
        }
        self
    }

    /// Installs the host-resolved config-file path after construction --
    /// see [`Inner::config_path`]'s doc comment. Same safety contract as
    /// [`Self::with_skills`]: call only right after construction, before
    /// this `ToolSessionState` has been cloned anywhere else.
    pub fn with_config_path(mut self, config_path: Option<PathBuf>) -> Self {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            inner.config_path = config_path;
        }
        self
    }

    /// Records whether `workspace_root` is an isolated worktree the daemon
    /// itself created for this session -- see [`Inner::isolated_worktree`]'s
    /// doc comment. Same construction-time-only safety contract as
    /// [`Self::with_skills`]/[`Self::with_config_path`].
    pub fn with_isolated_worktree(mut self, isolated: bool) -> Self {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            inner.isolated_worktree = isolated;
        }
        self
    }

    /// Whether this session's `workspace_root` is a daemon-created isolated
    /// worktree -- see [`Inner::isolated_worktree`]'s doc comment. `false`
    /// for every session that isn't (including one with no workspace root
    /// at all).
    pub(crate) fn is_isolated_worktree(&self) -> bool {
        self.inner.isolated_worktree
    }

    /// Installs this session's own network-proxy pair after construction --
    /// see [`Inner::network`]'s doc comment. Same construction-time-only
    /// safety contract as [`Self::with_skills`]/[`Self::with_config_path`].
    pub fn with_network_proxy(mut self, network: Option<Arc<SessionNetworkProxy>>) -> Self {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            inner.network = network;
        }
        self
    }

    /// This session's own network-proxy pair, if one is running -- see
    /// [`Inner::network`]'s doc comment. What `tools::execution::
    /// execute_tier1_bash` passes into `bash::spawn_sandboxed`, and what
    /// `tools::approval`'s domain-denial-retry path mutates
    /// (`SessionNetworkProxy::allow_domain`) on approve.
    pub(crate) fn network_proxy(&self) -> Option<Arc<SessionNetworkProxy>> {
        self.inner.network.clone()
    }

    /// Installs this session's shadow-mode judge handle after construction
    /// -- see [`Inner::judge`]'s doc comment. Same construction-time-only
    /// safety contract as [`Self::with_skills`]/[`Self::with_config_path`]/
    /// [`Self::with_network_proxy`].
    pub fn with_judge(mut self, judge: Option<Arc<JudgeHandle>>) -> Self {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            inner.judge = judge;
        }
        self
    }

    /// This session's shadow-mode judge handle, if one is installed -- see
    /// [`Inner::judge`]'s doc comment. What `judge::maybe_fire_shadow_judge`
    /// (called from `policy::horizon_events_for_provider_event`'s
    /// `Classification::BoundaryCrossing` arm) reads to decide whether to
    /// fire at all.
    pub(crate) fn judge_handle(&self) -> Option<Arc<JudgeHandle>> {
        self.inner.judge.clone()
    }

    /// v1 workspace root: the process's current directory at session start,
    /// canonicalized. If the current directory can't be read or
    /// canonicalized, the session gets no root at all and every file-tool
    /// path is rejected with an actionable error — never a panic, and
    /// never a fallback root that fails open. `tools` is the resolved
    /// `[agent]` tool tuning, and `recall` is this session's recall context
    /// (see [`RecallContext`]) -- both passed in by the caller
    /// (`horizon-sessiond`'s `session::run_session`, the one production call
    /// site) rather than resolved here — this crate can't read Horizon's
    /// config file itself (see `config`'s module doc), and the caller has
    /// already resolved a full `AgentConfig` (and knows its own session id)
    /// by the time it spawns a session.
    pub fn for_current_dir(tools: AgentToolsConfig, recall: RecallContext) -> Self {
        let root = std::env::current_dir()
            .and_then(|dir| dir.canonicalize())
            .ok();
        Self::with_root(root, tools, recall)
    }

    /// A session confined to an explicit directory rather than this
    /// process's own current directory -- the per-session
    /// `wire::SessionNew::workspace_root`, when a caller supplies one,
    /// instead of the `for_current_dir` fallback. Canonicalized the same
    /// way `for_current_dir` canonicalizes the process cwd: if
    /// canonicalization fails, the session gets no root at all (see
    /// [`Inner::workspace_root`]'s doc comment), never a fallback that
    /// fails open.
    pub fn for_root(
        workspace_root: PathBuf,
        tools: AgentToolsConfig,
        recall: RecallContext,
    ) -> Self {
        let root = workspace_root.canonicalize().ok();
        Self::with_root(root, tools, recall)
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.inner.workspace_root.as_deref()
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
pub struct SessionRuntime {
    pub tool_state: ToolSessionState,
    pub live_state: LiveState,
    pub bash_results: crossbeam_channel::Sender<BashCompletion>,
}

thread_local! {
    // LiveState's inner Rc<RefCell<..>> is confined to a single thread, so
    // this registry is too. It bridges `horizon-sessiond`'s session loop
    // (`crates/horizon-sessiond/src/session.rs`, where a session's runtime
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
    bash_results: crossbeam_channel::Sender<BashCompletion>,
) {
    SESSION_RUNTIMES.with(|runtimes| {
        runtimes.borrow_mut().insert(
            session_id,
            SessionRuntime {
                tool_state,
                live_state,
                bash_results,
            },
        );
    });
}

pub(crate) fn session_runtime(session_id: SessionId) -> Option<SessionRuntime> {
    SESSION_RUNTIMES.with(|runtimes| runtimes.borrow().get(&session_id).cloned())
}

/// `session_id`'s current live frame, if it has a registered runtime -- the
/// narrow read `judge::maybe_fire_shadow_judge` needs (prior user messages
/// for the shadow judge's input, `docs/agent-approval-design.md`'s "Input
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
    SESSION_RUNTIMES.with(|runtimes| {
        runtimes.borrow_mut().remove(&session_id);
    });
}
