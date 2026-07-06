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
use crate::live::LiveState;
use crate::persistence::projection::duckdb::DuckdbStoreHandle;
use crate::tools::bash::BashCompletion;

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
}

impl ToolSessionState {
    #[cfg(test)]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::with_root(
            Some(workspace_root),
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
    }

    /// A session with no usable workspace root: every file-tool path
    /// resolution returns an `is_error` result.
    #[cfg(test)]
    pub fn without_root() -> Self {
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
            }),
        }
    }

    /// v1 workspace root: the process's current directory at session start,
    /// canonicalized. If the current directory can't be read or
    /// canonicalized, the session gets no root at all and every file-tool
    /// path is rejected with an actionable error — never a panic, and
    /// never a fallback root that fails open. `tools` is the resolved
    /// `[agent]` tool tuning, and `recall` is this session's recall context
    /// (see [`RecallContext`]) -- both passed in by the caller
    /// (`horizon-agentd`'s `session::run_session`, the one production call
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

    pub fn workspace_root(&self) -> Option<&Path> {
        self.inner.workspace_root.as_deref()
    }

    /// The resolved `[agent]` tool tuning for this session (bash + fs
    /// knobs).
    pub fn tools_config(&self) -> AgentToolsConfig {
        self.inner.tools
    }

    /// Convenience accessor for just the bash slice of `tools_config` — the
    /// value threaded onto the bash background thread by
    /// `tools::approval::resolve_bash`.
    pub fn bash_config(&self) -> BashToolConfig {
        self.inner.tools.bash
    }

    /// This session's recall context (see [`RecallContext`]) -- cheap to
    /// clone (an `Option<SessionId>` and an `Option<Arc<Mutex<_>>>`).
    pub fn recall_context(&self) -> RecallContext {
        self.inner.recall.clone()
    }

    pub fn record_mtime(&self, path: PathBuf, mtime: SystemTime) {
        self.inner.recorded_mtimes.borrow_mut().insert(path, mtime);
    }

    pub fn recorded_mtime(&self, path: &Path) -> Option<SystemTime> {
        self.inner.recorded_mtimes.borrow().get(path).copied()
    }

    /// Clones out the shared handle to bash's tracked cwd, so the
    /// background thread that actually runs a bash call (`tools::bash::
    /// exec`) can read and update it without touching anything else on this
    /// `Rc`-based, UI-thread-confined struct.
    pub fn bash_cwd_handle(&self) -> Arc<Mutex<PathBuf>> {
        Arc::clone(&self.inner.bash_cwd)
    }

    #[cfg(test)]
    pub fn bash_cwd(&self) -> PathBuf {
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
    // Horizon's UI/reactive state (RwSignal, and LiveState's inner
    // Rc<RefCell<..>>) is confined to a single thread, so this registry is
    // too. It bridges `app/runtime/agent.rs` (where a session's runtime is
    // created) and `workspace/view/pane.rs` (where the user's approve/deny
    // click needs it back), which don't otherwise share scope.
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

pub fn session_runtime(session_id: SessionId) -> Option<SessionRuntime> {
    SESSION_RUNTIMES.with(|runtimes| runtimes.borrow().get(&session_id).cloned())
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
