//! What a host's composer adds to the agent without the host naming it:
//! roles, skills and tools, what starts sessions on its own and where
//! their answers go, and the board tool's host. The board and the wiki are
//! the two today (`horizon-board-flow`, `horizon-wiki-flow`).

use std::path::Path;
use std::sync::Arc;

use horizon_agent::packages::{Packages, PackagesBuilder};
use horizon_agent::tools::BoardHost;

use crate::outbox::{ReplySink, TriggerSource};
use crate::session::AgentHostState;

/// One addition to the host. Every method has a default that adds nothing.
pub trait Extension: Send + Sync + 'static {
    /// Adds roles, skills and tools to every session.
    fn packages(&self, packages: PackagesBuilder) -> PackagesBuilder {
        packages
    }

    /// What starts sessions on its own, driven by the outbox.
    fn trigger_sources(&self) -> Vec<Box<dyn TriggerSource>> {
        Vec::new()
    }

    /// Where answers addressed by this extension's inputs are delivered.
    fn reply_sinks(&self) -> Vec<Arc<dyn ReplySink>> {
        Vec::new()
    }

    /// The board tool's host for a session rooted at `root`.
    fn board_host(&self, _state: &Arc<AgentHostState>, _root: &Path) -> Option<Arc<dyn BoardHost>> {
        None
    }
}

/// The extensions a host is composed with, in order.
#[derive(Clone, Default)]
pub struct Extensions(Vec<Arc<dyn Extension>>);

impl Extensions {
    pub fn new(extensions: Vec<Arc<dyn Extension>>) -> Self {
        Self(extensions)
    }

    /// The roles, skills and tools every extension adds.
    pub fn packages(&self) -> Packages {
        self.0
            .iter()
            .fold(Packages::builder(), |packages, extension| {
                extension.packages(packages)
            })
            .build()
    }

    pub fn trigger_sources(&self) -> Vec<Box<dyn TriggerSource>> {
        self.0
            .iter()
            .flat_map(|extension| extension.trigger_sources())
            .collect()
    }

    pub fn reply_sinks(&self) -> Vec<Arc<dyn ReplySink>> {
        self.0
            .iter()
            .flat_map(|extension| extension.reply_sinks())
            .collect()
    }

    pub(crate) fn board_host(
        &self,
        state: &Arc<AgentHostState>,
        root: &Path,
    ) -> Option<Arc<dyn BoardHost>> {
        self.0
            .iter()
            .find_map(|extension| extension.board_host(state, root))
    }
}
