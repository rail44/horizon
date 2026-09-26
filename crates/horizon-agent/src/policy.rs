//! The approval trust model's policy seam (`docs/agent-approval-design.md`).
//! `plan_tool_call` selects automatic execution, an exact approval candidate,
//! or a terminal rejection. The execution coordinator consumes this decision.

use crate::tools::input::{PreparedCall, ToolInput};

#[cfg(test)]
use serde_json::Value;

use crate::contract::{ApprovalKind, ApprovalRequest, ToolCallRequest, ToolPermission};
use crate::tools::call_escapes_root;
use crate::tools::ToolSessionState;

/// A per-call trust classification -- the tier a single tool call falls
/// into, not a static per-tool-id policy. See the design doc's "The three
/// tiers".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Classification {
    /// Auto by construction (tier 1): runs without asking. Reversibility
    /// (an isolated worktree's git diff) and/or containment (the OS
    /// sandbox) stand in for consent.
    Contained,
    /// Crosses the containment boundary (tier 2 --
    /// `docs/agent-approval-design.md`'s "Judge at the boundary"): the
    /// judge's canonical case (MCP/non-sandboxed tools; network egress is
    /// excluded, see leg 4b's own `DomainDenialRetry` path). No *real*
    /// tool in this crate's catalog is classified this way today (there are
    /// no MCP/external tools wired in yet); `mock.boundary_crossing` is the
    /// fixture that exercises this classification until a real
    /// boundary-crossing tool exists.
    BoundaryCrossing,
    /// Always human (tier 3): irreversible/destructive by policy, or a
    /// contained-eligible tool call whose session isn't isolated / has no
    /// engaged sandbox available.
    AlwaysAsk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryDisposition {
    /// The call crosses the host boundary but the owner-selected policy
    /// permits it without creating an approval candidate.
    Auto,
    /// The call must wait for a human decision before contact occurs.
    Human,
}

/// The per-call trust predicate: pure, conservative, and explicit. `tool_id`
/// and `input` are the call being classified. Bash input is inspected only
/// to identify direct Git commands that may write repository metadata;
/// anything the small recognizer misses remains subject to the sandbox's
/// normal denial path. `session_isolated` is whether this call's session runs in a
/// daemon-created isolated worktree; `sandbox_available` is whether this
/// host can actually engage `horizon-sandbox`'s containment (checked, not
/// assumed -- see `horizon_sandbox::is_available`).
///
/// `config.write` always asks, regardless of isolation -- it edits
/// Horizon's own config file, not anything inside a session's workspace, so
/// worktree isolation buys it nothing.
fn classify_input(
    tool_id: &str,
    input: &ToolInput,
    session_isolated: bool,
    sandbox_available: bool,
) -> Classification {
    match tool_id {
        "fs.write" | "fs.edit" => {
            if session_isolated {
                Classification::Contained
            } else {
                Classification::AlwaysAsk
            }
        }
        "bash" => {
            if session_isolated && sandbox_available {
                if input.requires_metadata_write() {
                    Classification::AlwaysAsk
                } else {
                    Classification::Contained
                }
            } else {
                Classification::AlwaysAsk
            }
        }
        // Test-only fixture -- see `Classification::BoundaryCrossing`'s doc
        // comment. Not sensitive to `session_isolated`/`sandbox_available`:
        // a boundary crossing is defined by running outside the containment
        // perimeter regardless of this session's own isolation.
        "web_search" | "web_fetch" | "mock.boundary_crossing" => Classification::BoundaryCrossing,
        // `config.write`, `mock.approval_required`, and anything else this
        // crate ever catalogs as `RequireApproval` in the future: always
        // ask unless explicitly classified above -- the conservative
        // default the design doc asks for.
        _ => Classification::AlwaysAsk,
    }
}

fn boundary_input(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &ToolInput,
) -> BoundaryDisposition {
    match tool_id {
        "web_search" => BoundaryDisposition::Auto,
        "web_fetch" => match input {
            ToolInput::WebFetch(input) => match crate::tools::web::fetch_gate(tool_state, input) {
                crate::tools::web::FetchGate::NeedsApproval { .. } => BoundaryDisposition::Human,
                crate::tools::web::FetchGate::Invalid
                | crate::tools::web::FetchGate::Allowed { .. } => BoundaryDisposition::Auto,
            },
            _ => BoundaryDisposition::Auto,
        },
        _ => BoundaryDisposition::Human,
    }
}

/// The single policy decision used by approval display and execution dispatch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToolPlan {
    Automatic(AutomaticTool),
    Approval(Box<ApprovalRequest>),
    Reject(crate::tools::output::Response),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AutomaticTool {
    Synchronous,
    ContainedFilesystem,
    SandboxedBash,
    Web,
}

pub(crate) fn plan_prepared_call(
    tool_state: &ToolSessionState,
    request: &PreparedCall<'_>,
) -> ToolPlan {
    let approval = |reason, kind| {
        ToolPlan::Approval(Box::new(ApprovalRequest {
            call_id: request.call_id.clone(),
            occurrence_id: request.occurrence_id.clone(),
            reason,
            kind,
        }))
    };
    match crate::tools::permission_for_tool(&request.tool_id) {
        Some(ToolPermission::AutoAllowRead | ToolPermission::AutoAllowUi) => {
            if call_escapes_root(tool_state, &request.input) {
                let path = request.input.read_path().expect("validated escaping path");
                let verb = match request.tool_id.as_str() {
                    "fs.grep" => "search",
                    "fs.glob" => "find files in",
                    _ => "read",
                };
                approval(format!("`{}` requested to {verb} `{path}`, which is outside the session's workspace root. Allow this?", request.tool_id), ApprovalKind::Standard)
            } else {
                ToolPlan::Automatic(AutomaticTool::Synchronous)
            }
        }
        Some(ToolPermission::RequireApproval) => {
            match classify_input(
                &request.tool_id,
                &request.input,
                tool_state.is_isolated_worktree(),
                horizon_sandbox::is_available(),
            ) {
                Classification::Contained => ToolPlan::Automatic(if request.tool_id == "bash" {
                    AutomaticTool::SandboxedBash
                } else {
                    AutomaticTool::ContainedFilesystem
                }),
                Classification::BoundaryCrossing
                    if boundary_input(tool_state, &request.tool_id, &request.input)
                        == BoundaryDisposition::Auto =>
                {
                    ToolPlan::Automatic(AutomaticTool::Web)
                }
                Classification::BoundaryCrossing if request.tool_id == "web_fetch" => {
                    let domain = match &request.input {
                        ToolInput::WebFetch(input) => {
                            crate::tools::web::domain_grant_from_input(input)
                        }
                        _ => None,
                    };
                    let reason = domain.as_ref().map_or_else(
                        || "`web_fetch` requested an invalid or unavailable domain.".into(),
                        |domain| {
                            format!(
                                "Allow `{domain}` for this session and fetch the requested URL?"
                            )
                        },
                    );
                    approval(
                        reason,
                        ApprovalKind::DomainGrant {
                            domains: domain.into_iter().collect(),
                        },
                    )
                }
                Classification::BoundaryCrossing | Classification::AlwaysAsk => {
                    let (reason, kind) = git_operation_approval(tool_state, request)
                        .unwrap_or_else(|| {
                            (standard_approval_reason(request), ApprovalKind::Standard)
                        });
                    approval(reason, kind)
                }
            }
        }
        Some(ToolPermission::Deny) => ToolPlan::Reject(crate::tools::output::error(format!(
            "Tool `{}` is denied by Horizon policy.",
            request.tool_id
        ))),
        None => {
            let available = crate::tools::definitions()
                .into_iter()
                .map(|definition| definition.id)
                .collect::<Vec<_>>()
                .join(", ");
            ToolPlan::Reject(crate::tools::output::error(format!(
                "Unknown tool `{}`; available: {available}.",
                request.tool_id
            )))
        }
    }
}

fn standard_approval_reason(request: &ToolCallRequest) -> String {
    if request.tool_id == "bash" {
        "`bash` requested approval to run this call once outside Horizon's filesystem, network, \
         and process sandbox with the host process's ordinary authority. Later calls start \
         sandboxed again."
            .to_string()
    } else {
        format!(
            "`{}` requested Horizon approval for this tool call.",
            request.tool_id
        )
    }
}

fn git_operation_approval(
    tool_state: &ToolSessionState,
    request: &PreparedCall<'_>,
) -> Option<(String, ApprovalKind)> {
    if request.tool_id != "bash"
        || !tool_state.is_isolated_worktree()
        || !horizon_sandbox::is_available()
        || !request.input.requires_metadata_write()
    {
        return None;
    }
    let roots = tool_state
        .workspace_root()
        .ok_or_else(|| "the session has no workspace root".to_string())
        .and_then(crate::tools::metadata_writable_roots);
    Some(match roots {
        Ok(writable_roots) => {
            let displayed = writable_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!(
                    "`bash` requested a Git operation that may update shared repository metadata. \
                     Allow write access to {displayed} for this call and run it inside the sandbox?"
                ),
                ApprovalKind::GitOperation { writable_roots },
            )
        }
        Err(error) => (
            format!(
                "`bash` requested a Git operation, but Horizon could not derive a safe metadata \
                 grant ({error}). Approval will fail closed."
            ),
            ApprovalKind::GitOperation {
                writable_roots: Vec::new(),
            },
        ),
    })
}

#[cfg(test)]
pub(crate) fn plan_tool_call(state: &ToolSessionState, request: &ToolCallRequest) -> ToolPlan {
    match PreparedCall::new(request) {
        Ok(request) => plan_prepared_call(state, &request),
        Err(message) => ToolPlan::Reject(crate::tools::output::error(message)),
    }
}

#[cfg(test)]
fn classify_call(id: &str, input: &Value, isolated: bool, sandbox: bool) -> Classification {
    classify_input(
        id,
        &ToolInput::parse(id, input).unwrap_or(ToolInput::External),
        isolated,
        sandbox,
    )
}

#[cfg(test)]
fn boundary_disposition(state: &ToolSessionState, id: &str, input: &Value) -> BoundaryDisposition {
    boundary_input(
        state,
        id,
        &ToolInput::parse(id, input).unwrap_or(ToolInput::External),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Event, SessionId, SessionState};
    use crate::tools::test_support::policy_events;

    // --- classify_call: the trust predicate's classification table --------

    #[test]
    fn fs_mutations_are_contained_only_when_isolated() {
        let input = serde_json::json!({});
        for tool_id in ["fs.write", "fs.edit"] {
            assert_eq!(
                classify_call(tool_id, &input, true, false),
                Classification::Contained,
                "{tool_id} should be contained when isolated, regardless of sandbox availability"
            );
            assert_eq!(
                classify_call(tool_id, &input, true, true),
                Classification::Contained
            );
            assert_eq!(
                classify_call(tool_id, &input, false, false),
                Classification::AlwaysAsk,
                "{tool_id} must always-ask when the session isn't isolated"
            );
            assert_eq!(
                classify_call(tool_id, &input, false, true),
                Classification::AlwaysAsk,
                "{tool_id} isolation, not sandbox availability, is what fs tier 1 needs"
            );
        }
    }

    #[test]
    fn standard_bash_approval_reason_names_the_one_call_host_scope() {
        let request = ToolCallRequest {
            call_id: (crate::contract::ToolCallId("call-host-reason".to_string())).clone(),
            tool_id: "bash".to_string(),
            input: serde_json::json!({ "command": "cargo check" }).into(),
            occurrence_id: crate::contract::OccurrenceId(
                (crate::contract::ToolCallId("call-host-reason".to_string()))
                    .0
                    .clone(),
            ),
        };
        let reason = standard_approval_reason(&request);
        assert!(reason.contains("filesystem, network, and process sandbox"));
        assert!(reason.contains("host process's ordinary authority"));
        assert!(reason.contains("Later calls start sandboxed again"));
    }

    #[test]
    fn bash_is_contained_only_when_isolated_and_sandboxed() {
        let input = serde_json::json!({ "command": "echo hi" });
        assert_eq!(
            classify_call("bash", &input, true, true),
            Classification::Contained
        );
        assert_eq!(
            classify_call("bash", &input, true, false),
            Classification::AlwaysAsk,
            "isolated but no engaged sandbox must never silently degrade to auto-approve"
        );
        assert_eq!(
            classify_call("bash", &input, false, true),
            Classification::AlwaysAsk,
            "a sandbox alone (non-isolated session) is not enough for tier 1"
        );
        assert_eq!(
            classify_call("bash", &input, false, false),
            Classification::AlwaysAsk
        );
    }

    #[test]
    fn metadata_writing_git_asks_up_front_while_read_only_git_stays_contained() {
        assert_eq!(
            classify_call(
                "bash",
                &serde_json::json!({ "command": "git add src/lib.rs && git commit -m change" }),
                true,
                true,
            ),
            Classification::AlwaysAsk
        );
        assert_eq!(
            classify_call(
                "bash",
                &serde_json::json!({ "command": "git status --short && git diff --stat" }),
                true,
                true,
            ),
            Classification::Contained
        );
    }

    #[test]
    fn config_write_always_asks_regardless_of_isolation_or_sandbox() {
        let input = serde_json::json!({ "content": "" });
        for session_isolated in [false, true] {
            for sandbox_available in [false, true] {
                assert_eq!(
                    classify_call("config.write", &input, session_isolated, sandbox_available),
                    Classification::AlwaysAsk
                );
            }
        }
    }

    #[test]
    fn unknown_and_test_tool_ids_default_to_always_ask() {
        let input = serde_json::json!({});
        assert_eq!(
            classify_call("mock.approval_required", &input, true, true),
            Classification::AlwaysAsk
        );
        assert_eq!(
            classify_call("some.future.tool", &input, true, true),
            Classification::AlwaysAsk
        );
    }

    #[test]
    fn mock_boundary_crossing_is_always_a_boundary_crossing() {
        let input = serde_json::json!({});
        for session_isolated in [false, true] {
            for sandbox_available in [false, true] {
                assert_eq!(
                    classify_call(
                        "mock.boundary_crossing",
                        &input,
                        session_isolated,
                        sandbox_available
                    ),
                    Classification::BoundaryCrossing
                );
            }
        }
    }

    #[test]
    fn web_tools_are_boundary_crossings_with_per_call_dispositions() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        assert_eq!(
            classify_call("web_search", &serde_json::json!({}), false, false),
            Classification::BoundaryCrossing
        );
        assert_eq!(
            boundary_disposition(&tool_state, "web_search", &serde_json::json!({})),
            BoundaryDisposition::Auto
        );
        let fetch = serde_json::json!({ "url": "https://example.com/docs" });
        assert_eq!(
            boundary_disposition(&tool_state, "web_fetch", &fetch),
            BoundaryDisposition::Human
        );
        tool_state.allow_domain("example.com");
        assert_eq!(
            boundary_disposition(&tool_state, "web_fetch", &fetch),
            BoundaryDisposition::Auto
        );
    }

    // --- plan_tool_call ---------------------------------

    fn requested(tool_id: &str) -> Event {
        requested_with_input(tool_id, serde_json::json!({}))
    }

    fn requested_with_input(tool_id: &str, input: Value) -> Event {
        Event::ToolCallRequested(crate::contract::ToolCallRequest {
            call_id: (crate::contract::ToolCallId("call-1".to_string())).clone(),
            tool_id: tool_id.to_string(),
            input: input.into(),
            occurrence_id: crate::contract::OccurrenceId(
                (crate::contract::ToolCallId("call-1".to_string()))
                    .0
                    .clone(),
            ),
        })
    }

    #[test]
    fn web_search_auto_crosses_without_a_human_prompt() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let events = policy_events(
            &requested_with_input("web_search", serde_json::json!({ "query": "rust" })),
            &tool_state,
            SessionId::new(),
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
    }

    #[test]
    fn web_fetch_prompts_for_exact_domain_then_reuses_the_session_grant() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let request = requested_with_input(
            "web_fetch",
            serde_json::json!({ "url": "https://Docs.Example.com/page" }),
        );
        let events = policy_events(&request, &tool_state, SessionId::new());
        assert!(events.iter().any(|event| matches!(
            event,
            Event::ApprovalRequested(ApprovalRequest {
                kind: ApprovalKind::DomainGrant { domains },
                ..
            }) if domains == &["docs.example.com".to_string()]
        )));

        tool_state.allow_domain("example.com");
        let events = policy_events(&request, &tool_state, SessionId::new());
        assert!(events.iter().any(|event| matches!(
            event,
            Event::ApprovalRequested(ApprovalRequest {
                kind: ApprovalKind::DomainGrant { domains },
                ..
            }) if domains == &["docs.example.com".to_string()]
        )));

        tool_state.allow_domain("docs.example.com");
        let events = policy_events(&request, &tool_state, SessionId::new());
        assert!(!events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
    }

    #[test]
    fn invalid_web_fetch_input_fails_without_a_meaningless_human_prompt() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let events = policy_events(
            &requested_with_input(
                "web_fetch",
                serde_json::json!({ "url": "file:///etc/passwd" }),
            ),
            &tool_state,
            SessionId::new(),
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
    }

    #[test]
    fn contained_fs_write_in_an_isolated_session_gets_no_approval_prompt() {
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_isolated_worktree(true)
            .build();
        let events = policy_events(
            &requested_with_input(
                "fs.write",
                serde_json::json!({"path": "/tmp/example", "content": "hello"}),
            ),
            &tool_state,
            SessionId::new(),
        );

        assert_eq!(
            events.len(),
            1,
            "only the original event, no approval prompt: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
    }

    #[test]
    fn non_isolated_fs_write_still_gets_the_ordinary_approval_prompt() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let events = policy_events(
            &requested_with_input(
                "fs.write",
                serde_json::json!({"path": "/tmp/example", "content": "hello"}),
            ),
            &tool_state,
            SessionId::new(),
        );

        assert!(events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::StateChanged(SessionState::WaitingForApproval))));
    }

    #[test]
    fn an_unknown_tool_id_never_gets_an_approval_prompt() {
        // The 2026-07-19 dogfooding bug: an unrecognized tool id (not in
        // `tools::catalog::definitions` at all, e.g. the model calling
        // `write` instead of `fs.write`) used to default to
        // `ToolPermission::RequireApproval`, reaching a real human approval
        // prompt for a tool call that could never actually run.
        // `tools::execution::execute_agent_tool` (exercised separately, on
        // this same event, by `tools::processing::
        // process_agent_provider_event`) is the one place this now resolves
        // -- immediately, with a `ToolCallFinished` error result -- so this
        // seam must contribute nothing beyond the original event.
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_isolated_worktree(true)
            .build();
        let events = policy_events(&requested("write"), &tool_state, SessionId::new());

        assert_eq!(
            events.len(),
            1,
            "only the original event, nothing else: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
    }

    #[test]
    fn mock_approval_required_always_gets_a_prompt_even_when_isolated() {
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_isolated_worktree(true)
            .build();
        let events = policy_events(
            &requested("mock.approval_required"),
            &tool_state,
            SessionId::new(),
        );

        assert!(events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
    }

    /// Both policy classes derive the same typed candidate shape.
    /// `horizon-agentd` gates that candidate after this pure policy mapping
    /// returns.
    #[test]
    fn boundary_crossing_produces_the_same_events_as_always_ask() {
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_isolated_worktree(true)
            .build();
        let session_id = SessionId::new();

        let boundary_events = policy_events(
            &requested("mock.boundary_crossing"),
            &tool_state,
            session_id,
        );
        let always_ask_events = policy_events(
            &requested("mock.approval_required"),
            &tool_state,
            session_id,
        );

        // Compare shape (event kind + reason/kind fields), not the tool id
        // each carries in its own `ToolCallRequested`/`ApprovalRequested`.
        assert_eq!(boundary_events.len(), always_ask_events.len());
        assert_eq!(
            boundary_events.len(),
            3,
            "request + approval + state change"
        );
        assert!(matches!(boundary_events[0], Event::ToolCallRequested(_)));
        assert!(matches!(boundary_events[1], Event::ApprovalRequested(_)));
        assert!(matches!(
            boundary_events[2],
            Event::StateChanged(SessionState::WaitingForApproval)
        ));
        if let (Event::ApprovalRequested(boundary), Event::ApprovalRequested(always_ask)) =
            (&boundary_events[1], &always_ask_events[1])
        {
            assert_eq!(boundary.kind, always_ask.kind);
        } else {
            panic!("both must be ApprovalRequested");
        }
    }
}
