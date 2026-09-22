//! Command environment and authority assembled before spawning a sandbox.
use super::super::wrapped_script;
use crate::tools::network::SessionNetworkProxy;
use std::path::{Path, PathBuf};

pub(super) struct PreparedCommand {
    pub(super) command: std::process::Command,
    pub(super) policy: horizon_sandbox::SandboxPolicy,
    pub(super) grants: Vec<horizon_sandbox::FilesystemGrant>,
}

pub(super) fn prepare(
    command: &str,
    cwd: &Path,
    workspace_root: &Path,
    network: Option<&SessionNetworkProxy>,
    loopback_connect: &[std::net::SocketAddr],
    filesystem_grants: &[horizon_sandbox::FilesystemGrant],
) -> PreparedCommand {
    // Same wrapper shape as the unsandboxed path (`wrapped_script`): merges
    // the command's own stdout+stderr, and reports the final `$PWD` on the
    // wrapper's own stderr afterward so cwd tracking keeps working across
    // sandboxed calls too. The command's own stderr ends up in this
    // wrapper's stdout via that merge and remains visible in the result.
    let script = wrapped_script(command);
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-c").arg(&script).current_dir(cwd);
    // A parent Git process (notably this repository's pre-commit hook) can
    // export repository-routing variables. They must not redirect the
    // sandboxed command away from the workspace whose metadata roots Horizon
    // validated and displayed for approval.
    for key in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_DIR",
        "GIT_GRAFT_FILE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_INTERNAL_SUPER_PREFIX",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_WORK_TREE",
    ] {
        cmd.env_remove(key);
    }
    // Read-only Git commands such as `status` may otherwise refresh the
    // index as a performance optimization. The metadata classifier keeps
    // those commands in tier 1, so suppress optional locks/writes while
    // leaving locks required by approved mutating operations intact.
    cmd.env("GIT_OPTIONAL_LOCKS", "0");

    let network_policy = match network.map(SessionNetworkProxy::proxy_addr) {
        Some(proxy_addr) => horizon_sandbox::NetworkPolicy::Proxied {
            proxy_addr,
            loopback_connect: loopback_connect.to_vec(),
            unix_socket_connect: unix_socket_connect_grants(
                std::env::var_os("SSH_AUTH_SOCK").as_deref(),
                std::env::var_os("HOME").as_deref().map(Path::new),
            ),
        },
        None => horizon_sandbox::NetworkPolicy::Disabled,
    };
    if let Some(network) = network {
        configure_proxy_environment(&mut cmd, &network.proxy_url());
        // macOS only: the tunnel rides BSD nc's CONNECT support; Linux's
        // enforcement layer and netcat dialect are a separate port.
        #[cfg(target_os = "macos")]
        configure_ssh_tunneling(&mut cmd, network.proxy_addr().port(), workspace_root);
    }
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![workspace_root.to_path_buf()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: network_policy,
    };

    // Harness-provisioned grants first (`~/.gnupg` for gpg signing), then
    // the caller's approved/configured grants, deduplicated by value so a
    // config or judge grant for the same path stays a single entry.
    let mut effective_grants =
        default_filesystem_grants(std::env::var_os("HOME").as_deref().map(Path::new));
    for grant in filesystem_grants {
        if !effective_grants.contains(grant) {
            effective_grants.push(grant.clone());
        }
    }

    PreparedCommand {
        command: cmd,
        policy,
        grants: effective_grants,
    }
}

fn configure_proxy_environment(command: &mut std::process::Command, proxy_url: &str) {
    for key in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "CARGO_HTTP_PROXY",
    ] {
        command.env(key, proxy_url);
    }
    // An inherited bypass list would send matching hosts to a route the
    // kernel deliberately refuses. Empty values are understood by the common
    // clients and make the session proxy the only configured HTTP route.
    command.env("no_proxy", "").env("NO_PROXY", "");
    // Do not claim arbitrary-protocol proxy compatibility. The allowlist
    // proxy is HTTP/CONNECT; scheme-specific variables above cover web tools.
    command.env_remove("all_proxy").env_remove("ALL_PROXY");
}

/// macOS: route Git's SSH transport through the session proxy. The
/// Seatbelt profile's only non-DNS TCP egress is the proxy endpoint, so a
/// plain `git push` to an SSH remote dies at `connect(2)` with "Operation
/// not permitted". BSD `nc -X connect` bridges the gap: it CONNECT-tunnels
/// to `<host>:22`, which the allowlist proxy accepts for any approved host
/// regardless of port (verified live: `CONNECT github.com:22` tunnels once
/// `github.com` is approved), so domain approval rides the existing
/// `DomainDenialRetry` flow. `UserKnownHostsFile` moves into the sandbox
/// scratch dir -- the real `~/.ssh/known_hosts` is not writable here --
/// with `accept-new` TOFU semantics, and `ssh` reaches its keys through
/// the unix-socket grants on the network policy. Linux is untouched for
/// now (its enforcement layer and netcat dialect differ).
#[cfg(target_os = "macos")]
fn configure_ssh_tunneling(
    command: &mut std::process::Command,
    proxy_port: u16,
    workspace_root: &Path,
) {
    // An inherited variant would silently bypass this session's tunnel (or
    // point at another session's proxy address).
    command.env_remove("GIT_SSH_COMMAND").env_remove("GIT_SSH");
    let known_hosts = workspace_root
        .join(horizon_sandbox::SCRATCH_DIR_NAME)
        .join("known_hosts");
    // `tmpdir::provision` normally creates the scratch dir; create it here
    // too so the known_hosts file's parent exists even when the caller set
    // its own TMPDIR (which skips provisioning).
    let _ = std::fs::create_dir_all(known_hosts.parent().expect("scratch join has a parent"));
    command.env(
        "GIT_SSH_COMMAND",
        git_ssh_command_for_proxy(proxy_port, &known_hosts),
    );
}

/// The `GIT_SSH_COMMAND` value: SSH over the session proxy, with host keys
/// accumulated in the sandbox scratch dir (TOFU via `accept-new` -- no
/// interactive prompt a contained session could never answer).
// Production callers are macOS-only (`configure_ssh_tunneling`, whose own
// doc comment records Linux's separate enforcement/netcat dialect); on
// Linux the only caller is `sandbox_provisioning_tests`, which clippy's
// non-test lib target cannot see.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn git_ssh_command_for_proxy(proxy_port: u16, known_hosts: &Path) -> String {
    format!(
        "ssh -o ProxyCommand='/usr/bin/nc -X connect -x 127.0.0.1:{proxy_port} %h %p' \
         -o UserKnownHostsFile='{}' -o StrictHostKeyChecking=accept-new",
        known_hosts.display()
    )
}

/// `~/.gnupg` read-write when it exists: gpg-signed commits (this repo
/// sets `commit.gpgsign`) write lockfiles there and spawn `gpg-agent`.
/// Reads were never contained (`ReadableScope::Full` grants `/`), so this
/// only adds the write side signing needs, and only while the directory
/// exists. Caller-provided grants (config or judge-approved) are merged
/// after these and deduplicated by value at the merge site.
fn default_filesystem_grants(home: Option<&Path>) -> Vec<horizon_sandbox::FilesystemGrant> {
    let Some(home) = home else {
        return Vec::new();
    };
    let gnupg = home.join(".gnupg");
    if !gnupg.is_dir() {
        return Vec::new();
    }
    vec![horizon_sandbox::FilesystemGrant {
        path: gnupg,
        access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
        scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
        excluded_subpaths: Vec::new(),
    }]
}

/// Harness-provisioned unix-socket `connect` grants for a proxied sandbox
/// (`NetworkPolicy::Proxied`'s `unix_socket_connect` field): the ssh-agent
/// socket for SSH remotes, and -- when a GPG home exists -- its children
/// with a bind allowance, because the sandboxed command itself spawns the
/// gpg-agent that binds them. Without these, `git push` over SSH fails at
/// the agent connect ("Error connecting to agent: Operation not
/// permitted") and signed commits fail at keyboxd/gpg-agent -- both
/// observed live in the 2026-09-07 session this fix comes from.
fn unix_socket_connect_grants(
    ssh_auth_sock: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Vec<horizon_sandbox::UnixSocketConnectGrant> {
    let mut grants = Vec::new();
    if let Some(sock) = ssh_auth_sock {
        grants.push(horizon_sandbox::UnixSocketConnectGrant {
            path: PathBuf::from(sock),
            scope: horizon_sandbox::UnixSocketConnectScope::File,
            allow_bind: false,
        });
    }
    if let Some(home) = home {
        let gnupg = home.join(".gnupg");
        if gnupg.is_dir() {
            grants.push(horizon_sandbox::UnixSocketConnectGrant {
                path: gnupg,
                scope: horizon_sandbox::UnixSocketConnectScope::DirChildren,
                allow_bind: true,
            });
        }
    }
    grants
}

#[cfg(test)]
mod proxy_environment_tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use super::configure_proxy_environment;

    #[test]
    fn standard_http_clients_are_routed_without_configuring_unrelated_protocols() {
        let mut command = std::process::Command::new("true");
        configure_proxy_environment(&mut command, "http://127.0.0.1:43210");
        let env = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect::<BTreeMap<_, _>>();

        for key in [
            "http_proxy",
            "https_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "CARGO_HTTP_PROXY",
        ] {
            assert_eq!(
                env.get(OsStr::new(key)).and_then(Option::as_deref),
                Some(OsStr::new("http://127.0.0.1:43210"))
            );
        }
        for key in ["no_proxy", "NO_PROXY"] {
            assert_eq!(
                env.get(OsStr::new(key)).and_then(Option::as_deref),
                Some(OsStr::new(""))
            );
        }
        for key in ["all_proxy", "ALL_PROXY"] {
            assert!(matches!(env.get(OsStr::new(key)), Some(None)));
        }
    }
}

#[cfg(test)]
mod sandbox_provisioning_tests {
    use std::path::{Path, PathBuf};

    use super::{default_filesystem_grants, git_ssh_command_for_proxy, unix_socket_connect_grants};

    fn tmp_home_with_gnupg(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("hzn-exec-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".gnupg")).expect("create fake home");
        home
    }

    #[test]
    fn gnupg_grant_is_provisioned_only_when_the_directory_exists() {
        let home = tmp_home_with_gnupg("gnupg-grant");
        let grants = default_filesystem_grants(Some(&home));
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].path, home.join(".gnupg"));
        assert_eq!(
            grants[0].access,
            horizon_sandbox::FilesystemGrantAccess::ReadWrite
        );
        assert_eq!(
            grants[0].scope,
            horizon_sandbox::FilesystemGrantScope::DirectoryTree
        );

        let missing = home.join("no-such-gnupg");
        assert!(default_filesystem_grants(Some(&missing)).is_empty());
        assert!(default_filesystem_grants(None).is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unix_socket_grants_cover_the_agent_and_the_gnupg_children() {
        let home = tmp_home_with_gnupg("unix-socket-grants");
        let grants = unix_socket_connect_grants(
            Some(std::ffi::OsStr::new(
                "/var/run/com.apple.launchd.x/Listeners",
            )),
            Some(&home),
        );
        assert_eq!(grants.len(), 2);
        assert_eq!(
            grants[0].path,
            PathBuf::from("/var/run/com.apple.launchd.x/Listeners")
        );
        assert_eq!(
            grants[0].scope,
            horizon_sandbox::UnixSocketConnectScope::File
        );
        assert!(!grants[0].allow_bind, "ssh-agent is a pure client");
        assert_eq!(grants[1].path, home.join(".gnupg"));
        assert_eq!(
            grants[1].scope,
            horizon_sandbox::UnixSocketConnectScope::DirChildren
        );
        assert!(grants[1].allow_bind, "gpg-agent binds its own sockets");

        assert_eq!(unix_socket_connect_grants(None, Some(&home)).len(), 1);
        assert!(unix_socket_connect_grants(None, None).is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_ssh_tunnel_command_targets_the_proxy_and_scratch_known_hosts() {
        let command =
            git_ssh_command_for_proxy(59279, Path::new("/ws/.horizon-sandbox-tmp/known_hosts"));
        assert!(command.starts_with("ssh "));
        assert!(
            command.contains("ProxyCommand='/usr/bin/nc -X connect -x 127.0.0.1:59279 %h %p'"),
            "unexpected tunnel command: {command}"
        );
        assert!(
            command.contains("UserKnownHostsFile='/ws/.horizon-sandbox-tmp/known_hosts'"),
            "unexpected known_hosts: {command}"
        );
        assert!(command.contains("StrictHostKeyChecking=accept-new"));
    }
}
