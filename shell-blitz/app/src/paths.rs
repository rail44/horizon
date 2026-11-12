//! Where this shell keeps its sockets, and where it finds the daemons.
//!
//! Until it replaces the GPUI shell, this shell does not share that one's
//! daemons: its sockets are in a directory of its own. The runtime clients
//! drain and restart a daemon whose build does not match theirs, and a
//! shell under development reaching the terminal daemon in daily use would
//! end every terminal running in it.

use std::path::{Path, PathBuf};

/// The directory this shell's sockets are in: `$HORIZON_BLITZ_RUNTIME_DIR`,
/// else `horizon-blitz` under `$XDG_RUNTIME_DIR`, else under the system's
/// temporary directory.
pub fn runtime_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os("HORIZON_BLITZ_RUNTIME_DIR") {
        return PathBuf::from(explicit);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("horizon-blitz")
}

/// A daemon's binary: the one the environment names for it if any, else
/// the one beside this shell's own binary, else the one of the same
/// profile in the root workspace's `target/`, where `cargo build
/// --workspace` at the repository's root puts it. `None` leaves it to the
/// runtime client's own search.
pub fn daemon_binary(name: &str, exe: &Path) -> Option<PathBuf> {
    let beside = exe.parent()?.join(name);
    if beside.is_file() {
        return Some(beside);
    }
    // `shell-blitz/target/<profile>/horizon-blitz`, and the root's
    // `target/<profile>/<name>`.
    let profile = exe.parent()?;
    let root = profile.parent()?.parent()?.parent()?;
    let built = root.join("target").join(profile.file_name()?).join(name);
    built.is_file().then_some(built)
}

/// The file this shell keeps its workspace in: `$HORIZON_BLITZ_WORKSPACE_STATE`,
/// else `horizon-blitz/workspace.json` under `$XDG_STATE_HOME`, else under
/// `~/.local/state`, else under the system's temporary directory. Never
/// the GPUI shell's file: the sessions in it are another daemon's.
pub fn workspace_state() -> PathBuf {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(explicit) = set("HORIZON_BLITZ_WORKSPACE_STATE") {
        return PathBuf::from(explicit);
    }
    let base = (set("XDG_STATE_HOME").map(PathBuf::from))
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("horizon-blitz").join("workspace.json")
}
