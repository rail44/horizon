//! `[grants]`: per-project filesystem trees and network destinations a
//! session may access from the start
//! (`docs/containment-denial-narrow-grants-design.md`'s 2026-07-26
//! project-scoped-tree-grants decision).
//!
//! ```toml
//! [[grants.project]]
//! root    = "/home/satoshi/src/github.com/rail44/horizon"
//! trees   = ["~/.cargo"]
//! network = ["127.0.0.1:4226", "build-cache.internal"]
//! ```
//!
//! `network` entries are dispatched by shape, not by a separate key
//! (owner decision 2026-08-02, unifying two previously asymmetric surfaces
//! -- see below): one that parses as a `SocketAddr` (an `ip:port` string,
//! e.g. `127.0.0.1:4226`) is validated as a direct-connect endpoint the
//! seccomp-notify enforcement layer lets the sandboxed session reach
//! alongside its own domain proxy (`crates/horizon-sandbox-runtime/src/
//! linux/network.rs`'s `NetworkEnforcement::ProxyOnly`); only IPv4 loopback
//! (`127.0.0.0/8`) with a non-zero port is accepted this way, and anything
//! else shaped like `ip:port` (IPv6, a non-loopback IPv4 address, port `0`)
//! is refused with a warning suggesting a domain name instead. Everything
//! that does NOT parse as a `SocketAddr` is treated as a domain name and
//! pre-seeded into the session's `SessionDomainPolicy` at spawn
//! (`horizon-agentd`'s `session::setup::configured_domains` /
//! `session::run::run_session`), so a project-trusted domain never needs a
//! judge/approval round trip through the session's network proxy -- the
//! runtime approve-on-denial flow still applies on top for anything not
//! listed here. Validation there is a light syntax check only (non-empty,
//! no URL scheme, no `/`, no whitespace); the proxy itself is the real gate
//! at connect time.
//!
//! Before this, the same request ("let this project reach X") lived on two
//! asymmetric surfaces: loopback endpoints had this static `loopback_connect`
//! key (added 2026-08-02, a few hours before this unification) while domains
//! had no config surface at all, only the runtime judge/approval flow.
//! `loopback_connect` is retired outright with no compatibility alias -- it
//! shipped with zero users.
//!
//! Three further properties of this section are deliberate, not incidental:
//!
//! - **It lives in the user's own config file, never in the repository.**
//!   A tracked project file would let a cloned repository widen its own
//!   authority the moment an agent session opened it -- the classic
//!   confused deputy. This is direnv's allow model (the human, outside the
//!   repository, says which project may reach where), not VS Code's
//!   tracked-settings model.
//! - **It is keyed by project, not by machine or by spawn.** Needing
//!   `~/.cargo` is a property of developing *this* project.
//! - **Nothing here is command- or language-specific.** `trees` are plain
//!   paths; no rule in Horizon maps a tool name to the directories it
//!   wants.
//!
//! Validation is warn-and-ignore, like every other section: a refused
//! entry names itself on stderr and drops out, and a file full of bad
//! grants still starts Horizon with no grants rather than failing.
//!
//! This module keeps `horizon_sandbox` out of its own public surface --
//! [`ProjectGrant`] carries plain `PathBuf`s. The one thing it borrows
//! from that crate is the over-broad-tree *rule*
//! ([`horizon_sandbox::is_overbroad_tree`]), so the path a config file is
//! allowed to name and the path the sandbox will actually accept can never
//! drift apart.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `[grants]`: the whole section. Only `project` exists today; the section
/// is a table (rather than `project` being a top-level key) so a future
/// grant flavor can join it without reshaping anything.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawGrantsConfig {
    /// `[[grants.project]]`, in file order.
    pub project: Vec<RawProjectGrant>,
}

/// One `[[grants.project]]` entry, exactly as written in the file --
/// unexpanded and unvalidated. [`resolve`] turns a list of these into
/// [`ProjectGrant`]s plus the warnings for whatever it refused.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawProjectGrant {
    /// The project's main-repository toplevel. A session working in a
    /// derived worktree of this repository resolves back to this root --
    /// see `horizon-agentd`'s `worktree::project_root`.
    pub root: String,
    /// Directories granted read-write, as whole trees, to every session of
    /// this project.
    pub trees: Vec<String>,
    /// Network destinations a sandboxed session of this project may reach
    /// beyond the empty runtime-approval default, dispatched by shape (see
    /// the module doc): an `ip:port` string is validated as a direct-connect
    /// endpoint (sccache on `127.0.0.1:4226` is the intended use, owner
    /// decision 2026-08-02), anything else as a domain name pre-seeded into
    /// the session's network-proxy allowlist. Replaces the short-lived
    /// `loopback_connect` key with no compatibility alias.
    pub network: Vec<String>,
}

/// A validated `[[grants.project]]` entry: absolute paths, `~` already
/// expanded, every tree checked against the same over-broad rule the
/// sandbox enforces at spawn time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectGrant {
    pub root: PathBuf,
    pub trees: Vec<PathBuf>,
    /// Validated direct-connect endpoints dispatched from `network`, each an
    /// IPv4 loopback `ip:port`.
    pub loopback_connect: Vec<SocketAddr>,
    /// Validated domain names dispatched from `network`, pre-seeded into a
    /// spawned session's `SessionDomainPolicy`.
    pub domains: Vec<String>,
}

/// Expands and validates every `[[grants.project]]` entry, returning the
/// usable ones plus one warning string per refusal.
///
/// Pure in `home` so it can be tested without touching the process
/// environment (and so validating a config file that names another
/// account's paths stays predictable). `None` means no `$HOME` is
/// available: a `~` path cannot be expanded and is refused, but everything
/// absolute still validates.
pub fn resolve(
    entries: &[RawProjectGrant],
    home: Option<&Path>,
) -> (Vec<ProjectGrant>, Vec<String>) {
    let mut resolved = Vec::new();
    let mut warnings = Vec::new();

    for entry in entries {
        let Some(root) = expand(&entry.root, home) else {
            warnings.push(format!(
                "[[grants.project]]: root {:?} is not an absolute path (and no $HOME is set to \
                 expand a leading \"~/\" against), ignoring this entry",
                entry.root
            ));
            continue;
        };
        let mut trees = Vec::new();
        for tree in &entry.trees {
            let Some(tree_path) = expand(tree, home) else {
                warnings.push(format!(
                    "[[grants.project]] root {:?}: tree {tree:?} is not an absolute path (and no \
                     $HOME is set to expand a leading \"~/\" against), ignoring it",
                    entry.root
                ));
                continue;
            };
            if horizon_sandbox::is_overbroad_tree(&tree_path, home) {
                warnings.push(format!(
                    "[[grants.project]] root {:?}: tree {tree:?} resolves to {}, which is the \
                     filesystem root, your home directory, or a system directory -- refusing to \
                     grant it, ignoring it",
                    entry.root,
                    tree_path.display()
                ));
                continue;
            }
            if !trees.contains(&tree_path) {
                trees.push(tree_path);
            }
        }
        let mut loopback_connect = Vec::new();
        let mut domains = Vec::new();
        for value in &entry.network {
            let trimmed = value.trim();
            if trimmed.parse::<SocketAddr>().is_ok() {
                match validate_loopback_endpoint(trimmed) {
                    Ok(addr) => {
                        if !loopback_connect.contains(&addr) {
                            loopback_connect.push(addr);
                        }
                    }
                    Err(reason) => {
                        warnings.push(format!(
                            "[[grants.project]] root {:?}: network entry {value:?} -- {reason}; an \
                             external host should be written as a bare domain name instead, which \
                             is routed through the session's network proxy, ignoring it",
                            entry.root
                        ));
                    }
                }
            } else {
                match validate_domain_entry(trimmed) {
                    Ok(domain) => {
                        if !domains.contains(&domain) {
                            domains.push(domain);
                        }
                    }
                    Err(reason) => {
                        warnings.push(format!(
                            "[[grants.project]] root {:?}: network entry {value:?} -- {reason}, \
                             ignoring it",
                            entry.root
                        ));
                    }
                }
            }
        }
        resolved.push(ProjectGrant {
            root,
            trees,
            loopback_connect,
            domains,
        });
    }

    (resolved, warnings)
}

/// The trees granted to a session whose project root is `project_root`.
/// Entries are matched by exact root path (both sides already canonical in
/// production: the config's own expansion here, and the git-resolved
/// repository toplevel on the session side). Several entries naming the
/// same root contribute all of their trees.
pub fn trees_for_project(entries: &[ProjectGrant], project_root: &Path) -> Vec<PathBuf> {
    let mut trees = Vec::new();
    for entry in entries {
        if entry.root != project_root {
            continue;
        }
        for tree in &entry.trees {
            if !trees.contains(tree) {
                trees.push(tree.clone());
            }
        }
    }
    trees
}

/// The loopback endpoints granted to a session whose project root is
/// `project_root`. Entries are matched by exact root path, same as
/// [`trees_for_project`]. Several entries naming the same root contribute
/// all of their endpoints.
pub fn loopback_connect_for_project(
    entries: &[ProjectGrant],
    project_root: &Path,
) -> Vec<SocketAddr> {
    let mut endpoints = Vec::new();
    for entry in entries {
        if entry.root != project_root {
            continue;
        }
        for addr in &entry.loopback_connect {
            if !endpoints.contains(addr) {
                endpoints.push(*addr);
            }
        }
    }
    endpoints
}

/// The domain names granted to a session whose project root is
/// `project_root`, dispatched from `network` entries that did not parse as
/// an `ip:port` endpoint. Same exact-root matching as
/// [`trees_for_project`]/[`loopback_connect_for_project`]; several entries
/// naming the same root contribute all of their domains.
/// `horizon-agentd`'s `session::setup::configured_domains` calls this at
/// session spawn to pre-seed the session's `SessionDomainPolicy`.
pub fn domains_for_project(entries: &[ProjectGrant], project_root: &Path) -> Vec<String> {
    let mut domains = Vec::new();
    for entry in entries {
        if entry.root != project_root {
            continue;
        }
        for domain in &entry.domains {
            if !domains.contains(domain) {
                domains.push(domain.clone());
            }
        }
    }
    domains
}

/// Expands a leading `~`/`~/` against `home` and requires the result to be
/// absolute -- the same rule the persistence-path overrides
/// (`HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB`) already apply. A
/// `~user` form is deliberately *not* supported: it would need passwd
/// lookups to mean anything, and this file only ever describes the account
/// Horizon runs as.
pub(crate) fn expand(value: &str, home: Option<&Path>) -> Option<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let expanded = if trimmed == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home?.join(rest)
    } else {
        PathBuf::from(trimmed)
    };
    expanded.is_absolute().then_some(expanded)
}

/// Parses and validates one `network` entry that already parsed as a
/// `SocketAddr` (the direct-connect dispatch branch, see the module doc).
/// Accepts only an IPv4 loopback address (`127.0.0.0/8`) with a non-zero
/// port -- the shape the seccomp-notify enforcement layer can proxy-connect
/// (it builds a `sockaddr_in` on a duplicated `AF_INET` socket). IPv6
/// loopback (`[::1]`) is refused here rather than silently failing at
/// enforcement time, where the `AF_INET` socket-domain check would deny it.
/// Returns a human-readable reason on failure so `resolve` can fold it into
/// its warn-and-ignore warning, matching the over-broad-tree refusal
/// pattern.
fn validate_loopback_endpoint(value: &str) -> Result<SocketAddr, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("empty endpoint".to_string());
    }
    let addr: SocketAddr = trimmed
        .parse()
        .map_err(|_| format!("{trimmed:?} is not a valid ip:port"))?;
    match addr {
        SocketAddr::V4(v4) if v4.ip().is_loopback() && v4.port() != 0 => Ok(SocketAddr::V4(v4)),
        SocketAddr::V4(v4) if v4.port() == 0 => {
            Err(format!("{addr} has port 0; a non-zero port is required"))
        }
        SocketAddr::V4(_) => Err(format!(
            "{addr} is not a loopback address (only 127.0.0.0/8 is accepted)"
        )),
        SocketAddr::V6(_) => Err(format!(
            "{addr} is IPv6; only IPv4 loopback is supported (the enforcement layer \
             proxy-connects on an AF_INET socket)"
        )),
    }
}

/// Parses and validates one `network` entry that did NOT parse as a
/// `SocketAddr` -- the domain-name dispatch branch (see the module doc).
/// Only a light syntax check: reject anything empty, carrying a URL scheme,
/// a path separator, or whitespace, since none of those is a bare domain
/// name a proxy `CONNECT` target could be. The proxy itself, not this
/// function, is the real gate at connect time.
fn validate_domain_entry(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("empty entry".to_string());
    }
    if trimmed.contains("://") {
        return Err(format!(
            "{trimmed:?} looks like a URL; write a bare domain name with no scheme"
        ));
    }
    if trimmed.contains('/') {
        return Err(format!(
            "{trimmed:?} contains '/'; write a bare domain name"
        ));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(format!(
            "{trimmed:?} contains whitespace; write a bare domain name"
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &str, trees: &[&str]) -> RawProjectGrant {
        RawProjectGrant {
            root: root.to_string(),
            trees: trees.iter().map(|tree| tree.to_string()).collect(),
            network: Vec::new(),
        }
    }

    #[test]
    fn a_tilde_tree_expands_against_home() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(&[entry("/src/project", &["~/.cargo"])], Some(&home));

        assert!(warnings.is_empty(), "warnings = {warnings:?}");
        assert_eq!(
            resolved,
            vec![ProjectGrant {
                root: PathBuf::from("/src/project"),
                trees: vec![PathBuf::from("/home/someone/.cargo")],
                loopback_connect: Vec::new(),
                domains: Vec::new(),
            }]
        );
    }

    #[test]
    fn a_tilde_root_expands_against_home_too() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(
            &[entry("~/src/project", &["~/.cache/project"])],
            Some(&home),
        );

        assert_eq!(resolved[0].root, PathBuf::from("/home/someone/src/project"));
        assert_eq!(
            resolved[0].trees,
            vec![PathBuf::from("/home/someone/.cache/project")]
        );
    }

    #[test]
    fn home_itself_is_refused_with_a_named_warning() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(&[entry("/src/project", &["~"])], Some(&home));

        assert!(resolved[0].trees.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("/home/someone"));
        assert!(warnings[0].contains("refusing to grant it"));
    }

    #[test]
    fn the_filesystem_root_and_system_directories_are_refused() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(
            &[entry("/src/project", &["/", "/usr", "/etc", "~/.cargo"])],
            Some(&home),
        );

        assert_eq!(
            resolved[0].trees,
            vec![PathBuf::from("/home/someone/.cargo")],
            "only the sound tree survives"
        );
        assert_eq!(warnings.len(), 3, "warnings = {warnings:?}");
        assert!(warnings.iter().all(|w| w.contains("refusing to grant it")));
    }

    #[test]
    fn a_relative_tree_is_refused() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) =
            resolve(&[entry("/src/project", &["relative/dir"])], Some(&home));

        assert!(resolved[0].trees.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("not an absolute path"));
    }

    #[test]
    fn a_relative_root_drops_the_whole_entry() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(&[entry("project", &["~/.cargo"])], Some(&home));

        assert!(resolved.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("ignoring this entry"));
    }

    #[test]
    fn a_tilde_path_without_home_is_refused_rather_than_guessed() {
        let (resolved, warnings) = resolve(&[entry("/src/project", &["~/.cargo"])], None);

        assert!(resolved[0].trees.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
    }

    #[test]
    fn duplicate_trees_collapse() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(
            &[entry("/src/project", &["~/.cargo", "/home/someone/.cargo"])],
            Some(&home),
        );

        assert_eq!(resolved[0].trees.len(), 1);
    }

    #[test]
    fn trees_are_looked_up_by_exact_project_root() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(
            &[
                entry("/src/project", &["~/.cargo"]),
                entry("/src/other", &["~/.other-cache"]),
                entry("/src/project", &["~/.extra"]),
            ],
            Some(&home),
        );

        assert_eq!(
            trees_for_project(&resolved, Path::new("/src/project")),
            vec![
                PathBuf::from("/home/someone/.cargo"),
                PathBuf::from("/home/someone/.extra"),
            ],
            "every entry naming this root contributes"
        );
        assert_eq!(
            trees_for_project(&resolved, Path::new("/src/other")),
            vec![PathBuf::from("/home/someone/.other-cache")]
        );
        assert!(trees_for_project(&resolved, Path::new("/src/unlisted")).is_empty());
    }

    #[test]
    fn a_subdirectory_of_a_listed_root_is_not_a_match() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(&[entry("/src/project", &["~/.cargo"])], Some(&home));

        assert!(
            trees_for_project(&resolved, Path::new("/src/project/crates/inner")).is_empty(),
            "a grant is keyed by the project root itself, not by containment"
        );
    }

    // --- network: direct-connect endpoints (ip:port-shaped entries) -------

    fn entry_with_network(root: &str, network: &[&str]) -> RawProjectGrant {
        RawProjectGrant {
            root: root.to_string(),
            trees: Vec::new(),
            network: network.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn an_ip_port_entry_is_parsed_into_a_direct_connect_socket_addr() {
        let (resolved, warnings) = resolve(
            &[entry_with_network("/src/project", &["127.0.0.1:4226"])],
            None,
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
        assert_eq!(
            resolved[0].loopback_connect,
            vec!["127.0.0.1:4226".parse().unwrap()]
        );
        assert!(resolved[0].domains.is_empty());
    }

    #[test]
    fn a_non_loopback_ip_port_entry_is_refused_with_a_named_warning() {
        let (resolved, warnings) = resolve(
            &[entry_with_network("/src/project", &["10.0.0.1:4226"])],
            None,
        );
        assert!(resolved[0].loopback_connect.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("not a loopback"));
        assert!(warnings[0].contains("bare domain name"));
    }

    #[test]
    fn an_ipv6_ip_port_entry_is_refused_with_a_named_warning() {
        let (resolved, warnings) =
            resolve(&[entry_with_network("/src/project", &["[::1]:4226"])], None);
        assert!(resolved[0].loopback_connect.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("IPv6"));
    }

    #[test]
    fn a_zero_port_entry_is_refused() {
        let (resolved, warnings) = resolve(
            &[entry_with_network("/src/project", &["127.0.0.1:0"])],
            None,
        );
        assert!(resolved[0].loopback_connect.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("port 0"));
    }

    #[test]
    fn duplicate_direct_connect_entries_collapse() {
        let (resolved, _) = resolve(
            &[entry_with_network(
                "/src/project",
                &["127.0.0.1:4226", "127.0.0.1:4226"],
            )],
            None,
        );
        assert_eq!(resolved[0].loopback_connect.len(), 1);
    }

    #[test]
    fn direct_connect_endpoints_are_looked_up_by_exact_project_root() {
        let (resolved, _) = resolve(
            &[
                entry_with_network("/src/project", &["127.0.0.1:4226"]),
                entry_with_network("/src/other", &["127.0.0.1:9999"]),
                entry_with_network("/src/project", &["127.0.0.2:4226"]),
            ],
            None,
        );
        assert_eq!(
            loopback_connect_for_project(&resolved, Path::new("/src/project")),
            vec![
                "127.0.0.1:4226".parse().unwrap(),
                "127.0.0.2:4226".parse().unwrap()
            ],
            "every entry naming this root contributes"
        );
        assert!(loopback_connect_for_project(&resolved, Path::new("/src/unlisted")).is_empty());
    }

    // --- network: domain names (anything that isn't ip:port-shaped) -------

    #[test]
    fn a_bare_domain_entry_is_dispatched_as_a_domain() {
        let (resolved, warnings) = resolve(
            &[entry_with_network(
                "/src/project",
                &["build-cache.internal"],
            )],
            None,
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
        assert_eq!(
            resolved[0].domains,
            vec!["build-cache.internal".to_string()]
        );
        assert!(resolved[0].loopback_connect.is_empty());
    }

    #[test]
    fn a_value_that_is_neither_ip_port_nor_a_real_domain_still_dispatches_as_one() {
        // The dispatch rule is purely "does it parse as a `SocketAddr`" --
        // this deliberately accepts domain-shaped strings that aren't valid
        // DNS names either; the proxy is the real gate at connect time (see
        // the module doc).
        let (resolved, warnings) = resolve(
            &[entry_with_network("/src/project", &["not-an-address"])],
            None,
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
        assert_eq!(resolved[0].domains, vec!["not-an-address".to_string()]);
    }

    #[test]
    fn a_domain_entry_with_a_url_scheme_is_refused() {
        let (resolved, warnings) = resolve(
            &[entry_with_network("/src/project", &["https://example.com"])],
            None,
        );
        assert!(resolved[0].domains.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("URL"));
    }

    #[test]
    fn a_domain_entry_with_a_path_is_refused() {
        let (resolved, warnings) = resolve(
            &[entry_with_network("/src/project", &["example.com/path"])],
            None,
        );
        assert!(resolved[0].domains.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains('/'));
    }

    #[test]
    fn a_domain_entry_with_whitespace_is_refused() {
        let (resolved, warnings) = resolve(
            &[entry_with_network("/src/project", &["exa mple.com"])],
            None,
        );
        assert!(resolved[0].domains.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("whitespace"));
    }

    #[test]
    fn duplicate_domain_entries_collapse() {
        let (resolved, _) = resolve(
            &[entry_with_network(
                "/src/project",
                &["build-cache.internal", "build-cache.internal"],
            )],
            None,
        );
        assert_eq!(resolved[0].domains.len(), 1);
    }

    #[test]
    fn domains_are_looked_up_by_exact_project_root() {
        let (resolved, _) = resolve(
            &[
                entry_with_network("/src/project", &["a.internal"]),
                entry_with_network("/src/other", &["b.internal"]),
                entry_with_network("/src/project", &["c.internal"]),
            ],
            None,
        );
        assert_eq!(
            domains_for_project(&resolved, Path::new("/src/project")),
            vec!["a.internal".to_string(), "c.internal".to_string()],
            "every entry naming this root contributes"
        );
        assert!(domains_for_project(&resolved, Path::new("/src/unlisted")).is_empty());
    }

    #[test]
    fn a_mixed_network_list_dispatches_each_entry_by_its_own_shape() {
        let (resolved, warnings) = resolve(
            &[entry_with_network(
                "/src/project",
                &["127.0.0.1:4226", "build-cache.internal"],
            )],
            None,
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
        assert_eq!(
            resolved[0].loopback_connect,
            vec!["127.0.0.1:4226".parse().unwrap()]
        );
        assert_eq!(
            resolved[0].domains,
            vec!["build-cache.internal".to_string()]
        );
    }
}
