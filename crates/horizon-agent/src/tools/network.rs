//! The per-session domain policy: the hosts `web_fetch` may fetch and a
//! contained `bash` call's runtime proxy lets through. It starts with the
//! project's `[[grants.project]] network` domains and grows as domains are
//! approved for the session (`web_fetch`'s `DomainGrant`).

mod allowlist;

pub(crate) use allowlist::Allowlist;
use std::sync::Arc;

/// The always-present per-session domain policy.
#[derive(Clone, Default)]
pub struct SessionDomainPolicy {
    allowlist: Arc<Allowlist>,
}

impl SessionDomainPolicy {
    /// A policy with `domains` already allowed: `horizon-agent-host` seeds it
    /// with the project's configured `network` domains.
    pub fn with_allowed(domains: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowlist: Arc::new(Allowlist::new(domains)),
        }
    }

    pub(crate) fn allow(&self, domain: impl Into<String>) {
        self.allowlist.allow(domain);
    }

    /// Whether `domain` is currently allowed.
    pub fn is_allowed(&self, domain: &str) -> bool {
        self.allowlist.is_allowed(domain)
    }

    /// The allowed domains now, for a contained call's runtime proxy.
    pub(crate) fn domains(&self) -> Vec<String> {
        self.allowlist.domains()
    }

    pub(crate) fn shared(&self) -> Arc<Allowlist> {
        Arc::clone(&self.allowlist)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_allowed_pre_seeds_domains_before_any_runtime_grant() {
        let policy = SessionDomainPolicy::with_allowed(["crates.io"]);
        assert!(policy.is_allowed("crates.io"));
        assert!(!policy.is_allowed("example.com"));

        policy.allow("example.com");

        assert!(policy.is_allowed("example.com"));
        let mut domains = policy.domains();
        domains.sort();
        assert_eq!(domains, ["crates.io", "example.com"]);
    }

    #[test]
    fn clones_share_one_session_policy() {
        let policy = SessionDomainPolicy::default();
        let clone = policy.clone();

        clone.allow("example.com");

        assert!(policy.is_allowed("example.com"));
    }
}
