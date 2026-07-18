//! `config.read`/`config.write`/`skill.read`. The first two are the config
//! role's only allowed tools (`roles::CONFIG_ROLE`); `skill.read` is
//! available to every session, role-bearing or not (see `skills`' module
//! doc). Grouped in one module because `config.read`/`config.write` share a
//! trust story, and `skill.read` shares this module's dispatch shape --
//! not because any of the three share code with `tools::fs`:
//!
//! **Why `config.write` bypasses `workspace_root` confinement.**
//! `tools::fs::safety::resolve_path` confines every filesystem tool to the
//! session's workspace root -- the untrusted-repository boundary
//! (`docs/trust-boundaries.md`). `config.write` deliberately does not go
//! through it: its one and only target is the single config file
//! `horizon-sessiond` itself resolves (`$HORIZON_CONFIG` >
//! `$XDG_CONFIG_HOME/horizon/config.toml` > `~/.config/horizon/config.toml`,
//! via `horizon_config::resolved_path()`) and injects into this session's
//! `ToolSessionState::config_path` at spawn time -- a host-owned file
//! addressed the same way the host process itself addresses it, not an
//! arbitrary path the model supplies. `docs/agent-tools-design.md` already
//! notes `bash` can
//! write this same file with no additional tool at all; cataloging
//! `config.write` globally (`tools::catalog::definitions`) adds no new
//! *capability* to the system for that reason. What `config.write`
//! contributes is a *narrower* one: paired with a role's `allowed_tool_ids`
//! (`roles::CONFIG_ROLE`), it lets a session be restricted to touching only
//! this one file and nothing else in the filesystem or shell -- see
//! `docs/agent-tools-design.md`'s tool table and `roles`'s own doc comment
//! on why the config role excludes `bash`/`fs.*` entirely.
//!
//! **Staleness gate.** Mirrors `tools::fs::write`'s prior-read + staleness
//! pattern exactly, reusing the same generic `ToolSessionState::
//! record_mtime`/`recorded_mtime` (keyed by path, not confined to
//! `workspace_root`) -- see [`write::execute`].

mod write;

use std::path::Path;

use serde_json::{json, Value};

use crate::tools::state::ToolSessionState;

/// Executes an auto-allowed (`AutoAllowRead`) tool from this module's
/// catalog entries. Returns `None` for any other tool id, so the caller can
/// try elsewhere -- same contract as `tools::fs::execute_auto`.
pub(crate) fn execute_auto(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    match tool_id {
        "config.read" => Some(read_at(tool_state, tool_state.config_path())),
        "skill.read" => Some(crate::skills::execute_read(
            tool_state.skill_registry(),
            input,
        )),
        _ => None,
    }
}

/// Executes a Horizon-approved (`RequireApproval`) tool from this module --
/// only `config.write`. Mirrors `tools::fs::execute_approved`'s shape and
/// fallback-error convention.
pub(crate) fn execute_approved(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Value {
    match tool_id {
        "config.write" => write::execute(
            tool_state,
            tool_state.config_path().map(Path::to_path_buf),
            input,
        ),
        _ => error_output(format!("tool `{tool_id}` has no Horizon-side execution")),
    }
}

/// The pure body of `config.read`, taking the resolved path as a parameter
/// (rather than resolving it itself) so it's testable without touching real
/// env vars or the developer's own config file -- the same
/// pure-function/path-parameter style `config`'s own precedence-resolution
/// functions use, applied here for the same reason.
fn read_at(tool_state: &ToolSessionState, path: Option<&Path>) -> Value {
    let Some(path) = path else {
        return error_output(
            "could not resolve a config file path -- set HORIZON_CONFIG, or ensure $HOME or \
             $XDG_CONFIG_HOME is set",
        );
    };
    match std::fs::read_to_string(path) {
        Ok(content) => {
            if let Ok(mtime) = std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
                tool_state.record_mtime(path.to_path_buf(), mtime);
            }
            json!({
                "path": path.display().to_string(),
                "exists": true,
                "content": content,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({
            "path": path.display().to_string(),
            "exists": false,
            "content": Value::Null,
            "message": "config file does not exist yet -- write it with config.write to create it",
        }),
        Err(error) => error_output(format!("cannot read `{}`: {error}", path.display())),
    }
}

fn error_output(message: impl Into<String>) -> Value {
    json!({ "is_error": true, "message": message.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-agent-config-tool-{label}-{}.toml",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn read_at_reports_missing_file_without_erroring() {
        let tool_state = ToolSessionState::without_root();
        let missing = temp_path("missing");

        let result = read_at(&tool_state, Some(&missing));

        assert_eq!(result["exists"], false);
        assert_eq!(result["content"], Value::Null);
        assert_eq!(result["path"], missing.display().to_string());
        assert!(tool_state.recorded_mtime(&missing).is_none());
    }

    #[test]
    fn read_at_returns_existing_file_contents_and_records_its_mtime() {
        let tool_state = ToolSessionState::without_root();
        let path = temp_path("existing");
        std::fs::write(&path, "[theme]\naccent = \"#ffffff\"\n").unwrap();

        let result = read_at(&tool_state, Some(&path));

        assert_eq!(result["exists"], true);
        assert!(result["content"].as_str().unwrap().contains("accent"));
        assert!(
            tool_state.recorded_mtime(&path).is_some(),
            "a successful read must record the mtime for config.write's staleness gate"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_at_errors_when_no_path_can_be_resolved() {
        let tool_state = ToolSessionState::without_root();

        let result = read_at(&tool_state, None);

        assert_eq!(result["is_error"], true);
    }
}
