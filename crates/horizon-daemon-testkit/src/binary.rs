//! Locating and spawning a workspace daemon binary from an integration test.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

/// How long to wait once before re-probing/re-spawning after finding a
/// daemon binary transiently missing -- see [`resolve_daemon_binary`] and
/// [`spawn_with_link_retry`]. A single bounded wait, not a polling loop: the
/// race this covers is cargo's own artifact-uplift `remove_file`-then-relink
/// (a link syscall's worth of time), confirmed locally (`docs/tasks/backlog.md`
/// #36) to close well under this.
pub const TRANSIENT_LINK_RETRY_DELAY: Duration = Duration::from_millis(200);

/// The name of the runtime environment variable cargo injects for
/// `binary_name` -- same name as the compile-time `env!()` bake, but a
/// *different* mechanism; see [`resolve_daemon_binary`].
pub fn cargo_bin_exe_var(binary_name: &str) -> String {
    format!("CARGO_BIN_EXE_{binary_name}")
}

/// Resolves the daemon binary to spawn, preferring
/// `std::env::var("CARGO_BIN_EXE_<binary_name>")` -- a genuine OS
/// environment variable of *this test process*, re-injected fresh by
/// cargo/cargo-nextest on every invocation -- over `baked_in`, which callers
/// pass as `env!("CARGO_BIN_EXE_<name>")`: a constant frozen into the test
/// binary's own compiled code at the moment it was built. (The `env!()` half
/// has to be produced at the call site, because that macro only expands
/// inside the package that owns the `[[bin]]` target.)
///
/// Cargo documents setting `CARGO_BIN_EXE_<name>` twice, for two different
/// consumers: once as a `rustc` build-time env var (readable only via
/// `env!()`/`option_env!()`, baked into the compiled artifact), and again
/// as the literal runtime environment of the spawned test *process* itself
/// every time `cargo test`/`cargo nextest run` executes it -- see
/// <https://doc.rust-lang.org/cargo/reference/environment-variables.html>
/// ("Cargo sets several environment variables when tests are run. You can
/// retrieve the values when the tests are run"). Confirmed empirically
/// against this repo's actual toolchain (both `cargo test` and `cargo
/// nextest run`) while diagnosing `docs/tasks/backlog.md` #40.
///
/// That distinction is load-bearing here because this repo's
/// `build.build-dir` split (`AGENTS.md` "Build setup") makes test binaries
/// themselves shared, reusable *intermediate* build artifacts: unlike a
/// daemon (a real `[[bin]]` target, uplifted fresh into every worktree's own
/// `target/`), a compiled integration-test binary can be reused unchanged
/// (relinked, not recompiled) into a fresh worktree without ever re-running
/// `rustc` -- confirmed by inspecting the shared build-dir directly:
/// `deps/e2e-*` binaries live only under `{cargo-cache-home}/
/// horizon-build-dir/`, never uplifted into any worktree's `target/`, so
/// `std::env::current_exe()` for a test binary always resolves *inside the
/// shared build-dir*, not the worktree -- it cannot anchor a per-worktree
/// path at all. A stale, reused test binary's `env!()` bake therefore still
/// holds the absolute path from whichever worktree compiled it *first*, and
/// once that worktree is deleted (the normal worker lifecycle), that path is
/// permanently dead. The runtime env var has no such problem: cargo/nextest
/// compute and inject it fresh for *this* invocation, from *this* worktree's
/// own `cargo metadata`, regardless of how stale the test binary's compiled
/// code is.
///
/// The `env!()` bake is kept only as a defensive fallback, for the (today
/// unobserved) case of a test binary being invoked directly, bypassing
/// `cargo test`/`cargo nextest run`'s own env injection. Either path can
/// still be transiently missing due to cargo's own non-atomic
/// artifact-uplift step (`docs/tasks/backlog.md` #36); that race is handled
/// at the `spawn()` call site by [`spawn_with_link_retry`], not here.
pub fn resolve_daemon_binary(binary_name: &str, baked_in: &str) -> PathBuf {
    let var = cargo_bin_exe_var(binary_name);
    if let Ok(runtime_var) = std::env::var(&var) {
        let path = PathBuf::from(runtime_var);
        if path.is_file() {
            return path;
        }
    }

    let baked_in = PathBuf::from(baked_in);
    if baked_in.is_file() {
        return baked_in;
    }

    panic!(
        "could not locate the {binary_name} binary to spawn for this test -- probed runtime env \
         var {var} = {:?} and the compile-time {var} bake = {} (exists = {}) -- see \
         docs/tasks/backlog.md #40",
        std::env::var(&var),
        baked_in.display(),
        baked_in.is_file(),
    );
}

/// Resolves a daemon binary belonging to *another* package, as a sibling of
/// one this test can name itself: `CARGO_BIN_EXE_<name>` is only injected
/// for binaries of the package a test belongs to, and every workspace binary
/// is uplifted into the same target directory, so the sibling lookup is the
/// one honest way to reach it (the same rule
/// `horizon_wire::spawn::resolve_daemon_binary` uses in production).
pub fn sibling_daemon_binary(anchor: &Path, binary_name: &str) -> PathBuf {
    let candidate = anchor
        .parent()
        .expect("the anchor binary must live in a directory")
        .join(binary_name);
    assert!(
        candidate.is_file(),
        "expected {binary_name} next to {} at {} -- run `cargo build --workspace` (the quality \
         gate does)",
        anchor.display(),
        candidate.display(),
    );
    candidate
}

/// Spawns `command`, retrying once after [`TRANSIENT_LINK_RETRY_DELAY`] if
/// the program is momentarily missing -- cargo's own non-atomic
/// artifact-uplift window (`docs/tasks/backlog.md` #36).
pub fn spawn_with_link_retry(command: &mut Command) -> Child {
    match command.spawn() {
        Ok(child) => child,
        Err(first_error) if first_error.kind() == std::io::ErrorKind::NotFound => {
            thread::sleep(TRANSIENT_LINK_RETRY_DELAY);
            command.spawn().unwrap_or_else(|retry_error| {
                let program = command.get_program().to_owned();
                panic!(
                    "failed to spawn {} even after a retry for a transient link window: first \
                     error = {first_error}, retry error = {retry_error} (exists = {}) -- see \
                     docs/tasks/backlog.md #36",
                    program.to_string_lossy(),
                    Path::new(&program).is_file(),
                )
            })
        }
        Err(error) => panic!(
            "failed to spawn {}: {error}",
            command.get_program().to_string_lossy()
        ),
    }
}
