//! Proactive guard for Cargo operations that would erase a build cache
//! shared by every worktree. This is a UX/resource guard, not a containment
//! boundary: the sandbox still owns filesystem authority.

use std::path::Path;

use super::git::{executable_index, tokenize, ShellToken};

const CONFIG_PATHS: [&str; 2] = [".cargo/config.toml", ".cargo/config"];

/// Returns an explanation when a directly recognizable command contains an
/// unscoped `cargo clean` and this workspace explicitly puts its build dir in
/// Cargo's shared cache home. Package-scoped cleans remain available for the
/// stale-workspace-crate recovery documented in Horizon's agent guide.
pub(super) fn shared_cache_clean_refusal(
    command: &str,
    workspace_root: &Path,
) -> Option<&'static str> {
    if !uses_shared_cache_build_dir(workspace_root) {
        return None;
    }

    command_has_unscoped_clean(command).then_some(refusal_message())
}

fn command_has_unscoped_clean(command: &str) -> bool {
    let mut segment = Vec::new();
    for token in tokenize(command) {
        match token {
            ShellToken::Word(word) => segment.push(word),
            ShellToken::Boundary => {
                if segment_has_unscoped_clean(&segment) {
                    return true;
                }
                segment.clear();
            }
        }
    }
    segment_has_unscoped_clean(&segment)
}

fn refusal_message() -> &'static str {
    "Refused `cargo clean` without `--package`: this workspace's Cargo build-dir is shared \
     across all worktrees, so a full clean would erase Horizon's shared dependency cache. \
     Use `cargo clean -p <crate>` for a stale workspace crate. For an intentionally isolated \
     full rebuild, set `CARGO_BUILD_BUILD_DIR=$PWD/target-local-build` first."
}

fn uses_shared_cache_build_dir(workspace_root: &Path) -> bool {
    CONFIG_PATHS.iter().any(|relative| {
        let Ok(source) = std::fs::read_to_string(workspace_root.join(relative)) else {
            return false;
        };
        let Ok(config) = source.parse::<toml::Value>() else {
            return false;
        };
        config
            .get("build")
            .and_then(|build| build.get("build-dir"))
            .and_then(toml::Value::as_str)
            .is_some_and(|path| path.contains("{cargo-cache-home}"))
    })
}

fn segment_has_unscoped_clean(words: &[String]) -> bool {
    let Some(cargo_index) = executable_index(words) else {
        return false;
    };
    if Path::new(&words[cargo_index])
        .file_name()
        .and_then(|name| name.to_str())
        != Some("cargo")
    {
        return false;
    }

    let Some(clean_index) = cargo_subcommand_index(words, cargo_index + 1) else {
        return false;
    };
    words[clean_index] == "clean" && !has_package_scope(&words[clean_index + 1..])
}

fn cargo_subcommand_index(words: &[String], mut index: usize) -> Option<usize> {
    if words.get(index).is_some_and(|word| word.starts_with('+')) {
        index += 1;
    }
    while let Some(word) = words.get(index).map(String::as_str) {
        match word {
            "--color" | "--config" | "--manifest-path" | "--lockfile-path" | "-Z" => {
                index += 2;
            }
            value
                if value.starts_with("--color=")
                    || value.starts_with("--config=")
                    || value.starts_with("--manifest-path=")
                    || value.starts_with("--lockfile-path=")
                    || value.starts_with("-Z") =>
            {
                index += 1;
            }
            "--" => return None,
            value if value.starts_with('-') => index += 1,
            _ => return Some(index),
        }
    }
    None
}

fn has_package_scope(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "-p"
            || arg == "--package"
            || arg.starts_with("--package=")
            || (arg.starts_with("-p") && arg.len() > 2)
    })
}

#[cfg(test)]
mod tests {
    use super::{command_has_unscoped_clean, shared_cache_clean_refusal};

    #[test]
    fn recognizes_direct_unscoped_clean_through_common_shell_prefixes() {
        for command in [
            "cargo clean",
            "cargo +stable clean --release",
            "env CARGO_TERM_COLOR=never cargo clean",
            "command cargo --color always clean",
            "cargo check && cargo clean",
        ] {
            assert!(
                command_has_unscoped_clean(command),
                "expected to recognize {command:?}"
            );
        }
    }

    #[test]
    fn package_scoped_or_unrelated_cargo_commands_are_not_refused() {
        for command in [
            "cargo clean -p horizon-agent",
            "cargo clean --package=horizon-agent",
            "cargo check",
            "cargo run -- clean",
            "echo cargo clean",
        ] {
            assert!(
                !command_has_unscoped_clean(command),
                "expected to allow {command:?}"
            );
        }
    }

    #[test]
    fn guard_only_activates_for_a_workspace_with_a_shared_build_dir() {
        let root = std::env::temp_dir().join(format!(
            "horizon-cargo-clean-guard-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        std::fs::write(
            root.join(".cargo/config.toml"),
            "[build]\nbuild-dir = \"target-build\"\n",
        )
        .unwrap();
        assert!(shared_cache_clean_refusal("cargo clean", &root).is_none());

        std::fs::write(
            root.join(".cargo/config.toml"),
            "[build]\nbuild-dir = \"{cargo-cache-home}/shared\"\n",
        )
        .unwrap();
        assert!(shared_cache_clean_refusal("cargo clean", &root).is_some());
        assert!(shared_cache_clean_refusal("cargo clean -p crate", &root).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
