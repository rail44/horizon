//! What packages outside this crate add to the agent -- roles, skills and
//! tools -- as one value the composition root (`horizon-agentd`) builds at
//! startup and hands to what needs it: the provider registry and every
//! session's tool state. Nothing here is global; a process that builds no
//! `Packages` has the built-in roles, skills and tools only.

use std::sync::Arc;

use crate::roles::{RoleDefinition, RoleId};
use crate::tools::ExternalTool;

#[derive(Clone, Default)]
pub struct Packages(Arc<Inner>);

#[derive(Default)]
struct Inner {
    roles: Vec<&'static RoleDefinition>,
    skill_sources: Vec<&'static str>,
    tools: Vec<ExternalTool>,
}

/// Two `Packages` are equal when they are the same composition.
impl PartialEq for Packages {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Packages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packages")
            .field(
                "roles",
                &self.0.roles.iter().map(|r| r.id).collect::<Vec<_>>(),
            )
            .field("skill_sources", &self.0.skill_sources.len())
            .field(
                "tools",
                &self.0.tools.iter().map(|t| &t.id).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Builds a [`Packages`]: each package adds its roles, skill sources and
/// tools.
#[derive(Default)]
pub struct PackagesBuilder {
    inner: Inner,
}

impl PackagesBuilder {
    /// Adds roles. A role whose id is already known, built in or added
    /// before, is ignored. A definition is leaked to `'static`: sessions
    /// hold on to it for the process's life.
    pub fn roles(mut self, roles: Vec<RoleDefinition>) -> Self {
        for role in roles {
            let known = crate::roles::builtin(&RoleId(role.id.to_string())).is_some()
                || self.inner.roles.iter().any(|known| known.id == role.id);
            if !known {
                self.inner.roles.push(Box::leak(Box::new(role)));
            }
        }
        self
    }

    /// Adds embedded skill sources, each a `SKILL.md` the providing crate
    /// loaded with `include_str!`.
    pub fn skill_sources(mut self, sources: Vec<&'static str>) -> Self {
        for source in sources {
            if !self.inner.skill_sources.contains(&source) {
                self.inner.skill_sources.push(source);
            }
        }
        self
    }

    /// Adds tools. Refuses the whole batch, leaving the builder as it was,
    /// when an id is not a name a provider accepts, repeats within it, or is
    /// already taken by a built-in or an added tool.
    pub fn tools(mut self, tools: Vec<ExternalTool>) -> Result<Self, (Self, String)> {
        if let Err(error) = self.refusal(&tools) {
            return Err((self, error));
        }
        self.inner.tools.extend(tools);
        Ok(self)
    }

    fn refusal(&self, tools: &[ExternalTool]) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for tool in tools {
            if !crate::tools::is_valid_tool_id(&tool.id) {
                return Err(format!(
                    "tool id `{}` is not 1-64 of a-z, A-Z, 0-9, `_` and `-`",
                    tool.id
                ));
            }
            let taken = crate::tools::is_builtin_tool(&tool.id)
                || self
                    .inner
                    .tools
                    .iter()
                    .any(|existing| existing.id == tool.id);
            if taken || !seen.insert(tool.id.as_str()) {
                return Err(format!("tool id `{}` is already taken", tool.id));
            }
        }
        Ok(())
    }

    pub fn build(self) -> Packages {
        Packages(Arc::new(self.inner))
    }
}

impl Packages {
    pub fn builder() -> PackagesBuilder {
        PackagesBuilder::default()
    }

    /// The role `role_id` names: a built-in one, then an added one. A `None`
    /// must never be treated as "no role" by a caller starting a session
    /// (`roles`' module doc).
    pub fn resolve_role(&self, role_id: &RoleId) -> Option<&'static RoleDefinition> {
        crate::roles::builtin(role_id).or_else(|| {
            self.0
                .roles
                .iter()
                .copied()
                .find(|role| role.id == role_id.0)
        })
    }

    /// The roles a user can start a session with: every known role but the
    /// ones `task` delegates to and the ones nobody attends.
    pub fn user_launchable(&self) -> Vec<&'static RoleDefinition> {
        crate::roles::builtin_roles()
            .iter()
            .copied()
            .chain(self.0.roles.iter().copied())
            .filter(|role| !crate::roles::is_delegated(role.id) && !role.unattended)
            .collect()
    }

    pub(crate) fn skill_sources(&self) -> &[&'static str] {
        &self.0.skill_sources
    }

    pub(crate) fn tools(&self) -> &[ExternalTool] {
        &self.0.tools
    }
}
