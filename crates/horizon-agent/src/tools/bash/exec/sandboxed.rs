//! Sandboxed bash: prepare authority, capture execution, then classify the result.
use super::{failed_output, finished, resolve_timeout, BashCompletion};
use crate::config::BashToolConfig;
use crate::contract::ToolCallId;
use crate::tools::network::SessionNetworkProxy;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

mod capture;
mod prepare;
mod result;

// --- sandboxed execution (tier 1: docs/agent-approval-design.md) ----------
//
// A tier-1-auto-approved `bash` call runs through `horizon_sandbox::spawn`
// instead of a plain `TokioCommand`. `horizon_sandbox::spawn` hands back a
// plain `std::process::Child` (there is no tokio integration in that crate
// -- see its own crate doc), so this is a fully synchronous, thread-based
// implementation rather than reusing `run_async`'s tokio machinery: a
// watcher thread bounds the wait with `timeout` (killing by pid on
// expiry -- see `wait_child_with_timeout`'s doc comment), and two more
// threads blocking-pump stdout/stderr
// into shared buffers, bounded by `drain_grace` the same way `run_async`
// bounds its own tokio pumps. This already runs on its own dedicated
// background thread (`bash::spawn_sandboxed` -> `registry::enqueue`), so
// there is no UI-thread-blocking concern in doing this synchronously.

/// Runs one *sandboxed* bash call to completion (or until it times out).
/// `workspace_root` is the base writable root; explicitly approved session
/// grants are additive. Filesystem denials come from the authenticated Linux
/// supervisor report rather than output or exit-code heuristics.
///
/// Network (`docs/agent-approval-design.md` leg 4b): `Some(network)` gets
/// `NetworkPolicy::Proxied { proxy_addr }` for that session's exact loopback
/// TCP endpoint. Standard HTTP proxy variables make ordinary clients use it;
/// the Linux supervisor independently refuses every other remote endpoint,
/// so those variables are compatibility plumbing rather than the security
/// boundary. `None` falls back to plain `NetworkPolicy::Disabled`.
///
/// A denied domain is detected proxy-side, never from the child's own exit
/// code (backlog 59: a piped command like `curl ... | head` can exit `0`
/// even though the network call itself was refused) -- right after the
/// child exits, this drains `network`'s recorded denials
/// (`SessionNetworkProxy::drain_denied_hosts`) and, if any were recorded,
/// returns [`BashCompletion::DomainDenied`] instead of a plain `Finished`
/// result, regardless of what the child's own exit status/output would
/// otherwise suggest. The proxy record is authoritative; output text is not
/// used to infer a grant.
///
/// Containment fix (2026-07-19 dogfooding, backlog): this policy used to add
/// `std::env::temp_dir()` (the *host's* real temp dir) as a second writable
/// root, on the theory that the bash tool's own output-spill file
/// (`output::spill`) and a command's ordinary scratch use both needed it.
/// That was wrong on both counts and, worse, actively dangerous: `spill` is
/// called from *this* function, on the host process, after the sandboxed
/// child has already exited and its output has been captured over a pipe --
/// it never runs inside the sandbox and needs no writable-root grant at all.
/// And adding the host's real temp dir as a writable root actively broke
/// containment: a live dogfooding session observed a tier-1 auto-approved
/// `echo ... > /tmp/<name>` writing through to the real host `/tmp` while
/// the result still carried `sandboxed: true`. The sandbox now provides a
/// private scratch dir instead (`horizon_sandbox`'s TMPDIR-parity
/// provisioning under the first writable root -- `SCRATCH_DIR_NAME` -- which
/// replaced the retired bwrap backend's private `--tmpfs /tmp`), so the host
/// temp dir must never be a writable root.
#[allow(clippy::too_many_arguments)]
pub(in crate::tools::bash) fn run_sandboxed(
    call_id: &ToolCallId,
    input: &Value,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    workspace_root: &Path,
    network: Option<&SessionNetworkProxy>,
    loopback_connect: &[std::net::SocketAddr],
    filesystem_grants: &[horizon_sandbox::FilesystemGrant],
    config: &BashToolConfig,
) -> BashCompletion {
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return finished(
            call_id,
            failed_output("bash requires a `command` string argument", None, config),
        );
    };
    if command.trim().is_empty() {
        return finished(
            call_id,
            failed_output("bash requires a non-empty `command` string", None, config),
        );
    }
    if let Some(message) =
        crate::tools::bash::cargo::shared_cache_clean_refusal(command, workspace_root)
    {
        return finished(call_id, failed_output(message, None, config));
    }

    let timeout = resolve_timeout(input, config);
    let cwd = cwd_handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    let prepared = prepare::prepare(
        command,
        &cwd,
        workspace_root,
        network,
        loopback_connect,
        filesystem_grants,
    );
    #[cfg(target_os = "macos")]
    let started_at = std::time::SystemTime::now();
    let sandboxed = match horizon_sandbox::spawn_with_filesystem_grants(
        prepared.command,
        &prepared.policy,
        &prepared.grants,
        horizon_sandbox::SandboxStdio::piped_output(),
    ) {
        Ok(sandboxed) => sandboxed,
        Err(error) => {
            return finished(
                call_id,
                failed_output(
                    &format!("failed to start sandboxed bash: {error}"),
                    None,
                    config,
                ),
            );
        }
    };
    let captured = match capture::collect(
        sandboxed,
        call_id,
        timeout,
        config,
        #[cfg(target_os = "macos")]
        started_at,
    ) {
        Ok(captured) => captured,
        Err(value) => return finished(call_id, value),
    };
    // Read after the child exits, regardless of its exit code. Early spawn
    // or report failures retain their original failure without draining this.
    let denied_domains = network
        .map(SessionNetworkProxy::drain_denied_hosts)
        .unwrap_or_default();
    result::complete(
        call_id,
        captured,
        denied_domains,
        timeout,
        cwd_handle,
        config,
    )
}
