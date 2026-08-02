//! `[grants]`: per-project filesystem trees a session may write to from
//! the start (`docs/containment-denial-narrow-grants-design.md`'s
//! 2026-07-26 project-scoped-tree-grants decision).
//!
//! ```toml
//! [[grants.project]]
//! root  = "/home/satoshi/src/github.com/rail44/horizon"
//! trees = ["~/.cargo"]
//! loopback_connect = ["127.0.0.1:4226"]
//! ```
//!
//! Three properties of this section are deliberate, not incidental:
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
    /// Loopback TCP endpoints (`ip:port`) a sandboxed session of this project
    /// may connect to directly, alongside the session proxy. Each entry is
    /// an exact `ip:port` pair -- the sandbox matches it by full `SocketAddr`
    /// equality, never by a looser `is_loopback && port` rule (see the
    /// enforcement module's doc comment for the decoy rationale). Only IPv4
    /// loopback addresses (`127.0.0.0/8`) with a non-zero port are accepted;
    /// IPv6 and non-loopback addresses are refused with a warning at parse
    /// time and dropped. sccache on `127.0.0.1:4226` is the intended use
    /// (owner decision 2026-08-02).
    pub loopback_connect: Vec<String>,
}

/// A validated `[[grants.project]]` entry: absolute paths, `~` already
/// expanded, every tree checked against the same over-broad rule the
/// sandbox enforces at spawn time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectGrant {
    pub root: PathBuf,
    pub trees: Vec<PathBuf>,
    /// Validated loopback endpoints, each an IPv4 loopback `ip:port`.
    pub loopback_connect: Vec<SocketAddr>,
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
        for endpoint in &entry.loopback_connect {
            match validate_loopback_endpoint(endpoint) {
                Ok(addr) => {
                    if !loopback_connect.contains(&addr) {
                        loopback_connect.push(addr);
                    }
                }
                Err(reason) => {
                    warnings.push(format!(
                        "[[grants.project]] root {:?}: loopback_connect entry {endpoint:?} -- {reason}, \
                         ignoring it",
                        entry.root
                    ));
                }
            }
        }
        resolved.push(ProjectGrant {
            root,
            trees,
            loopback_connect,
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

/// Expands a leading `~`/`~/` against `home` and requires the result to be
/// absolute -- the same rule the persistence-path overrides
/// (`HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB`) already apply. A
/// `~user` form is deliberately *not* supported: it would need passwd
/// lookups to mean anything, and this file only ever describes the account
/// Horizon runs as.
fn expand(value: &str, home: Option<&Path>) -> Option<PathBuf> {
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

/// Parses and validates one `loopback_connect` entry. Accepts only an IPv4
/// loopback address (`127.0.0.0/8`) with a non-zero port -- the shape the
/// seccomp-notify enforcement layer can proxy-connect (it builds a
/// `sockaddr_in` on a duplicated `AF_INET` socket). IPv6 loopback (`[::1]`)
/// is refused here rather than silently failing at enforcement time, where
/// the `AF_INET` socket-domain check would deny it. Returns a human-readable
/// reason on failure so `resolve` can fold it into its warn-and-ignore
/// warning, matching the over-broad-tree refusal pattern.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &str, trees: &[&str]) -> RawProjectGrant {
        RawProjectGrant {
            root: root.to_string(),
            trees: trees.iter().map(|tree| tree.to_string()).collect(),
            loopback_connect: Vec::new(),
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

    // --- loopback_connect -------------------------------------------------

    fn entry_with_loopback(root: &str, loopback: &[&str]) -> RawProjectGrant {
        RawProjectGrant {
            root: root.to_string(),
            trees: Vec::new(),
            loopback_connect: loopback.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_loopback_endpoint_is_parsed_into_a_socket_addr() {
        let (resolved, warnings) = resolve(
            &[entry_with_loopback("/src/project", &["127.0.0.1:4226"])],
            None,
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
        assert_eq!(
            resolved[0].loopback_connect,
            vec!["127.0.0.1:4226".parse().unwrap()]
        );
    }

    #[test]
    fn a_non_loopback_ipv4_is_refused_with_a_named_warning() {
        let (resolved, warnings) = resolve(
            &[entry_with_loopback("/src/project", &["10.0.0.1:4226"])],
            None,
        );
        assert!(resolved[0].loopback_connect.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("not a loopback"));
        assert!(warnings[0].contains("ignoring it"));
    }

    #[test]
    fn an_ipv6_loopback_is_refused_with_a_named_warning() {
        let (resolved, warnings) = resolve(
            &[entry_with_loopback("/src/project", &["[::1]:4226"])],
            None,
        );
        assert!(resolved[0].loopback_connect.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("IPv6"));
    }

    #[test]
    fn a_zero_port_is_refused() {
        let (resolved, warnings) = resolve(
            &[entry_with_loopback("/src/project", &["127.0.0.1:0"])],
            None,
        );
        assert!(resolved[0].loopback_connect.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("port 0"));
    }

    #[test]
    fn an_unparseable_endpoint_is_refused() {
        let (resolved, warnings) = resolve(
            &[entry_with_loopback("/src/project", &["not-an-address"])],
            None,
        );
        assert!(resolved[0].loopback_connect.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("not a valid ip:port"));
    }

    #[test]
    fn duplicate_loopback_endpoints_collapse() {
        let (resolved, _) = resolve(
            &[entry_with_loopback(
                "/src/project",
                &["127.0.0.1:4226", "127.0.0.1:4226"],
            )],
            None,
        );
        assert_eq!(resolved[0].loopback_connect.len(), 1);
    }

    #[test]
    fn loopback_endpoints_are_looked_up_by_exact_project_root() {
        let (resolved, _) = resolve(
            &[
                entry_with_loopback("/src/project", &["127.0.0.1:4226"]),
                entry_with_loopback("/src/other", &["127.0.0.1:9999"]),
                entry_with_loopback("/src/project", &["127.0.0.2:4226"]),
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
}
