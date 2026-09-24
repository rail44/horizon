//! Construction before sharing: configuration cannot silently disappear after a clone.

use super::{AgentToolsConfig, Inner, RecallContext, ToolSessionState};
use crate::judge::JudgeHandle;
use crate::skills::SkillRegistry;
use crate::tools::network::{SessionDomainPolicy, SessionNetworkProxy};
use std::{
    cell::RefCell,
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

/// Owns a session's initial capabilities until `build` makes the state shareable.
/// Runtime grants remain operations on `ToolSessionState`; initial configuration
/// cannot be changed through an alias to a running session.
pub struct ToolSessionBuilder {
    inner: Inner,
}

impl ToolSessionBuilder {
    #[cfg(test)]
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self::with_root(
            Some(workspace_root),
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
    }

    pub fn build(self) -> ToolSessionState {
        ToolSessionState {
            inner: Rc::new(self.inner),
        }
    }

    /// Initialize the root and runtime storage before any handle can be shared.
    pub(super) fn with_root(
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
            inner: Inner {
                workspace_root,
                recorded_mtimes: RefCell::new(HashMap::new()),
                filesystem_grants: RefCell::new(Vec::new()),
                bash_cwd: Arc::new(Mutex::new(bash_cwd)),
                tools,
                recall,
                skills: SkillRegistry::default(),
                config_path: None,
                isolated_worktree: false,
                unattended: false,
                network: None,
                loopback_connect: Vec::new(),
                domains: SessionDomainPolicy::default(),
                mach_services: RefCell::new(Vec::new()),
                judge: None,
                exploration: None,
                board: None,
            },
        }
    }

    /// Canonicalize the current directory; failure leaves file tools without authority.
    pub fn for_current_dir(tools: AgentToolsConfig, recall: RecallContext) -> Self {
        let root = std::env::current_dir()
            .and_then(|dir| dir.canonicalize())
            .ok();
        Self::with_root(root, tools, recall)
    }

    /// Canonicalize an explicit root; failure leaves file tools without authority.
    pub fn for_root(
        workspace_root: PathBuf,
        tools: AgentToolsConfig,
        recall: RecallContext,
    ) -> Self {
        let root = workspace_root.canonicalize().ok();
        Self::with_root(root, tools, recall)
    }

    /// Install the host-discovered skill registry.
    pub fn with_skills(mut self, skills: SkillRegistry) -> Self {
        self.inner.skills = skills;
        self
    }

    /// Install the config path resolved by the host.
    pub fn with_config_path(mut self, config_path: Option<PathBuf>) -> Self {
        self.inner.config_path = config_path;
        self
    }

    /// Record the outcome of host worktree creation, never infer it from a path.
    pub fn with_isolated_worktree(mut self, isolated: bool) -> Self {
        self.inner.isolated_worktree = isolated;
        self
    }

    /// Record whether any client can answer an approval prompt.
    pub fn with_unattended(mut self, unattended: bool) -> Self {
        self.inner.unattended = unattended;
        self
    }

    /// Seed configured macOS service grants.
    pub fn with_mach_services(mut self, services: Vec<String>) -> Self {
        self.inner.mach_services = RefCell::new(services);
        self
    }

    /// Install this session's proxy when sandboxed networking is available.
    pub fn with_network_proxy(mut self, network: Option<Arc<SessionNetworkProxy>>) -> Self {
        self.inner.network = network;
        self
    }

    /// Seed the configured direct-connect endpoints.
    pub fn with_loopback_connect(mut self, endpoints: Vec<SocketAddr>) -> Self {
        self.inner.loopback_connect = endpoints;
        self
    }

    /// Share the domain authority used by the host-side tools and proxy.
    pub fn with_domain_policy(mut self, domains: SessionDomainPolicy) -> Self {
        self.inner.domains = domains;
        self
    }

    /// Install the enforcing judge, or retain human approval when unavailable.
    pub fn with_judge(mut self, judge: Option<Arc<JudgeHandle>>) -> Self {
        self.inner.judge = judge;
        self
    }

    /// Install the host capability for child sessions.
    pub fn with_exploration_host(
        mut self,
        exploration: Option<Arc<dyn crate::tools::explore::ExplorationHost>>,
    ) -> Self {
        self.inner.exploration = exploration;
        self
    }

    /// Seed only grants that still revalidate against the filesystem.
    pub fn with_filesystem_grants(mut self, grants: Vec<horizon_sandbox::FilesystemGrant>) -> Self {
        self.inner.filesystem_grants = RefCell::new(
            grants
                .into_iter()
                .filter(|grant| horizon_sandbox::revalidate_grant(grant).is_ok())
                .collect(),
        );
        self
    }
}
