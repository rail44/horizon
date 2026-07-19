//! macOS sandbox backend: nono's Seatbelt sandbox. `crate::caps::build`
//! maps `SandboxPolicy` -> `nono::CapabilitySet` (shared verbatim with the
//! Linux backend); this module applies it via a tiny exec helper
//! (`horizon-sandbox-helper`, `src/bin/horizon-sandbox-helper.rs`) instead
//! of the old `sandbox-exec`+SBPL invocation (`docs/roadmap.md`'s
//! backlog-60 entry).
//!
//! **Why a helper process, unlike Linux's in-thread `apply_auto`:** on
//! Linux, `nono::Sandbox::apply_auto` restricts only the *calling
//! thread* (`linux::spawn` applies it on a dedicated throwaway thread that
//! then spawns the child directly). On macOS, `nono::Sandbox::apply_auto`
//! (`sandbox::macos::apply`, verified in nono 0.68.0 source) self-applies
//! Seatbelt to the *whole calling process*, irreversibly -- there is no
//! thread-scoped variant, because Seatbelt itself has no such concept.
//! Applying it in Horizon's own process (or `horizon-sessiond`'s) would
//! therefore sandbox the entire host, not just the one command being run.
//! Instead, `spawn` execs a tiny helper binary that applies the sandbox to
//! *itself* and then `exec()`s the real command -- the same
//! separate-process shape the old `sandbox-exec` wrapping (and Linux's
//! pre-nono bwrap backend) already used, just with nono in the helper
//! instead of a system binary.
//!
//! **Not runtime-verified from this development machine** (Linux only,
//! per `AGENTS.md`): this backend is compile-checked cross-target
//! (`cargo check --target x86_64-apple-darwin -p horizon-sandbox`) and
//! unit-tested at the `CapabilitySet`-construction level only
//! (`crate::caps::tests`, cfg'd macOS-only where it inspects macOS-specific
//! capability shapes -- those can't run here either, compile-checked via
//! `--target x86_64-apple-darwin --tests`). The owner's macOS build is the
//! runtime verification gate for this backend -- same posture the repo
//! already takes for the winit backend (see `docs/winit-backend-design.md`)
//! -- though nono's own Seatbelt backend does carry real upstream CI on
//! macos-14, more runtime verification than this crate's old hand-rolled
//! SBPL ever had.

use crate::error::SandboxError;
use crate::policy::{SandboxPolicy, SandboxStdio};
use crate::SandboxedChild;
use std::path::PathBuf;
use std::process::Command;

/// Name of the exec-helper binary `spawn` looks for -- see
/// [`resolve_helper`].
const HELPER_BIN_NAME: &str = "horizon-sandbox-helper";

/// Applies `policy` to the calling process via nono's Seatbelt backend.
/// Called only from within the `horizon-sandbox-helper` binary, after it
/// has exec'd into existence for exactly this purpose (see the module
/// doc) -- irreversible, like all of nono's `apply_auto`.
///
/// Exposed as `pub` (rather than `pub(crate)`) purely because that helper
/// is a separate crate -- a bin target of this same package, but Cargo
/// bin targets can't see `pub(crate)` items from the package's lib target.
/// Not intended as public API for other consumers of this library.
#[doc(hidden)]
pub fn apply_seatbelt_to_self(policy: &SandboxPolicy) -> Result<(), SandboxError> {
    let caps = crate::caps::build(policy)?;
    nono::Sandbox::apply_auto(&caps)?;
    Ok(())
}

/// Cheap capability probe backing [`crate::is_available`]: nono's own
/// static "is Seatbelt supported" check, replacing the old `sandbox-exec`
/// binary-existence probe now that this backend no longer shells out to
/// it. Always `true` on any real macOS (per nono's own doc for
/// `Sandbox::is_supported`'s macOS arm -- Seatbelt has been present on
/// every modern release) -- `spawn` itself still fails informatively if
/// the helper binary can't be resolved, see [`resolve_helper`].
pub(crate) fn is_available() -> bool {
    nono::Sandbox::is_supported()
}

/// Resolves the `horizon-sandbox-helper` binary: next to the running
/// executable first (the normal deployed shape, mirroring how Horizon
/// ships `horizon-sessiond` alongside the main binary), then falls back to
/// a `PATH` lookup (e.g. a `cargo install`'d or dev-build layout where the
/// two binaries don't share a directory) -- the same two-tier precedent
/// the old Linux backend's `resolve_bwrap` used for a hardcoded system
/// binary, adapted here since the helper is one of Horizon's own binaries
/// rather than a fixed system path. A missing helper is a clean, typed
/// error, never a panic.
fn resolve_helper() -> Result<PathBuf, SandboxError> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(HELPER_BIN_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(HELPER_BIN_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(SandboxError::HelperNotFound)
}

/// Prepares and spawns `command` under `policy` via the exec helper:
/// `horizon-sandbox-helper <policy-json> <program> [args...]`. The helper
/// builds the same `CapabilitySet` this process would have (`crate::
/// caps::build`), self-applies it, then `exec()`s into `program`/`args` --
/// see the module doc for why this indirection is necessary on macOS.
pub(crate) fn spawn(
    command: Command,
    policy: &SandboxPolicy,
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    let helper = resolve_helper()?;
    let policy_json = serde_json::to_string(policy)?;

    let program = command.get_program().to_os_string();
    let args: Vec<_> = command.get_args().map(|a| a.to_os_string()).collect();

    let mut wrapped = Command::new(&helper);
    wrapped.arg(&policy_json);
    wrapped.arg(&program);
    wrapped.args(&args);

    if let Some(cwd) = command.get_current_dir() {
        wrapped.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(v) => {
                wrapped.env(key, v);
            }
            None => {
                wrapped.env_remove(key);
            }
        }
    }

    // TMPDIR parity (`docs/roadmap.md`'s backlog-60 entry): set on the
    // helper invocation itself, so it's already part of the ambient
    // environment the helper inherits across its own `exec()` into the
    // real command -- see `crate::tmpdir`'s module doc.
    crate::tmpdir::provision(policy, &command, &mut wrapped)?;

    wrapped
        .stdin(stdio.stdin)
        .stdout(stdio.stdout)
        .stderr(stdio.stderr);

    let child = wrapped.spawn()?;
    Ok(SandboxedChild { child })
}
