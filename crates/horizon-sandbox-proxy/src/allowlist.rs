//! The allowlist policy input (`docs/agent-approval-design.md`: "a plain
//! constructor-provided allowlist
//! is the spike surface"). Leg 4b (per-session domain approval, `horizon-
//! agent`'s `tools::network::SessionNetworkProxy`) needs it to grow at
//! runtime too -- a session's allowlist starts empty and gains entries as
//! the user approves domains one at a time -- so the domain set is behind a
//! `RwLock` rather than plain owned data; the match rule itself is
//! unchanged.

use std::collections::HashSet;
use std::sync::RwLock;

/// A set of allowed hosts, matched exactly and case-insensitively. An empty
/// allowlist allows nothing -- the default
/// posture the design doc calls for until a domain is explicitly approved
/// (`docs/agent-approval-design.md`: "Default allowlist is EMPTY").
#[derive(Debug, Default)]
pub struct Allowlist {
    domains: RwLock<HashSet<String>>,
}

impl Allowlist {
    /// Builds an allowlist from a set of domains. Each is lowercased and
    /// has any trailing `.` stripped so `Example.COM` and `example.com.`
    /// match the same entries; matching itself is done the same way (see
    /// [`Allowlist::is_allowed`]).
    pub fn new(domains: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            domains: RwLock::new(domains.into_iter().map(|d| normalize(&d.into())).collect()),
        }
    }

    /// Whether `host` is an exact match for an allowed host. A grant for
    /// `example.com` deliberately does not cover `api.example.com`: every
    /// distinct destination must be observed and approved independently.
    pub fn is_allowed(&self, host: &str) -> bool {
        let host = normalize(host);
        let domains = self
            .domains
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        domains.contains(&host)
    }

    /// Adds `domain` to this allowlist at runtime -- the per-session
    /// domain-approval mutation leg 4b needs (`docs/agent-approval-
    /// design.md`): approving a denied domain for a session adds it here so
    /// the retried call (and every later call in the same session) can
    /// reach it, with no effect on any other session's own `Allowlist`
    /// instance. Normalized the same way [`Allowlist::new`]'s entries are.
    pub fn allow(&self, domain: impl Into<String>) {
        let mut domains = self
            .domains
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        domains.insert(normalize(&domain.into()));
    }
}

fn normalize(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_allows_nothing() {
        let allowlist = Allowlist::new(Vec::<String>::new());
        assert!(!allowlist.is_allowed("example.com"));
    }

    #[test]
    fn exact_match_is_allowed() {
        let allowlist = Allowlist::new(["example.com"]);
        assert!(allowlist.is_allowed("example.com"));
    }

    #[test]
    fn different_host_is_denied() {
        let allowlist = Allowlist::new(["example.com"]);
        assert!(!allowlist.is_allowed("example.org"));
        assert!(!allowlist.is_allowed("notexample.com"));
    }

    #[test]
    fn subdomain_requires_its_own_exact_grant() {
        let allowlist = Allowlist::new(["example.com"]);
        assert!(!allowlist.is_allowed("api.example.com"));
        assert!(!allowlist.is_allowed("deep.nested.example.com"));
    }

    #[test]
    fn match_is_case_insensitive() {
        let allowlist = Allowlist::new(["Example.COM"]);
        assert!(allowlist.is_allowed("EXAMPLE.com"));
        assert!(!allowlist.is_allowed("api.example.COM"));
    }

    #[test]
    fn trailing_dot_is_ignored_on_both_sides() {
        let allowlist = Allowlist::new(["example.com."]);
        assert!(allowlist.is_allowed("example.com"));
        assert!(allowlist.is_allowed("example.com."));
    }

    #[test]
    fn loopback_ip_matches_as_a_plain_exact_string() {
        // Loopback IPs (used by this crate's own hermetic tests to stand in
        // for distinct hosts) have no subdomain structure -- they're just
        // exact strings as far as this matcher is concerned.
        let allowlist = Allowlist::new(["127.0.0.2"]);
        assert!(allowlist.is_allowed("127.0.0.2"));
        assert!(!allowlist.is_allowed("127.0.0.3"));
    }
}
