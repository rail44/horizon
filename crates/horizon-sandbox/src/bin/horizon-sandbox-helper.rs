//! Exec helper for `horizon-sandbox`'s macOS backend (`docs/roadmap.md`'s
//! backlog-60 entry; see `crates/horizon-sandbox/src/macos/mod.rs`'s
//! module doc for why macOS needs this indirection while Linux doesn't).
//!
//! nono's `Sandbox::apply_auto` self-applies Seatbelt to the *whole
//! calling process*, irreversibly -- there is no thread-scoped variant the
//! way Linux's Landlock has. Horizon's own process (and `horizon-sessiond`)
//! must stay unsandboxed, so the policy can't be applied in-process the
//! way `linux::spawn` does it. This binary exists solely to be that
//! separate process: `macos::spawn` execs it with the serialized policy
//! and the real command, it self-applies the sandbox, then `exec()`s into
//! the real command -- replacing the process image in place, so nothing
//! about the real command's stdio/pid/exit-status handling changes from
//! the caller's perspective.
//!
//! Usage: `horizon-sandbox-helper <policy-json> <program> [args...]`
//!
//! `<policy-json>` is a `serde_json`-serialized `horizon_sandbox::
//! SandboxPolicy` (see `macos::spawn`). Working directory and environment
//! are *not* passed as arguments -- `macos::spawn` sets them directly on
//! this process's own `Command` invocation, so they're already this
//! process's ambient cwd/env by the time `main` runs, and `exec()` below
//! inherits them into the real command automatically (a `Command` only
//! changes what it's explicitly told to; leaving cwd/env untouched here
//! preserves whatever the parent process already set).
//!
//! Non-macOS builds compile to a stub `main` -- bin targets can't be
//! `cfg`'d away from a package entirely, so this exists only to fail
//! loudly if somehow invoked on the wrong platform.

#[cfg(target_os = "macos")]
fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(policy_arg), Some(program)) = (args.next(), args.next()) else {
        eprintln!(
            "horizon-sandbox-helper: usage: horizon-sandbox-helper <policy-json> <program> [args...]"
        );
        std::process::exit(2);
    };
    let target_args: Vec<std::ffi::OsString> = args.collect();

    let policy_json = match policy_arg.to_str() {
        Some(s) => s,
        None => {
            eprintln!("horizon-sandbox-helper: policy argument is not valid UTF-8");
            std::process::exit(2);
        }
    };

    let policy: horizon_sandbox::SandboxPolicy = match serde_json::from_str(policy_json) {
        Ok(policy) => policy,
        Err(e) => {
            eprintln!("horizon-sandbox-helper: failed to parse policy JSON: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = horizon_sandbox::apply_seatbelt_to_self(&policy) {
        eprintln!("horizon-sandbox-helper: failed to apply sandbox: {e}");
        std::process::exit(1);
    }

    // `exec` only returns on failure -- on success it replaces this
    // process's image in place, so the real command inherits this
    // process's (already-sandboxed, already correctly cwd/env'd) state
    // directly, including its pid and stdio.
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&program)
        .args(&target_args)
        .exec();
    eprintln!("horizon-sandbox-helper: exec failed for {program:?}: {err}");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!(
        "horizon-sandbox-helper: this binary only runs as part of horizon-sandbox's macOS \
         Seatbelt backend (see crates/horizon-sandbox/src/macos/mod.rs); it does nothing on \
         this platform."
    );
    std::process::exit(1);
}
