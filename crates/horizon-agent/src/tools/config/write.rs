use std::path::{Path, PathBuf};
use std::{fs, io};

use serde_json::{json, Value};

use super::error_output;
use crate::tools::state::ToolSessionState;

/// The pure-ish body of `config.write`: takes the resolved target path as a
/// parameter (see `super::read_at`'s doc comment on why) so it's testable
/// without touching a real `$HOME`/`$XDG_CONFIG_HOME`.
pub(super) fn execute(
    tool_state: &ToolSessionState,
    path: Option<PathBuf>,
    input: &Value,
) -> Value {
    let Some(path) = path else {
        return error_output(
            "could not resolve a config file path to write to -- set HORIZON_CONFIG, or ensure \
             $HOME or $XDG_CONFIG_HOME is set",
        );
    };
    let Some(content) = input.get("content").and_then(Value::as_str) else {
        return error_output("config.write requires a `content` string argument");
    };

    if let Err(error) = toml::from_str::<toml::Value>(content) {
        return error_output(format!("`content` is not valid TOML: {error}"));
    }

    let existed = path.exists();
    if existed {
        if path.is_dir() {
            return error_output(format!("`{}` is a directory, not a file", path.display()));
        }
        if let Err(error) = check_staleness(tool_state, &path) {
            return error;
        }
    } else if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = fs::create_dir_all(parent) {
                return error_output(format!(
                    "failed to create parent directories for `{}`: {error}",
                    path.display()
                ));
            }
        }
    }

    if let Err(error) = write_atomically(&path, content) {
        return error_output(format!("failed to write `{}`: {error}", path.display()));
    }

    if let Ok(mtime) = fs::metadata(&path).and_then(|metadata| metadata.modified()) {
        tool_state.record_mtime(path.clone(), mtime);
    }

    json!({
        "path": path.display().to_string(),
        "bytes_written": content.len(),
        "created": !existed,
    })
}

/// Mirrors `tools::fs::staleness::check_staleness` exactly, reusing the
/// same generic `ToolSessionState` mtime tracking (it isn't
/// `workspace_root`-scoped, so this works unchanged for a path outside it).
fn check_staleness(tool_state: &ToolSessionState, path: &Path) -> Result<(), Value> {
    let Some(recorded) = tool_state.recorded_mtime(path) else {
        return Err(error_output(format!(
            "`{}` has not been read this session -- read it with config.read first",
            path.display()
        )));
    };

    let current = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| error_output(format!("cannot stat `{}`: {error}", path.display())))?;

    if current != recorded {
        return Err(error_output(format!(
            "`{}` changed on disk -- read it again with config.read",
            path.display()
        )));
    }

    Ok(())
}

/// Writes `content` to `path` atomically: a temp file in the same
/// directory (so the final `rename` is same-filesystem and therefore
/// atomic), then a rename over `path`. A random suffix keeps two concurrent
/// writers (unlikely for a single host config file, but not impossible with
/// more than one session holding the config role) from colliding on the
/// same temp name.
fn write_atomically(path: &Path, content: &str) -> io::Result<()> {
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let tmp_name = format!(".{file_name}.horizon-tmp-{}", uuid::Uuid::new_v4());
    let tmp_path = match dir {
        Some(dir) => dir.join(tmp_name),
        None => PathBuf::from(tmp_name),
    };

    fs::write(&tmp_path, content)?;
    fs::rename(&tmp_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-agent-config-write-{label}-{}.toml",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn rejects_invalid_toml_without_touching_disk() {
        let tool_state = ToolSessionState::without_root();
        let path = temp_path("invalid-toml");

        let result = execute(
            &tool_state,
            Some(path.clone()),
            &json!({ "content": "not valid toml {{{" }),
        );

        assert_eq!(result["is_error"], true);
        assert!(!path.exists(), "an invalid write must never touch disk");
    }

    #[test]
    fn requires_prior_read_when_the_file_already_exists() {
        let tool_state = ToolSessionState::without_root();
        let path = temp_path("requires-read");
        std::fs::write(&path, "[theme]\naccent = \"#000000\"\n").unwrap();

        let result = execute(
            &tool_state,
            Some(path.clone()),
            &json!({ "content": "[theme]\naccent = \"#ffffff\"\n" }),
        );

        assert_eq!(result["is_error"], true);
        assert!(result["message"].as_str().unwrap().contains("read"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn detects_staleness_when_the_file_changed_since_the_recorded_read() {
        let tool_state = ToolSessionState::without_root();
        let path = temp_path("stale");
        std::fs::write(&path, "[theme]\naccent = \"#000000\"\n").unwrap();
        // Simulate config.read having been called earlier with a
        // deliberately wrong recorded mtime -- standing in for the file
        // changing on disk between the read and this write.
        tool_state.record_mtime(path.clone(), std::time::SystemTime::UNIX_EPOCH);

        let result = execute(
            &tool_state,
            Some(path.clone()),
            &json!({ "content": "[theme]\naccent = \"#ffffff\"\n" }),
        );

        assert_eq!(result["is_error"], true);
        assert!(result["message"].as_str().unwrap().contains("changed"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn writes_a_new_file_atomically_and_creates_parent_directories() {
        let tool_state = ToolSessionState::without_root();
        let dir = std::env::temp_dir().join(format!(
            "horizon-agent-config-write-parents-{}",
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("nested").join("config.toml");

        let result = execute(
            &tool_state,
            Some(path.clone()),
            &json!({ "content": "[theme]\naccent = \"#ffffff\"\n" }),
        );

        assert_eq!(result["is_error"], Value::Null);
        assert_eq!(result["created"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[theme]\naccent = \"#ffffff\"\n"
        );
        // No leftover temp file next to the target.
        let siblings: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(siblings, vec![std::ffi::OsString::from("config.toml")]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrites_an_existing_file_after_a_prior_read_records_its_mtime() {
        let tool_state = ToolSessionState::without_root();
        let path = temp_path("overwrite");
        std::fs::write(&path, "[theme]\naccent = \"#000000\"\n").unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        tool_state.record_mtime(path.clone(), mtime);

        let result = execute(
            &tool_state,
            Some(path.clone()),
            &json!({ "content": "[theme]\naccent = \"#ffffff\"\n" }),
        );

        assert_eq!(result["is_error"], Value::Null);
        assert_eq!(result["created"], false);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[theme]\naccent = \"#ffffff\"\n"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn errors_when_no_path_can_be_resolved() {
        let tool_state = ToolSessionState::without_root();

        let result = execute(&tool_state, None, &json!({ "content": "[theme]\n" }));

        assert_eq!(result["is_error"], true);
    }
}
