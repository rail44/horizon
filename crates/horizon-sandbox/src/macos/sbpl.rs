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

/// The vendored `PLATFORM_DEFAULTS` template (unmodified, per its own header
/// comment) includes a blanket "scratch space so tools can create temp
/// files" grant: `(allow file-read* file-write* (subpath "/tmp"))` and the
/// same for its aliases. That is a real, host-shared temp dir on macOS
/// (`/tmp` is a symlink to `/private/tmp`) -- exactly the containment hole a
/// 2026-07-19 dogfooding session found on Linux (see
/// `tools::bash::exec::run_sandboxed`'s doc comment in `horizon-agent`),
/// just baked into the vendored baseline instead of a caller-supplied
/// policy. Seatbelt SBPL evaluates rules in order and the *last* matching
/// rule for a given path/operation wins, so these `deny file-write*` rules,
/// emitted right after `PLATFORM_DEFAULTS` and before the caller's own
/// `writable_roots` loop, override that blanket grant while leaving it a
/// no-op for any `writable_roots` entry that happens to live under one of
/// these paths -- the loop's own `allow` comes later still and wins back.
/// Read access is untouched (`ReadableScope::Full`'s own rule, not this one,
/// governs that) -- only the *write* allowance is a containment hole.
const HOST_SHARED_TMP_DIRS: [&str; 4] = ["/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp"];

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

    // Negate PLATFORM_DEFAULTS's blanket host-shared-/tmp write allowance --
    // see HOST_SHARED_TMP_DIRS's doc comment. Must come before the
    // `writable_roots` loop below so an explicit writable root under one of
    // these paths still wins (last matching rule wins in SBPL).
    for dir in HOST_SHARED_TMP_DIRS {
        profile.push_str(&deny_rule("file-write*", Path::new(dir)));
    }

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

fn deny_rule(ops: &str, path: &Path) -> String {
    format!("(deny {ops} (subpath {}))\n", sbpl_string_literal(path))
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

    /// Containment fix (2026-07-19 dogfooding, mirrored from the Linux
    /// `--tmpfs /tmp` fix): the vendored `PLATFORM_DEFAULTS` template grants
    /// blanket write access to the host's shared `/tmp` and its aliases
    /// (`(allow file-read* file-write* (subpath "/tmp"))` etc. -- see the
    /// vendored file's own "Scratch space" comment). The generated policy
    /// must deny that write allowance for every writable-root-free policy,
    /// same "no blanket host temp-dir write" semantics `run_sandboxed` now
    /// enforces on Linux.
    #[test]
    fn blanket_host_tmp_write_from_platform_defaults_is_denied() {
        let profile = compose(&policy(vec![], NetworkPolicy::Disabled));
        for dir in ["/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp"] {
            assert!(
                profile.contains(&format!("(deny file-write* (subpath \"{dir}\"))")),
                "expected a deny rule for {dir}, profile:\n{profile}"
            );
        }
        // The vendored template's own blanket allow is still present
        // (unmodified, per its header) -- this proves the fix works by
        // *overriding* it with a later rule, not by editing the vendored
        // file.
        assert!(profile.contains("Scratch space so tools can create temp files"));
    }

    /// SBPL's last-matching-rule-wins semantics mean the deny above only
    /// actually contains the hole if it is emitted *before* the
    /// caller's own `writable_roots` allow -- otherwise an explicit
    /// writable root nested under `/tmp` (a realistic shape: a session's
    /// isolated worktree living under the OS temp dir) would itself get
    /// silently denied by this fix. Assert the ordering directly rather
    /// than just the rules' presence.
    #[test]
    fn explicit_writable_root_under_tmp_still_overrides_the_deny() {
        let profile = compose(&policy(
            vec!["/tmp/session-root".into()],
            NetworkPolicy::Disabled,
        ));
        let deny_pos = profile
            .find("(deny file-write* (subpath \"/tmp\"))")
            .expect("deny rule for /tmp should be present");
        let allow_pos = profile
            .find("(allow file-read* file-write* (subpath \"/tmp/session-root\"))")
            .expect("explicit writable root should still be allowed");
        assert!(
            deny_pos < allow_pos,
            "the blanket deny must come before the caller's explicit \
             writable-root allow so the more specific, later rule wins: \
             deny at {deny_pos}, allow at {allow_pos}"
        );
    }
}
