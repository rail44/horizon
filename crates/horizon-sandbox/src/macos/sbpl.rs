//! SBPL (Seatbelt Profile Language) profile composition for
//! `/usr/bin/sandbox-exec`. Pure string building -- no macOS APIs -- so
//! it's unit-testable on any host; see module docs on `macos::mod` for why
//! this backend's *runtime* behavior can't be verified from this (Linux)
//! development machine.
//!
//! The three vendored templates under `vendor/` (Codex's own
//! `seatbelt_base_policy.sbpl`, `seatbelt_network_policy.sbpl`, and
//! `restricted_read_only_platform_defaults.sbpl` -- Apache-2.0, see
//! `NOTICE`) supply the deny-by-default baseline and the "how does a
//! process even exec anything" platform rules; this module only adds the
//! policy-specific rules on top (`writable_roots`, `readable_scope`).

use crate::policy::{NetworkPolicy, ReadableScope, SandboxPolicy};
use std::path::Path;

const BASE_POLICY: &str = include_str!("vendor/seatbelt_base_policy.sbpl");
const NETWORK_POLICY: &str = include_str!("vendor/seatbelt_network_policy.sbpl");
const PLATFORM_DEFAULTS: &str = include_str!("vendor/restricted_read_only_platform_defaults.sbpl");

/// Composes the full SBPL profile text for `policy`.
pub(crate) fn compose(policy: &SandboxPolicy) -> String {
    let mut profile = String::new();
    profile.push_str(BASE_POLICY);
    profile.push('\n');

    if policy.network == NetworkPolicy::Enabled {
        profile.push_str(NETWORK_POLICY);
        profile.push('\n');
    }

    // Baseline platform read access (dylib loading, /etc, /tmp, /dev,
    // standard system paths) so the sandboxed process can exec and run at
    // all, regardless of readable_scope.
    profile.push_str(PLATFORM_DEFAULTS);
    profile.push('\n');

    profile.push_str("; --- horizon-sandbox policy rules (generated, not vendored) ---\n");

    match &policy.readable_scope {
        ReadableScope::Full => {
            profile.push_str(&allow_rule("file-read*", Path::new("/")));
        }
        ReadableScope::Roots(roots) => {
            for root in roots {
                profile.push_str(&allow_rule("file-read*", root));
            }
        }
    }

    for root in &policy.writable_roots {
        profile.push_str(&allow_rule("file-read* file-write*", root));
    }

    profile
}

fn allow_rule(ops: &str, path: &Path) -> String {
    format!("(allow {ops} (subpath {}))\n", sbpl_string_literal(path))
}

/// Renders `path` as an SBPL string literal, escaping backslashes and
/// double quotes.
fn sbpl_string_literal(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(writable_roots: Vec<std::path::PathBuf>, network: NetworkPolicy) -> SandboxPolicy {
        SandboxPolicy {
            writable_roots,
            readable_scope: ReadableScope::Full,
            network,
        }
    }

    #[test]
    fn includes_vendored_base_policy() {
        let profile = compose(&policy(vec![], NetworkPolicy::Disabled));
        assert!(profile.contains("(deny default)"));
    }

    #[test]
    fn network_disabled_omits_network_fragment() {
        let profile = compose(&policy(vec![], NetworkPolicy::Disabled));
        assert!(!profile.contains("system-socket"));
    }

    #[test]
    fn network_enabled_includes_network_fragment() {
        let profile = compose(&policy(vec![], NetworkPolicy::Enabled));
        assert!(profile.contains("system-socket"));
    }

    #[test]
    fn full_scope_allows_root_read() {
        let profile = compose(&policy(vec![], NetworkPolicy::Disabled));
        assert!(profile.contains("(allow file-read* (subpath \"/\"))"));
    }

    #[test]
    fn roots_scope_allows_only_listed_paths() {
        let mut p = policy(vec![], NetworkPolicy::Disabled);
        p.readable_scope = ReadableScope::Roots(vec!["/opt/tool".into()]);
        let profile = compose(&p);
        assert!(profile.contains("(allow file-read* (subpath \"/opt/tool\"))"));
        assert!(!profile.contains("(allow file-read* (subpath \"/\"))"));
    }

    #[test]
    fn writable_root_allows_read_and_write() {
        let profile = compose(&policy(
            vec!["/tmp/session-root".into()],
            NetworkPolicy::Disabled,
        ));
        assert!(profile.contains("(allow file-read* file-write* (subpath \"/tmp/session-root\"))"));
    }

    #[test]
    fn path_with_quote_is_escaped() {
        let literal = sbpl_string_literal(Path::new("/tmp/weird\"name"));
        assert_eq!(literal, "\"/tmp/weird\\\"name\"");
    }
}
