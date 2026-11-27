mod builder;
pub use builder::ToolSessionBuilder;

use std::{
    cell::RefCell,
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::SystemTime,
};

use crate::config::{AgentToolsConfig, BashToolConfig};
use crate::contract::SessionId;
use crate::frame::AgentFrame;
use crate::live::LiveState;
use crate::persistence::projection::duckdb::Projection;
use crate::skills::SkillRegistry;
use crate::tools::ToolCompletion;
use crate::tools::network::SessionDomainPolicy;

/// Where this session's persisted history lives, for the recall tools
/// (`tools::recall`): the session's own id (the tools' default scope) and
/// the projection of every session's log they query
/// (`persistence::projection::duckdb::Projection`). Without either, recall
/// answers with an error rather than a silent no-op or a search of
/// everything. Only the host's session construction populates them.
#[derive(Clone, Debug, Default)]
pub struct RecallContext {
    pub session_id: Option<SessionId>,
    pub projection: Option<Projection>,
}

/// Why a session may write nothing at all. Its shell runs with nothing
/// writable, and `fs_write`/`fs_edit`, a `bash` call outside the sandbox,
/// and a write `task` child are refused before anything runs. The reason only changes what the session is told
/// to do instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOnly {
    /// A delegated `task` child that only reads
    /// (`docs/agent-subagent-design.md` decision 7): whatever writes is
    /// left to the session that delegated to it.
    Delegated,
    /// A session whose role writes only in a worktree of its own
    /// (`roles::RoleWrites::InOwnWorktree`) and that has none yet.
    UntilOwnWorktree,
}

impl ReadOnly {
    /// What the session is told when a write is refused: why nothing can
    /// grant it, and what to do instead.
    pub(crate) fn guidance(self) -> &'static str {
        match self {
            Self::Delegated => {
                "this session is read-only: its shell can write nowhere, so nothing can grant \
                 this. Use commands that only read, and leave the step that writes to the \
                 session that delegated to you"
            }
            Self::UntilOwnWorktree => {
                "this session writes only in a worktree of its own and has none yet, so nothing \
                 can grant this. Use commands and tools that only read; writing files, building, \
                 and running tests wait until the session's own worktree is active"
            }
        }
    }
}

/// Per-session file-tool state: the workspace root every absolute path is
/// confined to, the mtimes recorded by `fs_read`/`fs_write`/`fs_edit` for
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
    /// The trees `[grants]` names for this session's project, injected at
    /// spawn (see [`ToolSessionBuilder::with_filesystem_grants`]), or
    /// inherited from the session that started this one: writable for its
    /// contained calls.
    filesystem_grants: RefCell<Vec<horizon_sandbox::FilesystemGrant>>,
    /// What the session may write of its own worktree's Git metadata
    /// (`docs/agent-subagent-design.md` decision 10), resolved by the host
    /// when the session's root is a worktree made for it. Merged into every
    /// sandboxed spawn like the grants above, but neither retained nor
    /// handed on: it belongs to this worktree, not to the session's
    /// authority.
    own_worktree_grants: Vec<horizon_sandbox::FilesystemGrant>,
    /// Where every `bash` call starts (`docs/agent-tools-design.md`, "Bash
    /// Semantics"): the workspace root. A fresh process per call, and a `cd`
    /// lasts for its own call only.
    bash_dir: PathBuf,
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
    /// one production call site (`horizon-agent-host`'s `session::run_session`),
    /// which installs the real per-session registry via
    /// [`ToolSessionBuilder::with_skills`] right after construction --
    /// mirroring how [`RecallContext`] is threaded in, except this seat is
    /// set post-construction (via a builder method) rather than as a
    /// `for_current_dir` parameter, so that constructor's signature -- and
    /// every non-production caller of it (this crate's own tests, `tools::
    /// recall`'s tests, Horizon's UI-side dummy-tool-state test helper) --
    /// stays unchanged.
    skills: SkillRegistry,
    /// The roles, skills and tools packages add (`crate::packages`).
    packages: crate::packages::Packages,
    /// Horizon's data home, where the knowledge tools keep their store
    /// (`RigAgentConfig::data_home`). `None`: they have none.
    data_home: Option<PathBuf>,
    /// Whether nobody is there to answer an approval prompt for this session
    /// (`roles::RoleDefinition::unattended`): a wiki thread, a board
    /// session. A call that would otherwise wait for a human resolves as a
    /// refused tool result instead, so the session keeps running
    /// (`tools::approval::unattended_refusal_result`). Threaded in after
    /// construction through [`ToolSessionBuilder`]: only
    /// `horizon-agent-host`'s `session::run_session` knows the session's role.
    /// `false` everywhere except that call site.
    unattended: bool,
    /// Why this session may write nothing at all, or `None` when it may
    /// (`roles::RoleDefinition::writes`, `docs/agent-subagent-design.md`
    /// decision 7): its sandbox gets no writable root and no write grant,
    /// and the file-writing tools are refused before they run. Threaded in
    /// through [`ToolSessionBuilder`] alongside `unattended`.
    read_only: Option<ReadOnly>,
    /// Loopback TCP endpoints this session's contained calls may connect to
    /// (e.g. a compile cache on `127.0.0.1:4226`), from the project's
    /// `[grants]` `network` `ip:port` entries.
    loopback_connect: Vec<SocketAddr>,
    /// The hosts this session may reach: `web_fetch`'s, and those a
    /// contained call's runtime proxy lets through.
    domains: SessionDomainPolicy,
    /// macOS services this session's contained calls may look up, from the
    /// project's `[[grants.project]]` `mach_services` entries.
    mach_services: Vec<String>,
    /// This session's enforcing judge handle (`docs/agent-approval-
    /// design.md`'s "Judge design"), if one could be built for it. `None`
    /// means approval candidates go directly to the human (no
    /// `OPENAI_API_KEY`, no event-log writer, or -- every
    /// construction site in this crate's own tests except where a test
    /// explicitly installs one via [`ToolSessionBuilder::with_judge`]) -- see
    /// `JudgeHandle::new`. Injected post-construction: the one production call site
    /// (`horizon-agent-host`'s `session::run_session`) is the only place that
    /// has both this session's resolved provider `base_url` and the
    /// process's event-log writer handle.
    /// The verdict stage's modules (`modules::verdict`): the tiers, and
    /// the judge when the session has one.
    modules: crate::modules::Modules,
    /// This session's handle onto the host's spawn/subscribe/terminate
    /// capability for parallel exploration sessions (`tools::explore`,
    /// `docs/agent-explore-design.md`). Injected post-construction the same
    /// way [`Self::judge`] is: only `horizon-agent-host`'s
    /// `session::run_session` can host a peer session, and only it knows
    /// this session's workspace root and provider -- both of which the
    /// exploration must share. `None` (every construction site in this
    /// crate's own tests, and deliberately for an exploration session
    /// itself, which must not spawn further explorations) makes
    /// `task` resolve to an actionable error result rather than a
    /// silent no-op.
    exploration: Option<Arc<dyn crate::tools::explore::ExplorationHost>>,
    /// This session's handle onto the host's board read/comment capability
    /// (`tools::board`, `docs/board-keeper-design.md`). Injected
    /// post-construction the same way [`Self::exploration`] is: only
    /// `horizon-agent-host`'s `session::run_session` can construct a
    /// `horizon_board::Store` (it knows the workspace root and the logd
    /// socket). `None` (every construction site in this crate's own tests)
    /// makes `board_session` resolve to an actionable error rather than a
    /// silent no-op.
    board: Option<Arc<dyn crate::tools::board::BoardHost>>,
    /// The primary checkout this session may modify only after one explicit
    /// approval. `None` for isolated or non-primary working roots. Until it
    /// is approved, a contained call writes nothing of it.
    primary_checkout: Option<PrimaryCheckout>,
}

#[derive(Debug)]
struct PrimaryCheckout {
    root: PathBuf,
    approved: std::cell::Cell<bool>,
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

    /// Whether an approval prompt raised for this session would reach
    /// nobody -- see [`Inner::unattended`]'s doc comment.
    pub(crate) fn is_unattended(&self) -> bool {
        self.inner.unattended
    }

    /// Why this session may write nothing at all, if it may not -- see
    /// [`Inner::read_only`]'s doc comment.
    pub(crate) fn read_only(&self) -> Option<ReadOnly> {
        self.inner.read_only
    }

    /// macOS services this session's contained calls may look up.
    pub(crate) fn mach_services(&self) -> Vec<String> {
        self.inner.mach_services.clone()
    }

    /// The configured and own-worktree grants a contained call's
    /// containment is built from, each re-checked against the live
    /// filesystem first.
    pub(crate) fn effective_sandbox_grants(&self) -> Vec<horizon_sandbox::FilesystemGrant> {
        let mut grants = self.filesystem_grants_snapshot();
        for grant in &self.inner.own_worktree_grants {
            if !grants.contains(grant) && horizon_sandbox::revalidate_grant(grant).is_ok() {
                grants.push(grant.clone());
            }
        }
        grants
    }

    pub(crate) fn allow_domain(&self, domain: impl Into<String>) {
        self.inner.domains.allow(domain);
    }

    /// Copies every domain `previous` allows into this session's own store.
    /// `horizon-agent-host` calls this when an
    /// environment activation replaces the tool state, so a domain approved
    /// "for this session" stays approved after the session enters its
    /// worktree; the replacement otherwise starts from the project's
    /// configured domains alone.
    pub fn retain_domains_from(&self, previous: &ToolSessionState) {
        for domain in previous.inner.domains.domains() {
            self.allow_domain(domain);
        }
    }

    /// Every domain this session may reach right now -- the copy a `task`
    /// child starts with (`docs/agent-subagent-design.md` decision 12).
    pub(crate) fn allowed_domains(&self) -> Vec<String> {
        self.inner.domains.domains()
    }

    pub(crate) fn is_domain_allowed(&self, domain: &str) -> bool {
        self.inner.domains.is_allowed(domain)
    }

    pub(crate) fn domain_allowlist(&self) -> Arc<crate::tools::network::Allowlist> {
        self.inner.domains.shared()
    }

    /// The loopback endpoints this session's contained calls may connect to.
    pub(crate) fn loopback_connect(&self) -> Vec<SocketAddr> {
        self.inner.loopback_connect.clone()
    }

    /// This session's enforcing judge handle, if one is installed.
    pub(crate) fn verdict_modules(&self) -> &[Arc<dyn crate::modules::SessionModule>] {
        self.inner.modules.as_slice()
    }

    /// This session's exploration host, if one is installed -- what
    /// `tools::explore::start` spawns and terminates through.
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

    /// Where the knowledge tools keep their store, when they have one.
    pub(crate) fn data_home(&self) -> Option<&Path> {
        self.inner.data_home.as_deref()
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.inner.workspace_root.as_deref()
    }

    /// The primary checkout this session may read but not write until it is
    /// approved. `None` once approved, and for every non-primary root.
    pub(crate) fn primary_checkout_without_write_access(&self) -> Option<&Path> {
        self.inner
            .primary_checkout
            .as_ref()
            .filter(|checkout| !checkout.approved.get())
            .map(|checkout| checkout.root.as_path())
    }

    /// Whether this session may write to its primary checkout. Always true
    /// for a root that is not a primary checkout.
    pub(crate) fn primary_checkout_writable(&self) -> bool {
        self.inner
            .primary_checkout
            .as_ref()
            .is_none_or(|checkout| checkout.approved.get())
    }

    /// Record the one session-scoped approval for direct primary-checkout
    /// work. It is deliberately not persisted across a `horizon-agent-acp` restart.
    pub(crate) fn approve_primary_checkout(&self) {
        if let Some(checkout) = &self.inner.primary_checkout {
            checkout.approved.set(true);
        }
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
    /// what `tools::config`'s `skill_read` dispatch reads from.
    pub(crate) fn skill_registry(&self) -> &SkillRegistry {
        &self.inner.skills
    }

    pub(crate) fn packages(&self) -> &crate::packages::Packages {
        &self.inner.packages
    }

    pub(crate) fn record_mtime(&self, path: PathBuf, mtime: SystemTime) {
        self.inner.recorded_mtimes.borrow_mut().insert(path, mtime);
    }

    pub(crate) fn recorded_mtime(&self, path: &Path) -> Option<SystemTime> {
        self.inner.recorded_mtimes.borrow().get(path).copied()
    }

    /// Where every `bash` call starts.
    pub(crate) fn bash_dir(&self) -> PathBuf {
        self.inner.bash_dir.clone()
    }
}

/// The per-session runtime the approval UI needs: the tool state above, a
/// handle to the session's live frame/event-log sink so a Horizon-executed
/// approval (`fs_write`/`fs_edit`/`bash`) can fold its result in exactly the
/// way an auto-allowed tool call does (see `agent::tools::approval`), and
/// the sender side of the channel a `bash` call's eventual result is
/// delivered back to the UI thread on (see `tools::bash::BashCompletion`).
#[derive(Clone)]
pub(crate) struct SessionRuntime {
    _work: std::rc::Rc<super::background::SessionOwner>,
    pub tool_state: ToolSessionState,
    pub live_state: LiveState,
    pub async_results: crossbeam_channel::Sender<ToolCompletion>,
}

thread_local! {
    // LiveState's inner Rc<RefCell<..>> is confined to a single thread, so
    // this registry is too. It bridges `horizon-agent-host`'s session loop
    // (`crates/horizon-agent-host/src/session/`, where a session's runtime
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
        let mut runtimes = runtimes.borrow_mut();
        // Environment activation replaces tool state within the same session.
        let owner = runtimes
            .get(&session_id)
            .map(|runtime| runtime._work.clone())
            .unwrap_or_else(|| std::rc::Rc::new(super::background::SessionOwner::new(session_id)));
        runtimes.insert(
            session_id,
            SessionRuntime {
                _work: owner,
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
    super::background::close_session(session_id);
    super::web::clear_session_approvals(session_id);
    crate::tools::explore::cancel_session(session_id);
    crate::tools::sandbox::remove_session_temp_dir(session_id);
    SESSION_RUNTIMES.with(|runtimes| {
        runtimes.borrow_mut().remove(&session_id);
    });
}
