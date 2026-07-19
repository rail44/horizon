//! The allowlist policy input (`docs/agent-approval-design.md`: "a plain
//! constructor-provided allowlist (Vec of domains, exact + subdomain match)
//! is the spike surface"). Config-file surfacing and one-time-approval UX
//! are later legs; this type is deliberately just data plus a match rule.

/// A set of allowed hosts, matched by exact name or as a subdomain,
/// case-insensitively. An empty allowlist allows nothing -- the default
/// posture the design doc calls for until the judge leg exists (`docs/
/// agent-approval-design.md`: "Default allowlist is EMPTY").
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    domains: Vec<String>,
}

impl Allowlist {
    /// Builds an allowlist from a set of domains. Each is lowercased and
    /// has any trailing `.` stripped so `Example.COM` and `example.com.`
    /// match the same entries; matching itself is done the same way (see
    /// [`Allowlist::is_allowed`]).
    pub fn new(domains: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            domains: domains.into_iter().map(|d| normalize(&d.into())).collect(),
        }
    }

    /// Whether `host` is covered by this allowlist: an exact match against
    /// one of the configured domains, or a subdomain of one (`api.example.
    /// com` matches an `example.com` entry; `notexample.com` does not).
    pub fn is_allowed(&self, host: &str) -> bool {
        let host = normalize(host);
        self.domains
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
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
    fn subdomain_is_allowed() {
        let allowlist = Allowlist::new(["example.com"]);
        assert!(allowlist.is_allowed("api.example.com"));
        assert!(allowlist.is_allowed("deep.nested.example.com"));
    }

    #[test]
    fn match_is_case_insensitive() {
        let allowlist = Allowlist::new(["Example.COM"]);
        assert!(allowlist.is_allowed("EXAMPLE.com"));
        assert!(allowlist.is_allowed("api.example.COM"));
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
