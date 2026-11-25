//! The external-name mapping table: turns a parsed [`crate::cli::Subcommand`]
//! into the stable string name and [`horizon_control::contract`] request
//! Horizon's control-plane endpoint expects, per the design doc's "Command
//! exposure" decision ("a mapping table, not an implementation"). The
//! server-side twin (external name -> internal `CommandId`) is a different
//! table, out of scope here -- this crate only ever speaks the external
//! vocabulary.

use horizon_control::contract::{Invoke, Query};

use crate::cli::Subcommand;

/// The wire request built for one subcommand: either an [`Invoke`]
/// (fire-and-forget-with-reply) or a [`Query`] (read-only snapshot), per
/// the design doc's "v1 operation shapes" decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Invoke(Invoke),
    Query(Query),
}

/// The stable external name for a subcommand -- identical to the CLI
/// subcommand string by construction (task spec: "外部名と1:1").
pub fn external_name(subcommand: &Subcommand) -> &'static str {
    match subcommand {
        Subcommand::New { .. } => "new",
        Subcommand::PluginCommand { .. } => "plugin-command",
        Subcommand::Attach { .. } => "attach",
        Subcommand::TerminateSession { .. } => "terminate-session",
        Subcommand::TerminateAllDetached => "terminate-all-detached",
        Subcommand::Approve { .. } => "approve",
        Subcommand::Deny { .. } => "deny",
        Subcommand::CancelTurn { .. } => "cancel-turn",
        Subcommand::Send { .. } => "send",
        Subcommand::SetModel { .. } => "set-model",
        Subcommand::SetEffort { .. } => "set-effort",
        Subcommand::ReloadAgentRuntime => "reload-agent-runtime",
        Subcommand::ReloadTerminalRuntime => "reload-terminal-runtime",
        Subcommand::ReloadConfig => "reload-config",
        Subcommand::OpenTerminalInSessionDirectory => "open-terminal-in-session-directory",
        Subcommand::Sessions => "sessions",
        Subcommand::State => "state",
        Subcommand::ExtensionUse { .. } => "extension-use",
        Subcommand::ExtensionReset { .. } => "extension-reset",
        Subcommand::ExtensionList => "extensions",
    }
}

/// Whether this subcommand is even a *candidate* for the design doc's
/// client-side destructive confirmation -- independent of whether the
/// server currently lists it in `State::destructive_commands` (checked at
/// runtime; see `crate::run`). Only the two subcommands the task spec calls
/// out ever require the check.
pub fn is_destructive(subcommand: &Subcommand) -> bool {
    matches!(
        subcommand,
        Subcommand::TerminateSession { .. } | Subcommand::TerminateAllDetached
    )
}

/// Builds the wire request for `subcommand`. `resolved_split` is
/// `NewTerminal`/`NewAgent`'s `--split` flag already resolved against the
/// environment (see `cli::resolved_split_for`) -- by the time this function
/// runs, "here" has already become a concrete session id or the caller has
/// already bailed out, so the wire only ever carries an explicit target
/// (`docs/cli-control-plane-design.md`'s "Targets are explicit in v1"
/// principle); every other subcommand ignores this parameter. `issuer` is
/// the session id of the pane that launched this CLI invocation (from
/// `HORIZON_SESSION_ID`), carried as the `"issuer"` wire key so the shell
/// can parent the new session to the issuer rather than the focused pane
/// (issue 013); `None` (no `HORIZON_SESSION_ID`, or it is empty) omits the
/// key, and the new session becomes a root. `args` interpretation on the
/// server side is entirely its own concern (per [`Invoke`]'s doc comment);
/// this function's only job is to fill it in consistently.
pub fn to_request(
    subcommand: &Subcommand,
    resolved_split: Option<&str>,
    issuer: Option<&str>,
) -> Request {
    let command_name = external_name(subcommand);
    match subcommand {
        Subcommand::New {
            plugin,
            arguments,
            named,
            directory,
            activate,
            ..
        } => {
            let mut args = serde_json::json!({
                "plugin": plugin,
                "arguments": arguments,
                "named": named,
                "directory": directory.display().to_string(),
                "activate": activate,
            });
            // (`split` and `issuer` ride only when there is one.)
            if let Some(split) = resolved_split {
                args["split"] = serde_json::Value::String(split.to_string());
            }
            if let Some(issuer) = issuer {
                args["issuer"] = serde_json::Value::String(issuer.to_string());
            }
            invoke(command_name, args)
        }
        Subcommand::PluginCommand { name } => {
            invoke(command_name, serde_json::json!({ "name": name }))
        }
        Subcommand::Attach {
            session_id,
            activate,
        } => invoke(
            command_name,
            serde_json::json!({ "session_id": session_id, "activate": activate }),
        ),
        Subcommand::TerminateSession { session_id } => invoke(
            command_name,
            serde_json::json!({ "session_id": session_id }),
        ),
        Subcommand::TerminateAllDetached => invoke(command_name, serde_json::json!({})),
        Subcommand::Approve {
            session_id,
            call_id,
            occurrence_id,
        } => invoke(
            command_name,
            serde_json::json!({ "session_id": session_id, "call_id": call_id, "occurrence_id": occurrence_id }),
        ),
        Subcommand::Deny {
            session_id,
            call_id,
            occurrence_id,
            reason,
        } => match reason {
            // Omit the key when no reason was supplied -- the server's
            // `Command::DenyToolCall.reason` carries `#[serde(default)]`, so
            // absence decodes as `None` (mirrors the `send` arm's text
            // omission).
            Some(reason) => invoke(
                command_name,
                serde_json::json!({ "session_id": session_id, "call_id": call_id, "occurrence_id": occurrence_id, "reason": reason }),
            ),
            None => invoke(
                command_name,
                serde_json::json!({ "session_id": session_id, "call_id": call_id, "occurrence_id": occurrence_id }),
            ),
        },
        Subcommand::CancelTurn { session_id } => invoke(
            command_name,
            serde_json::json!({ "session_id": session_id }),
        ),
        Subcommand::Send { session_id, text } => match text {
            Some(text) => invoke(
                command_name,
                serde_json::json!({ "session_id": session_id, "text": text }),
            ),
            // Pre-resolution state: `text` is `None` only between `parse`
            // and `run`'s stdin-fallback step, so `to_request` is never
            // called with `None` in production. Omitting the key lets the
            // server's own validation reject it (defensive -- see
            // `required_string_arg`).
            None => invoke(
                command_name,
                serde_json::json!({ "session_id": session_id }),
            ),
        },
        Subcommand::SetModel {
            session_id,
            provider,
            model,
        } => invoke(
            command_name,
            serde_json::json!({
                "session_id": session_id,
                "provider": provider,
                "model": model
            }),
        ),
        Subcommand::SetEffort { session_id, level } => invoke(
            command_name,
            serde_json::json!({ "session_id": session_id, "level": level }),
        ),
        Subcommand::ReloadAgentRuntime => invoke(command_name, serde_json::json!({})),
        Subcommand::ReloadTerminalRuntime => invoke(command_name, serde_json::json!({})),
        Subcommand::ReloadConfig => invoke(command_name, serde_json::json!({})),
        Subcommand::OpenTerminalInSessionDirectory => invoke(command_name, serde_json::json!({})),
        Subcommand::Sessions => Request::Query(Query {
            what: command_name.to_string(),
        }),
        Subcommand::State => Request::Query(Query {
            what: command_name.to_string(),
        }),
        Subcommand::ExtensionUse { file } => {
            invoke(command_name, serde_json::json!({ "file": file }))
        }
        Subcommand::ExtensionReset { name } => {
            invoke(command_name, serde_json::json!({ "name": name }))
        }
        Subcommand::ExtensionList => Request::Query(Query {
            what: command_name.to_string(),
        }),
    }
}

fn invoke(command: &str, args: serde_json::Value) -> Request {
    Request::Invoke(Invoke {
        command: command.to_string(),
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::SplitFlag;

    /// `new <plugin>` with `arguments` and those `named`, from `/work`.
    fn new(
        plugin: &str,
        arguments: &[&str],
        named: &[(&str, &str)],
        split: Option<SplitFlag>,
        activate: bool,
    ) -> Subcommand {
        Subcommand::New {
            plugin: plugin.to_string(),
            arguments: arguments.iter().map(|text| text.to_string()).collect(),
            named: (named.iter())
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            directory: std::path::PathBuf::from("/work"),
            split,
            activate,
        }
    }

    fn new_terminal(split: Option<SplitFlag>, activate: bool) -> Subcommand {
        new("terminal", &[], &[], split, activate)
    }

    /// What `new` sends, the request made for `subcommand`.
    fn sent(
        subcommand: &Subcommand,
        split: Option<&str>,
        issuer: Option<&str>,
    ) -> serde_json::Value {
        let Request::Invoke(invoke) = to_request(subcommand, split, issuer) else {
            panic!("expected an invoke");
        };
        assert_eq!(invoke.command, "new");
        invoke.args
    }

    #[test]
    fn new_sends_the_plugin_what_was_given_and_where_the_caller_is() {
        assert_eq!(
            sent(&new_terminal(None, false), None, None),
            serde_json::json!({
                "plugin": "terminal",
                "arguments": [],
                "named": {},
                "directory": "/work",
                "activate": false
            })
        );
        assert_eq!(
            sent(
                &new("preview", &["x.wasm", "README.md"], &[], None, true),
                None,
                None,
            ),
            serde_json::json!({
                "plugin": "preview",
                "arguments": ["x.wasm", "README.md"],
                "named": {},
                "directory": "/work",
                "activate": true
            })
        );
        assert_eq!(
            sent(
                &new(
                    "agent",
                    &[],
                    &[("prompt", "fix the bug"), ("role", "keeper")],
                    None,
                    false
                ),
                None,
                None,
            )["named"],
            serde_json::json!({"prompt": "fix the bug", "role": "keeper"})
        );
    }

    #[test]
    fn new_carries_the_resolved_split_and_the_issuer_only_when_there_is_one() {
        let bare = sent(&new_terminal(None, false), None, None);
        assert!(bare.get("split").is_none() && bare.get("issuer").is_none());
        let both = sent(
            &new_terminal(Some(SplitFlag::Here), false),
            Some("resolved-session"),
            Some("issuer-session"),
        );
        assert_eq!(both["split"], "resolved-session");
        assert_eq!(both["issuer"], "issuer-session");
        // (An explicit split is not the issuer's.)
        let apart = sent(
            &new(
                "agent",
                &[],
                &[],
                Some(SplitFlag::Explicit("s-1".to_string())),
                false,
            ),
            Some("s-1"),
            Some("s-2"),
        );
        assert_eq!(
            (&apart["split"], &apart["issuer"]),
            (&"s-1".into(), &"s-2".into())
        );
    }

    #[test]
    fn new_is_not_destructive_and_keeps_its_external_name() {
        assert_eq!(external_name(&new_terminal(None, false)), "new");
        assert!(!is_destructive(&new_terminal(None, false)));
    }

    #[test]
    fn a_plugin_s_command_is_sent_by_its_name() {
        let back = Subcommand::PluginCommand {
            name: "back".to_string(),
        };
        let Request::Invoke(invoke) = to_request(&back, None, None) else {
            panic!("expected an invoke");
        };
        assert_eq!(invoke.command, "plugin-command");
        assert_eq!(invoke.args, serde_json::json!({ "name": "back" }));
    }

    #[test]
    fn external_name_matches_the_cli_subcommand_string() {
        assert_eq!(external_name(&new_terminal(None, false)), "new");
        assert_eq!(
            external_name(&Subcommand::Attach {
                session_id: "s-1".to_string(),
                activate: false,
            }),
            "attach"
        );
        assert_eq!(
            external_name(&Subcommand::TerminateAllDetached),
            "terminate-all-detached"
        );
        assert_eq!(external_name(&Subcommand::Sessions), "sessions");
        assert_eq!(external_name(&Subcommand::State), "state");
        assert_eq!(
            external_name(&Subcommand::Send {
                session_id: "s-1".to_string(),
                text: None
            }),
            "send"
        );
        assert_eq!(
            external_name(&Subcommand::SetModel {
                session_id: "s-1".to_string(),
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
            }),
            "set-model"
        );
    }

    #[test]
    fn set_model_carries_the_session_provider_and_model_and_is_not_destructive() {
        let subcommand = Subcommand::SetModel {
            session_id: "s-1".to_string(),
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
        };
        assert!(!is_destructive(&subcommand));
        let Request::Invoke(invoke) = to_request(&subcommand, None, None) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "set-model");
        assert_eq!(
            invoke.args,
            serde_json::json!({ "session_id": "s-1", "provider": "openai", "model": "gpt-4o" })
        );
    }

    #[test]
    fn only_the_two_terminate_subcommands_are_destructive_candidates() {
        assert!(is_destructive(&Subcommand::TerminateAllDetached));
        assert!(is_destructive(&Subcommand::TerminateSession {
            session_id: "s-1".to_string()
        }));
        assert!(!is_destructive(&new_terminal(None, false)));
        assert!(!is_destructive(&Subcommand::Attach {
            session_id: "s-1".to_string(),
            activate: false,
        }));
        assert!(!is_destructive(&Subcommand::CancelTurn {
            session_id: "s-1".to_string()
        }));
        assert!(!is_destructive(&Subcommand::Sessions));
        assert!(!is_destructive(&Subcommand::Send {
            session_id: "s-1".to_string(),
            text: None
        }));
    }

    #[test]
    fn cancel_turn_carries_the_session_id() {
        let Request::Invoke(cancel) = to_request(
            &Subcommand::CancelTurn {
                session_id: "s-1".to_string(),
            },
            None,
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(cancel.command, "cancel-turn");
        assert_eq!(cancel.args, serde_json::json!({ "session_id": "s-1" }));
    }

    #[test]
    fn attach_carries_the_session_id_and_activate() {
        let Request::Invoke(invoke) = to_request(
            &Subcommand::Attach {
                session_id: "s-1".to_string(),
                activate: true,
            },
            None,
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "attach");
        assert_eq!(
            invoke.args,
            serde_json::json!({ "session_id": "s-1", "activate": true })
        );
    }

    #[test]
    fn terminate_session_carries_the_session_id() {
        let Request::Invoke(invoke) = to_request(
            &Subcommand::TerminateSession {
                session_id: "s-1".to_string(),
            },
            None,
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "terminate-session");
        assert_eq!(invoke.args, serde_json::json!({ "session_id": "s-1" }));
    }

    #[test]
    fn approve_and_deny_carry_both_ids() {
        let Request::Invoke(approve) = to_request(
            &Subcommand::Approve {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
                occurrence_id: "occ".into(),
            },
            None,
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(approve.command, "approve");
        assert_eq!(
            approve.args,
            serde_json::json!({ "session_id": "s-1", "call_id": "c-1", "occurrence_id": "occ" })
        );

        let Request::Invoke(deny) = to_request(
            &Subcommand::Deny {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
                occurrence_id: "occ".into(),
                reason: None,
            },
            None,
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(deny.command, "deny");
        assert_eq!(
            deny.args,
            serde_json::json!({ "session_id": "s-1", "call_id": "c-1", "occurrence_id": "occ" })
        );

        // `--reason` rides as a `reason` string on the invoke args; omitted
        // when `None` so the server's `#[serde(default)]` defaults it.
        let Request::Invoke(deny_reason) = to_request(
            &Subcommand::Deny {
                session_id: "s-1".to_string(),
                call_id: "c-1".to_string(),
                occurrence_id: "occ".into(),
                reason: Some("too risky".to_string()),
            },
            None,
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(
            deny_reason.args,
            serde_json::json!({ "session_id": "s-1", "call_id": "c-1", "occurrence_id": "occ", "reason": "too risky" })
        );
    }

    #[test]
    fn send_carries_session_id_and_text() {
        let Request::Invoke(invoke) = to_request(
            &Subcommand::Send {
                session_id: "s-1".to_string(),
                text: Some("fix the bug".to_string()),
            },
            None,
            None,
        ) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "send");
        assert_eq!(
            invoke.args,
            serde_json::json!({ "session_id": "s-1", "text": "fix the bug" })
        );
    }

    #[test]
    fn reload_terminal_runtime_is_a_bare_invoke_named_like_its_command() {
        // `docs/terminald-split-design.md` decision 3: the CLI verb is the
        // external name of `CommandId::ReloadTerminalRuntime`, carries no
        // arguments, and stays distinct from `reload-agent-runtime` --
        // one restarts the agent daemon, the other kills every PTY.
        assert_eq!(
            external_name(&Subcommand::ReloadTerminalRuntime),
            "reload-terminal-runtime"
        );
        let Request::Invoke(invoke) = to_request(&Subcommand::ReloadTerminalRuntime, None, None)
        else {
            panic!("expected an invoke request");
        };
        assert_eq!(invoke.command, "reload-terminal-runtime");
        assert_eq!(invoke.args, serde_json::json!({}));
    }

    #[test]
    fn reload_agent_runtime_and_reload_config_are_bare_invokes() {
        assert_eq!(
            external_name(&Subcommand::ReloadAgentRuntime),
            "reload-agent-runtime"
        );
        assert_eq!(external_name(&Subcommand::ReloadConfig), "reload-config");

        let Request::Invoke(invoke) = to_request(&Subcommand::ReloadConfig, None, None) else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "reload-config");
        assert_eq!(invoke.args, serde_json::json!({}));
    }

    #[test]
    fn open_terminal_in_session_directory_is_a_bare_invoke() {
        assert_eq!(
            external_name(&Subcommand::OpenTerminalInSessionDirectory),
            "open-terminal-in-session-directory"
        );
        assert!(!is_destructive(&Subcommand::OpenTerminalInSessionDirectory));

        let Request::Invoke(invoke) =
            to_request(&Subcommand::OpenTerminalInSessionDirectory, None, None)
        else {
            panic!("expected an Invoke request");
        };
        assert_eq!(invoke.command, "open-terminal-in-session-directory");
        assert_eq!(invoke.args, serde_json::json!({}));
    }

    #[test]
    fn sessions_and_state_are_queries() {
        std::assert_matches!(
            to_request(&Subcommand::Sessions, None, None),
            Request::Query(q) if q.what == "sessions");
        std::assert_matches!(
            to_request(&Subcommand::State, None, None),
            Request::Query(q) if q.what == "state");
    }

    #[test]
    fn the_extension_commands_are_two_invokes_and_a_query() {
        let Request::Invoke(used) = to_request(
            &Subcommand::ExtensionUse {
                file: "/work/a.wasm".to_string(),
            },
            None,
            None,
        ) else {
            panic!("an invoke");
        };
        assert_eq!(used.command, "extension-use");
        assert_eq!(used.args, serde_json::json!({ "file": "/work/a.wasm" }));
        let Request::Invoke(reset) =
            to_request(&Subcommand::ExtensionReset { name: None }, None, None)
        else {
            panic!("an invoke");
        };
        assert_eq!(reset.command, "extension-reset");
        assert_eq!(reset.args, serde_json::json!({ "name": null }));
        let Request::Query(listed) = to_request(&Subcommand::ExtensionList, None, None) else {
            panic!("a query");
        };
        assert_eq!(listed.what, "extensions");
    }
}
