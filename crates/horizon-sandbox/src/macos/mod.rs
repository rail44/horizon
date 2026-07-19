//! macOS sandbox backend: SBPL profile generation (`sbpl.rs`) plus
//! invocation of `/usr/bin/sandbox-exec`.
//!
//! **Not runtime-verified from this development machine** (Linux only,
//! per `AGENTS.md`): this backend is compile-structured and unit-tested at
//! the profile-*generation* string level only (`sbpl::tests`). The
//! owner's macOS build is the runtime verification gate for this backend
//! -- same posture the repo already takes for the winit backend (see
//! `docs/winit-backend-design.md`).

mod sbpl;

use crate::error::SandboxError;
use crate::policy::SandboxPolicy;
use crate::SandboxedChild;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Hardcoded path, deliberately not a `PATH` lookup: PATH-injection
/// defense (`docs/agent-approval-design.md`), and `sandbox-exec` has
/// lived at this exact path for over a decade of macOS releases with no
/// alternative location.
const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Prepares and spawns `command` under `policy` via `sandbox-exec -f
/// <profile file> -- <command>`. `-f` (a profile file) is used instead of
/// `-p` (an inline profile string) so the profile never has to round-trip
/// through shell argument quoting.
pub(crate) fn spawn(
    command: Command,
    policy: &SandboxPolicy,
) -> Result<SandboxedChild, SandboxError> {
    if !Path::new(SANDBOX_EXEC_PATH).is_file() {
        return Err(SandboxError::SandboxExecNotFound(SANDBOX_EXEC_PATH));
    }

    let profile = sbpl::compose(policy);
    let profile_path = write_profile_to_temp_file(&profile)?;

    let program = command.get_program().to_os_string();
    let args: Vec<_> = command.get_args().map(|a| a.to_os_string()).collect();

    let mut wrapped = Command::new(SANDBOX_EXEC_PATH);
    wrapped.arg("-f").arg(&profile_path);
    wrapped.arg("--");
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

    let child = wrapped.spawn()?;
    // The profile file only needs to survive until sandbox-exec has read
    // it at startup; sandbox-exec itself has no reload mechanism, so
    // removing it right after spawn (rather than leaking a temp file per
    // sandboxed command) is safe.
    let _ = std::fs::remove_file(&profile_path);

    Ok(SandboxedChild { child })
}

fn write_profile_to_temp_file(profile: &str) -> Result<std::path::PathBuf, SandboxError> {
    let path = std::env::temp_dir().join(format!(
        "horizon-sandbox-{}-{}.sb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    let mut file = std::fs::File::create(&path)?;
    file.write_all(profile.as_bytes())?;
    Ok(path)
}
