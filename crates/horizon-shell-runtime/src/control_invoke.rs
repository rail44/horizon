//! The control plane's external commands, their arguments validated
//! before any shell operation runs: what both shells dispatch on.

use crate::agent_model::ToolCallIdentity;
use horizon_control::contract::Invoke;
use horizon_workspace::commands::CommandId;
use horizon_workspace::{SessionId, SplitAxis};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Command {
    /// A new pane of the plugin named: `arguments`, and those `named`, are
    /// what its pane is made from, a file among them a path from
    /// `directory`. `issuer` is the session that asked.
    New {
        plugin: String,
        arguments: Vec<String>,
        named: BTreeMap<String, String>,
        directory: PathBuf,
        split: Option<(SessionId, SplitAxis)>,
        issuer: Option<SessionId>,
        activate: bool,
    },
    /// The command named, of the plugin the cursor's pane shows.
    PluginCommand(String),
    Attach {
        session_id: SessionId,
        activate: bool,
    },
    Terminate(SessionId),
    TerminateAllDetached,
    Execute(CommandId),
    Approve {
        session_id: SessionId,
        identity: ToolCallIdentity,
    },
    Deny {
        session_id: SessionId,
        identity: ToolCallIdentity,
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
    /// Takes the extension whose component is `file` in place of the one
    /// of its name, until it is reset or the app restarts.
    ExtensionUse {
        file: PathBuf,
    },
    /// Lets go of what `ExtensionUse` took in: the one named, or all.
    ExtensionReset {
        name: Option<String>,
    },
}

pub fn parse(invoke: &Invoke) -> Result<Command, String> {
    let args = &invoke.args;
    Ok(match invoke.command.as_str() {
        "new" => Command::New {
            plugin: required_string_arg(args, "plugin")?,
            arguments: string_list_arg(args, "arguments")?,
            named: named_arg(args, "named")?,
            directory: (optional_string_arg(args, "directory")?)
                .unwrap_or_default()
                .into(),
            split: optional_session_id_arg(args, "split")?,
            issuer: optional_plain_session_id_arg(args, "issuer")?,
            activate: activate_arg(args)?,
        },
        "plugin-command" => Command::PluginCommand(required_string_arg(args, "name")?),
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
            identity: approval_identity_arg(args)?,
        },
        "deny" => Command::Deny {
            session_id: session_id_arg(args, "session_id")?,
            identity: approval_identity_arg(args)?,
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
        "extension-use" => Command::ExtensionUse {
            file: nonempty_string_arg(args, "file")?.into(),
        },
        "extension-reset" => Command::ExtensionReset {
            name: optional_string_arg(args, "name")?,
        },
        "reload-agent-runtime" => Command::Execute(CommandId::ReloadAgentRuntime),
        "reload-terminal-runtime" => Command::Execute(CommandId::ReloadTerminalRuntime),
        other => return Err(format!("unknown external command `{other}`")),
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
/// "no reason supplied"; a string is taken verbatim.
fn optional_string_arg(args: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) => Ok(Some(raw.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn approval_identity_arg(args: &serde_json::Value) -> Result<ToolCallIdentity, String> {
    let call_id = required_string_arg(args, "call_id")?;
    let occurrence_id = required_string_arg(args, "occurrence_id")?;
    if call_id.is_empty() || occurrence_id.is_empty() {
        return Err("approval requires non-empty call_id and occurrence_id".into());
    }
    Ok(ToolCallIdentity {
        call_id,
        occurrence_id,
    })
}

/// Parses a required plain-string argument -- the `send` command's `text`
/// payload. Mirrors [`approval_identity_arg`]'s shape but returns a bare `String`
/// rather than a typed wrapper. The `send` arm enforces non-emptiness
/// separately (empty input is rejected at the CLI layer already; this is
/// defensive).
/// Parses a table of strings by name; none when the key is left out.
fn named_arg(args: &serde_json::Value, key: &str) -> Result<BTreeMap<String, String>, String> {
    let said = || format!("`{key}` must be a table of strings");
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(BTreeMap::new()),
        Some(serde_json::Value::Object(named)) => (named.iter())
            .map(|(name, value)| Ok((name.clone(), value.as_str().ok_or_else(said)?.to_string())))
            .collect(),
        Some(_) => Err(said()),
    }
}

/// Parses a list of strings; none when the key is left out.
fn string_list_arg(args: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    let said = || format!("`{key}` must be a list of strings");
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => (items.iter())
            .map(|item| item.as_str().map(str::to_string).ok_or_else(said))
            .collect(),
        Some(_) => Err(said()),
    }
}

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
    use super::{Command, optional_string_arg, parse, required_string_arg, session_id_arg};
    use horizon_control::contract::Invoke;

    #[test]
    fn a_new_pane_s_defaults_and_optional_nulls_match_the_external_contract() {
        for args in [
            serde_json::json!({"plugin": "agent"}),
            serde_json::json!({
                "plugin": "agent", "arguments": null, "named": null, "directory": null,
                "split": null, "issuer": null, "activate": null,
            }),
        ] {
            let command = parse(&Invoke {
                command: "new".into(),
                args,
            })
            .unwrap();
            let Command::New {
                plugin,
                arguments,
                named,
                split: None,
                issuer: None,
                activate: false,
                ..
            } = command
            else {
                panic!("a new pane with nothing given for it");
            };
            assert_eq!(plugin, "agent");
            assert!(arguments.is_empty() && named.is_empty());
        }
    }

    #[test]
    fn a_new_pane_takes_what_it_is_made_from_in_order_and_by_name() {
        let command = parse(&Invoke {
            command: "new".into(),
            args: serde_json::json!({
                "plugin": "preview",
                "arguments": ["a.wasm", "README.md"],
                "named": {"name": "board-list"},
                "directory": "/work",
            }),
        })
        .unwrap();
        let Command::New {
            arguments,
            named,
            directory,
            ..
        } = command
        else {
            panic!("a new pane");
        };
        assert_eq!(arguments, ["a.wasm", "README.md"]);
        assert_eq!(named["name"], "board-list");
        assert_eq!(directory, std::path::PathBuf::from("/work"));
    }

    #[test]
    fn invalid_arguments_report_the_first_error_before_execution() {
        let id = uuid::Uuid::nil().to_string();
        for (command, args, expected) in [
            (
                "new",
                serde_json::json!({"plugin": "agent", "split": 1, "activate": "yes"}),
                "`split` must be a string",
            ),
            (
                "new",
                serde_json::json!({"plugin": "agent", "named": {"prompt": 1}}),
                "`named` must be a table of strings",
            ),
            ("new", serde_json::json!({}), "`plugin` is required"),
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
    fn runtime_reload_and_model_switch_dispatch() {
        std::assert_matches!(
            parse(&Invoke {
                command: "reload-agent-runtime".into(),
                args: serde_json::json!({})
            })
            .unwrap(),
            Command::Execute(horizon_workspace::commands::CommandId::ReloadAgentRuntime)
        );
        std::assert_matches!(
            parse(&Invoke { command: "set-model".into(), args: serde_json::json!({
            "session_id": uuid::Uuid::nil().to_string(), "provider": "p", "model": "m",
        }) }).unwrap(), Command::SetModel { provider, model, .. } if provider == "p" && model == "m"
        );
    }

    #[test]
    fn the_extension_commands_take_a_file_and_a_name_that_may_be_left_out() {
        let parsed = |command: &str, args: serde_json::Value| {
            parse(&Invoke {
                command: command.into(),
                args,
            })
        };
        std::assert_matches!(
            parsed("extension-use", serde_json::json!({ "file": "/work/a.wasm" })).unwrap(),
            Command::ExtensionUse { file } if file == std::path::Path::new("/work/a.wasm")
        );
        assert!(parsed("extension-use", serde_json::json!({})).is_err());
        assert!(parsed("extension-use", serde_json::json!({ "file": "" })).is_err());
        std::assert_matches!(
            parsed("extension-reset", serde_json::json!({ "name": "sessions" })).unwrap(),
            Command::ExtensionReset { name: Some(name) } if name == "sessions"
        );
        std::assert_matches!(
            parsed("extension-reset", serde_json::json!({ "name": null })).unwrap(),
            Command::ExtensionReset { name: None }
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

    #[test]
    fn approval_identity_requires_both_ids_and_preserves_the_supplied_occurrence() {
        use super::approval_identity_arg;
        use crate::agent_model::ToolCallIdentity;
        for args in [
            serde_json::json!({"call_id":"same"}),
            serde_json::json!({"call_id":"same", "occurrence_id":""}),
            serde_json::json!({"call_id":"same", "occurrence_id":42}),
        ] {
            assert!(approval_identity_arg(&args).is_err());
        }
        assert_eq!(
            approval_identity_arg(&serde_json::json!({"call_id":"same", "occurrence_id":"old"}))
                .unwrap(),
            ToolCallIdentity {
                call_id: "same".into(),
                occurrence_id: "old".into()
            }
        );
    }
}
