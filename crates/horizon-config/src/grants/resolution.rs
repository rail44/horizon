//! Resolve each authority category in config order, retaining refusal diagnostics.

use super::{
    expand, validate_domain_entry, validate_loopback_endpoint, ProjectGrant, RawProjectGrant,
};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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
        let trees = resolve_trees(entry, home, &mut warnings);
        let (loopback_connect, domains) = resolve_network(entry, &mut warnings);
        let mach_services = resolve_mach_services(entry, &mut warnings);
        resolved.push(ProjectGrant {
            root,
            trees,
            loopback_connect,
            domains,
            mach_services,
        });
    }

    (resolved, warnings)
}

fn resolve_trees(
    entry: &RawProjectGrant,
    home: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Vec<PathBuf> {
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
    trees
}

fn resolve_network(
    entry: &RawProjectGrant,
    warnings: &mut Vec<String>,
) -> (Vec<SocketAddr>, Vec<String>) {
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
    (loopback_connect, domains)
}

fn resolve_mach_services(entry: &RawProjectGrant, warnings: &mut Vec<String>) -> Vec<String> {
    let mut mach_services = Vec::new();
    for service in &entry.mach_services {
        if !horizon_sandbox::KNOWN_SECURITY_SERVICES.contains(&service.as_str()) {
            warnings.push(format!(
                "[[grants.project]] root {:?}: mach_services entry {service:?} is not one \
                 of the macOS security services the sandbox knows about ({}), ignoring it",
                entry.root,
                horizon_sandbox::KNOWN_SECURITY_SERVICES.join(", ")
            ));
            continue;
        }
        if !mach_services.contains(service) {
            mach_services.push(service.clone());
        }
    }
    mach_services
}
