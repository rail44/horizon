//! Validate external command arguments before any shell operation runs.

use horizon_agent::contract::ToolCallId;
use horizon_agent::roles::RoleId;
use horizon_control::contract::Invoke;
use horizon_workspace::commands::CommandId;
use horizon_workspace::{PaneKind, SessionId, SplitAxis};
use std::path::PathBuf;

#[derive(Debug)]
pub(super) enum Command {
    NewSession {
        kind: PaneKind,
        role_id: Option<RoleId>,
        split: Option<(SessionId, SplitAxis)>,
        issuer: Option<SessionId>,
        activate: bool,
        prompt: Option<String>,
        isolate: Option<bool>,
    },
    Preview {
        path: PathBuf,
        name: Option<String>,
        split: Option<(SessionId, SplitAxis)>,
        activate: bool,
    },
    Attach {
        session_id: SessionId,
        activate: bool,
    },
    Terminate(SessionId),
    TerminateAllDetached,
    Execute(CommandId),
    Approve {
        session_id: SessionId,
        call_id: ToolCallId,
    },
    Deny {
        session_id: SessionId,
        call_id: ToolCallId,
        reason: Option<String>,
    },
    CancelTurn(SessionId),
    ContinueTurn(SessionId),
    Send {
        session_id: SessionId,
        text: String,
    },
    SetModel {
        session_id: SessionId,
        provider: String,
        model: String,
    },
}

pub(super) fn parse(invoke: &Invoke) -> Result<Command, String> {
    let args = &invoke.args;
    Ok(match invoke.command.as_str() {
        "new-terminal" => new_session(args, PaneKind::Terminal)?,
        "new-agent" => new_session(args, PaneKind::Agent)?,
        "preview" => Command::Preview {
            path: required_string_arg(args, "path")?.into(),
            name: optional_string_arg(args, "name")?,
            split: optional_session_id_arg(args, "split")?,
            activate: activate_arg(args)?,
        },
        "attach" => Command::Attach {
            session_id: session_id_arg(args, "session_id")?,
            activate: activate_arg(args)?,
        },
        "terminate-session" => Command::Terminate(session_id_arg(args, "session_id")?),
        "terminate-all-detached" => Command::TerminateAllDetached,
        "reload-config" => Command::Execute(CommandId::ReloadConfig),
        "open-terminal-in-session-directory" => {
            Command::Execute(CommandId::OpenTerminalInSessionDirectory)
        }
        "approve" => Command::Approve {
            session_id: session_id_arg(args, "session_id")?,
            call_id: call_id_arg(args, "call_id")?,
        },
        "deny" => Command::Deny {
            session_id: session_id_arg(args, "session_id")?,
            call_id: call_id_arg(args, "call_id")?,
            reason: optional_string_arg(args, "reason")?,
        },
        "cancel-turn" => Command::CancelTurn(session_id_arg(args, "session_id")?),
        "continue-turn" => Command::ContinueTurn(session_id_arg(args, "session_id")?),
        "send" => Command::Send {
            session_id: session_id_arg(args, "session_id")?,
            text: nonempty_string_arg(args, "text")?,
        },
        "set-model" => Command::SetModel {
            session_id: session_id_arg(args, "session_id")?,
            provider: nonempty_string_arg(args, "provider")?,
            model: nonempty_string_arg(args, "model")?,
        },
        "reload-agent-runtime" | "reload-session-runtime" => {
            Command::Execute(CommandId::ReloadAgentRuntime)
        }
        "reload-terminal-runtime" => Command::Execute(CommandId::ReloadTerminalRuntime),
        other => return Err(format!("unknown external command `{other}`")),
    })
}

fn new_session(args: &serde_json::Value, kind: PaneKind) -> Result<Command, String> {
    let role_id = optional_string_arg(args, "role")?
        .map(|role| {
            let known = horizon_agent::roles::user_launchable();
            if !known.iter().any(|r| r.id == role) {
                let available = known.iter().map(|r| r.title).collect::<Vec<_>>().join(", ");
                return Err(format!("unknown role: {role} (available: {available})"));
            }
            Ok(RoleId(role))
        })
        .transpose()?;
    let split = optional_session_id_arg(args, "split")?;
    let issuer = optional_plain_session_id_arg(args, "issuer")?;
    let activate = activate_arg(args)?;
    let prompt = match args.get("prompt") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(prompt)) if kind == PaneKind::Agent => Some(prompt.clone()),
        Some(serde_json::Value::String(_)) => {
            return Err("`prompt` is only accepted for agent sessions".into())
        }
        Some(_) => return Err("`prompt` must be a string".into()),
    };
    let isolate = isolate_arg(args)?;
    if isolate.is_some() && kind != PaneKind::Agent {
        return Err("`isolate` is only accepted for agent sessions".into());
    }
    Ok(Command::NewSession {
        kind,
        role_id,
        split,
        issuer,
        activate,
        prompt,
        isolate,
    })
}

fn nonempty_string_arg(args: &serde_json::Value, key: &str) -> Result<String, String> {
    let value = required_string_arg(args, key)?;
    if value.is_empty() {
        return Err(format!("`{key}` must not be empty"));
    }
    Ok(value)
}

fn session_id_arg(args: &serde_json::Value, key: &str) -> Result<SessionId, String> {
    match args.get(key) {
        Some(serde_json::Value::String(raw)) => raw
            .parse::<uuid::Uuid>()
            .map(SessionId::from_uuid)
            .map_err(|_| format!("`{key}` must be a UUID string")),
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("`{key}` is required")),
    }
}

fn optional_session_id_arg(
    args: &serde_json::Value,
    key: &str,
) -> Result<Option<(SessionId, SplitAxis)>, String> {
    optional_plain_session_id_arg(args, key).map(|id| id.map(|id| (id, SplitAxis::Horizontal)))
}

/// Parses an optional session-id argument that is *not* a split target --
/// the `"issuer"` key (issue 013): the session that dispatched the CLI
/// request. Mirrors [`optional_session_id_arg`]'s shape but returns a bare
/// `SessionId` without the `SplitAxis` pairing the split path needs.
fn optional_plain_session_id_arg(
    args: &serde_json::Value,
    key: &str,
) -> Result<Option<SessionId>, String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) => raw
            .parse::<uuid::Uuid>()
            .map(|uuid| Some(SessionId::from_uuid(uuid)))
            .map_err(|_| format!("`{key}` must be a UUID string")),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn activate_arg(args: &serde_json::Value) -> Result<bool, String> {
    match args.get("activate") {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(activate)) => Ok(*activate),
        Some(_) => Err("`activate` must be a boolean".to_string()),
    }
}

/// Parses an optional plain-string argument -- `deny`'s `reason`, when the
/// CLI supplied `--reason`. `None` (the key omitted, or explicit `null`) means
/// "no reason supplied"; a string is taken verbatim. Mirrors [`isolate_arg`]'s
/// omitted-means-default shape.
fn optional_string_arg(args: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) => Ok(Some(raw.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

/// `docs/session-relationship-design.md` decision 3's per-spawn isolation
/// override: `None` (the key omitted, or explicit `null`) means "apply the
/// origin default" (control-plane origin: isolated -- see
/// `WorkspaceShell::control_plane_new_session`), mirroring `activate_arg`'s own
/// omitted-means-apply-the-surface-default shape.
fn isolate_arg(args: &serde_json::Value) -> Result<Option<bool>, String> {
    match args.get("isolate") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(isolate)) => Ok(Some(*isolate)),
        Some(_) => Err("`isolate` must be a boolean".to_string()),
    }
}

fn call_id_arg(
    args: &serde_json::Value,
    key: &str,
) -> Result<horizon_agent::contract::ToolCallId, String> {
    required_string_arg(args, key).map(ToolCallId)
}

/// Parses a required plain-string argument -- the `send` command's `text`
/// payload. Mirrors [`call_id_arg`]'s shape but returns a bare `String`
/// rather than a typed wrapper. The `send` arm enforces non-emptiness
/// separately (empty input is rejected at the CLI layer already; this is
/// defensive).
fn required_string_arg(args: &serde_json::Value, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(serde_json::Value::String(raw)) => Ok(raw.clone()),
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("`{key}` is required")),
    }
}

#[cfg(test)]
mod tests {
    // Import only the helpers under test -- `use super::*` pulls in every
    // item from the parent module, and the crate's `recursion_limit`
    // (raised for `theme.rs`'s large `json!` macro) then can't absorb the
    // `#[test]` expansion on top, hitting the limit before the test body
    // even compiles. Narrowing the import sidesteps that.
    use super::{optional_string_arg, parse, required_string_arg, session_id_arg, Command};
    use horizon_control::contract::Invoke;

    #[test]
    fn session_defaults_and_optional_nulls_match_the_external_contract() {
        for args in [
            serde_json::json!({}),
            serde_json::json!({"split": null, "issuer": null, "activate": null, "prompt": null, "isolate": null}),
        ] {
            let command = parse(&Invoke {
                command: "new-agent".into(),
                args,
            })
            .unwrap();
            assert!(matches!(
                command,
                Command::NewSession {
                    kind: horizon_workspace::PaneKind::Agent,
                    role_id: None,
                    split: None,
                    issuer: None,
                    activate: false,
                    prompt: None,
                    isolate: None,
                }
            ));
        }
    }

    #[test]
    fn invalid_arguments_report_the_first_error_before_execution() {
        let id = uuid::Uuid::nil().to_string();
        for (command, args, expected) in [
            (
                "new-agent",
                serde_json::json!({"split": 1, "activate": "yes"}),
                "`split` must be a string",
            ),
            (
                "new-terminal",
                serde_json::json!({"prompt": "go", "isolate": true}),
                "`prompt` is only accepted for agent sessions",
            ),
            (
                "new-terminal",
                serde_json::json!({"isolate": false}),
                "`isolate` is only accepted for agent sessions",
            ),
            (
                "send",
                serde_json::json!({"text": ""}),
                "`session_id` is required",
            ),
            (
                "send",
                serde_json::json!({"session_id": id, "text": ""}),
                "`text` must not be empty",
            ),
            (
                "set-model",
                serde_json::json!({"session_id": id, "provider": "", "model": ""}),
                "`provider` must not be empty",
            ),
            (
                "set-model",
                serde_json::json!({"session_id": id, "provider": "p", "model": ""}),
                "`model` must not be empty",
            ),
        ] {
            assert_eq!(
                parse(&Invoke {
                    command: command.into(),
                    args
                })
                .unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn legacy_reload_name_and_model_switch_keep_their_dispatch() {
        for command in ["reload-session-runtime", "reload-agent-runtime"] {
            assert!(matches!(
                parse(&Invoke {
                    command: command.into(),
                    args: serde_json::json!({})
                })
                .unwrap(),
                Command::Execute(horizon_workspace::commands::CommandId::ReloadAgentRuntime)
            ));
        }
        assert!(
            matches!(parse(&Invoke { command: "set-model".into(), args: serde_json::json!({
            "session_id": uuid::Uuid::nil().to_string(), "provider": "p", "model": "m",
        }) }).unwrap(), Command::SetModel { provider, model, .. } if provider == "p" && model == "m")
        );
    }

    fn json_object(pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (key, value) in pairs {
            map.insert((*key).to_string(), value.clone());
        }
        serde_json::Value::Object(map)
    }

    #[test]
    fn required_string_arg_returns_the_string_when_present() {
        let args = json_object(&[("text", serde_json::Value::String("hello".to_string()))]);
        assert_eq!(required_string_arg(&args, "text"), Ok("hello".to_string()));
    }

    #[test]
    fn required_string_arg_rejects_a_missing_key() {
        let args = json_object(&[]);
        assert_eq!(
            required_string_arg(&args, "text").unwrap_err(),
            "`text` is required".to_string()
        );
    }

    #[test]
    fn required_string_arg_rejects_a_non_string_value() {
        let args = json_object(&[("text", serde_json::Value::Number(42.into()))]);
        assert_eq!(
            required_string_arg(&args, "text").unwrap_err(),
            "`text` must be a string".to_string()
        );
    }

    #[test]
    fn session_id_arg_rejects_a_missing_key() {
        let args = json_object(&[]);
        assert_eq!(
            session_id_arg(&args, "session_id").unwrap_err(),
            "`session_id` is required".to_string()
        );
    }

    #[test]
    fn optional_string_arg_returns_none_when_omitted() {
        let args = json_object(&[]);
        assert_eq!(optional_string_arg(&args, "reason"), Ok(None));
    }

    #[test]
    fn optional_string_arg_returns_none_for_explicit_null() {
        let args = json_object(&[("reason", serde_json::Value::Null)]);
        assert_eq!(optional_string_arg(&args, "reason"), Ok(None));
    }

    #[test]
    fn optional_string_arg_returns_the_string_when_present() {
        let args = json_object(&[("reason", serde_json::Value::String("too risky".to_string()))]);
        assert_eq!(
            optional_string_arg(&args, "reason"),
            Ok(Some("too risky".to_string()))
        );
    }

    #[test]
    fn optional_string_arg_rejects_a_non_string_value() {
        let args = json_object(&[("reason", serde_json::Value::Number(42.into()))]);
        assert_eq!(
            optional_string_arg(&args, "reason").unwrap_err(),
            "`reason` must be a string".to_string()
        );
    }
}
