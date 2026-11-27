//! The approval trust model's policy seam (`docs/agent-approval-design.md`).
//! `plan_tool_call` selects automatic execution, an exact approval candidate,
//! or a terminal rejection. The execution coordinator consumes this decision.

use crate::tools::input::{PreparedCall, ToolInput};

#[cfg(test)]
use serde_json::Value;

use crate::contract::{ApprovalKind, ApprovalRequest, ToolCallRequest, ToolPermission};
use crate::modules::{Containment, Reach};
use crate::tools::ToolSessionState;
use crate::tools::call_escapes_root;

/// A per-call trust classification -- the tier a single tool call falls
/// into, not a static per-tool-id policy. See the design doc's "The three
/// tiers".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Classification {
    /// Auto by construction (tier 1): runs without asking. Containment
    /// stands in for consent: the OS sandbox for `bash`, the tool's own
    /// workspace-root path check for `fs_write`/`fs_edit`
    /// (`docs/sandbox-perimeter-design.md`).
    Contained,
    /// Crosses the containment boundary (tier 2 --
    /// `docs/agent-approval-design.md`'s "Judge at the boundary"): the
    /// judge's canonical case (MCP/non-sandboxed tools; network egress is
    /// excluded, see leg 4b's own `DomainDenialRetry` path). No *real*
    /// tool in this crate's catalog is classified this way today (there are
    /// no MCP/external tools wired in yet); `mock_boundary_crossing` is the
    /// fixture that exercises this classification until a real
    /// boundary-crossing tool exists.
    BoundaryCrossing,
    /// Always human (tier 3): irreversible/destructive by policy, a Git
    /// metadata write, or a `bash` call on a host with no engaged sandbox.
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

/// The per-call trust predicate: pure, conservative, and explicit, by tool
/// id. `bash` is decided by [`bash_reach`] from its input instead.
///
/// `fs_write`/`fs_edit` do not run in the OS sandbox; their confinement is
/// the tool's own workspace-root path check, so sandbox availability does
/// not affect them. A target inside the repository's gitdir is decided
/// before this classification (`plan_prepared_call`).
fn classify_input(tool_id: &str) -> Classification {
    match tool_id {
        "fs_write" | "fs_edit" => Classification::Contained,
        // Test-only fixture -- see `Classification::BoundaryCrossing`'s doc
        // comment. Not sensitive to `sandbox_available`: a boundary crossing
        // is defined by running outside the containment perimeter.
        "web_search" | "web_fetch" | "mock_boundary_crossing" => Classification::BoundaryCrossing,
        // `mock_approval_required`, and anything else this
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
    Automatic,
    Approval(Box<ApprovalRequest>),
    Reject(crate::tools::output::Response),
}

/// How a call to whichever tool it names runs: what the session's
/// verdict modules decide from what the tool says it reaches
/// (`modules::Reach`), or a refusal naming the tools there are when no tool
/// has that id.
pub(crate) fn plan_prepared_call(
    tool_state: &ToolSessionState,
    request: &PreparedCall<'_>,
) -> ToolPlan {
    match crate::tools::tool::find(tool_state.packages(), &request.tool_id) {
        Some(tool) => {
            let reach = tool.reach(tool_state, request);
            crate::modules::verdict(
                tool_state.verdict_modules(),
                &crate::modules::CallContext {
                    state: tool_state,
                    call: request,
                    reach: &reach,
                },
            )
        }
        None => {
            let available = crate::tools::definitions(tool_state.packages())
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

/// What a call to a built-in tool reaches, in one table keyed by tool id.
pub(crate) fn builtin_reach(tool_state: &ToolSessionState, request: &PreparedCall<'_>) -> Reach {
    let tool_id = request.tool_id.as_str();
    let sandbox = crate::tools::sandbox::ready();
    let permission = crate::tools::permission_for_tool(tool_id);
    let primary_checkout = matches!(tool_id, "fs_write" | "fs_edit")
        .then(|| tool_state.primary_checkout_without_write_access())
        .flatten()
        .map(std::path::Path::to_path_buf);
    let (containment, ask) = match permission {
        Some(ToolPermission::AutoAllowRead | ToolPermission::AutoAllowUi) => {
            if call_escapes_root(tool_state, &request.input) {
                let path = request.input.read_path().expect("validated escaping path");
                let verb = match tool_id {
                    "fs_grep" => "search",
                    "fs_glob" => "find files in",
                    _ => "read",
                };
                (
                    Containment::ReadsOutside,
                    Some((
                        format!(
                            "`{tool_id}` requested to {verb} `{path}`, which is outside the \
                             session's workspace root. Allow this?"
                        ),
                        ApprovalKind::Standard,
                    )),
                )
            } else {
                (Containment::Inside, None)
            }
        }
        Some(ToolPermission::RequireApproval) if tool_id == "bash" => {
            bash_reach(tool_state, request, sandbox)
        }
        Some(ToolPermission::RequireApproval) => match classify_input(tool_id) {
            Classification::Contained => (Containment::Contained, None),
            Classification::BoundaryCrossing
                if boundary_input(tool_state, tool_id, &request.input)
                    == BoundaryDisposition::Auto =>
            {
                (Containment::CrossesBoundary { allowed: true }, None)
            }
            Classification::BoundaryCrossing if tool_id == "web_fetch" => {
                let domain = match &request.input {
                    ToolInput::WebFetch(input) => crate::tools::web::domain_grant_from_input(input),
                    _ => None,
                };
                let reason = domain.as_ref().map_or_else(
                    || "`web_fetch` requested an invalid or unavailable domain.".into(),
                    |domain| {
                        format!("Allow `{domain}` for this session and fetch the requested URL?")
                    },
                );
                (
                    Containment::CrossesBoundary { allowed: false },
                    Some((
                        reason,
                        ApprovalKind::DomainGrant {
                            domains: domain.into_iter().collect(),
                        },
                    )),
                )
            }
            classification => (
                if classification == Classification::BoundaryCrossing {
                    Containment::CrossesBoundary { allowed: false }
                } else {
                    Containment::Uncontained
                },
                Some((standard_approval_reason(request), ApprovalKind::Standard)),
            ),
        },
        Some(ToolPermission::Deny) | None => (Containment::Uncontained, None),
    };
    let git_metadata = (permission == Some(ToolPermission::RequireApproval))
        .then(|| crate::tools::call_writes_git_metadata(tool_state, &request.input))
        .flatten()
        .map(str::to_string);
    Reach {
        permission,
        containment,
        ask,
        uncontained_write: writes_without_containment(tool_id, &request.input, sandbox),
        primary_checkout,
        git_metadata,
    }
}

/// Whether `tool_id` writes in a way a read-only session's sandbox cannot
/// refuse after the fact (`tools::ReadOnly`): the file tools run outside
/// the sandbox, an area request asks for write access itself, and `bash`
/// with no sandbox to contain it would run on the host. A sandboxed `bash`
/// call is not among them: it runs with no writable root.
fn writes_without_containment(tool_id: &str, input: &ToolInput, sandbox_available: bool) -> bool {
    match (tool_id, input) {
        ("fs_write" | "fs_edit", _) => true,
        ("bash", ToolInput::Bash(input)) => input.outside_sandbox || !sandbox_available,
        ("bash", _) => !sandbox_available,
        _ => false,
    }
}

/// What a `bash` call reaches (`docs/sandbox-runtime-design.md`, "The
/// failure path"): contained when the sandbox is ready and the call asks for
/// nothing more; the host, asked for, when it asks to run outside the sandbox
/// or there is no sandbox; contained but with hosts beyond the session's
/// domains, asked for, when it names some.
fn bash_reach(
    tool_state: &ToolSessionState,
    request: &PreparedCall<'_>,
    sandbox_available: bool,
) -> (Containment, Option<(String, ApprovalKind)>) {
    let ToolInput::Bash(input) = &request.input else {
        return (
            Containment::Uncontained,
            Some((standard_approval_reason(request), ApprovalKind::Standard)),
        );
    };
    if input.outside_sandbox || !sandbox_available {
        return (
            Containment::Uncontained,
            Some((standard_approval_reason(request), ApprovalKind::Standard)),
        );
    }
    let domains: Vec<String> = input
        .domains
        .iter()
        .filter(|domain| !tool_state.is_domain_allowed(domain))
        .cloned()
        .collect();
    if domains.is_empty() {
        return (Containment::Contained, None);
    }
    (
        Containment::CrossesBoundary { allowed: false },
        Some((
            format!(
                "`bash` asks to reach {} for this one call, from inside the sandbox. \
                 Allow it?",
                domains
                    .iter()
                    .map(|domain| format!("`{domain}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ApprovalKind::DomainGrant { domains },
        )),
    )
}

fn standard_approval_reason(request: &ToolCallRequest) -> String {
    if request.tool_id == "bash" {
        "`bash` requested approval to run this call once outside the sandbox, with the host \
         process's ordinary authority. Later calls run sandboxed again."
            .to_string()
    } else {
        format!(
            "`{}` requested Horizon approval for this tool call.",
            request.tool_id
        )
    }
}
#[cfg(test)]
pub(crate) fn plan_tool_call(state: &ToolSessionState, request: &ToolCallRequest) -> ToolPlan {
    match PreparedCall::new(request) {
        Ok(request) => plan_prepared_call(state, &request),
        Err(message) => ToolPlan::Reject(crate::tools::output::error(message)),
    }
}

#[cfg(test)]
fn classify_call(id: &str, _input: &Value, _sandbox: bool) -> Classification {
    classify_input(id)
}

/// What a `bash` call with `input` reaches, with the sandbox ready or not.
#[cfg(test)]
fn bash_reach_of(
    state: &ToolSessionState,
    input: Value,
    sandbox: bool,
) -> (Containment, Option<(String, ApprovalKind)>) {
    let request = ToolCallRequest {
        call_id: crate::contract::ToolCallId("call-bash".into()),
        tool_id: "bash".into(),
        input: input.into(),
        occurrence_id: crate::contract::OccurrenceId("call-bash".into()),
    };
    let prepared = PreparedCall::new(&request).expect("valid bash input");
    bash_reach(state, &prepared, sandbox)
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
    fn fs_mutations_are_contained_regardless_of_sandbox_availability() {
        let input = serde_json::json!({});
        for tool_id in ["fs_write", "fs_edit"] {
            for sandbox in [false, true] {
                assert_eq!(
                    classify_call(tool_id, &input, sandbox),
                    Classification::Contained,
                    "{tool_id} is confined by its own root check, not the OS sandbox"
                );
            }
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
        assert!(reason.contains("outside the sandbox"));
        assert!(reason.contains("host process's ordinary authority"));
        assert!(reason.contains("Later calls run sandboxed again"));
    }

    #[test]
    fn bash_is_contained_exactly_when_the_sandbox_is_ready_and_nothing_more_is_asked() {
        let state = ToolSessionState::new(std::env::temp_dir());
        for command in ["echo hi", "git add src/lib.rs && git commit -m change"] {
            let (containment, ask) =
                bash_reach_of(&state, serde_json::json!({ "command": command }), true);
            assert_eq!(containment, Containment::Contained, "{command}");
            assert_eq!(ask, None);
        }
        let (containment, ask) =
            bash_reach_of(&state, serde_json::json!({ "command": "echo hi" }), false);
        assert_eq!(
            containment,
            Containment::Uncontained,
            "no ready sandbox must never silently degrade to auto-approve"
        );
        assert_eq!(ask.map(|(_, kind)| kind), Some(ApprovalKind::Standard));
    }

    #[test]
    fn a_bash_call_outside_the_sandbox_asks_for_one_host_execution() {
        let state = ToolSessionState::new(std::env::temp_dir());
        let (containment, ask) = bash_reach_of(
            &state,
            serde_json::json!({ "command": "cargo publish", "outside_sandbox": true }),
            true,
        );
        assert_eq!(containment, Containment::Uncontained);
        let (reason, kind) = ask.expect("asks");
        assert_eq!(kind, ApprovalKind::Standard);
        assert!(reason.contains("outside the sandbox"), "{reason}");
    }

    #[test]
    fn hosts_a_bash_call_names_are_asked_for_unless_the_session_has_them() {
        let state = ToolSessionState::new(std::env::temp_dir());
        state.allow_domain("crates.io");
        let (containment, ask) = bash_reach_of(
            &state,
            serde_json::json!({ "command": "cargo fetch", "domains": ["crates.io"] }),
            true,
        );
        assert_eq!(containment, Containment::Contained);
        assert_eq!(ask, None);

        let (containment, ask) = bash_reach_of(
            &state,
            serde_json::json!({
                "command": "cargo fetch",
                "domains": ["crates.io", "static.example.com"],
            }),
            true,
        );
        assert_eq!(containment, Containment::CrossesBoundary { allowed: false });
        let (reason, kind) = ask.expect("asks");
        assert_eq!(
            kind,
            ApprovalKind::DomainGrant {
                domains: vec!["static.example.com".into()],
            },
            "only the hosts the session lacks are asked for"
        );
        assert!(reason.contains("for this one call"), "{reason}");
    }

    #[test]
    fn unknown_and_test_tool_ids_default_to_always_ask() {
        let input = serde_json::json!({});
        assert_eq!(
            classify_call("mock_approval_required", &input, true),
            Classification::AlwaysAsk
        );
        assert_eq!(
            classify_call("some.future.tool", &input, true),
            Classification::AlwaysAsk
        );
    }

    #[test]
    fn mock_boundary_crossing_is_always_a_boundary_crossing() {
        let input = serde_json::json!({});
        for sandbox_available in [false, true] {
            assert_eq!(
                classify_call("mock_boundary_crossing", &input, sandbox_available),
                Classification::BoundaryCrossing
            );
        }
    }

    #[test]
    fn web_tools_are_boundary_crossings_with_per_call_dispositions() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        assert_eq!(
            classify_call("web_search", &serde_json::json!({}), false),
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
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_)))
        );
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
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_)))
        );
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
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_)))
        );
    }

    #[test]
    fn fs_write_in_a_non_isolated_session_gets_no_approval_prompt() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let events = policy_events(
            &requested_with_input(
                "fs_write",
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
    }

    #[test]
    fn reads_under_a_filesystem_grant_or_of_a_bash_spill_file_do_not_ask() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let granted = outside.path().canonicalize().unwrap().join("registry");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::write(granted.join("Cargo.toml"), "[package]").unwrap();
        let spill = std::env::temp_dir().join(format!("horizon-bash-{}.log", uuid::Uuid::new_v4()));
        std::fs::write(&spill, "full output").unwrap();
        let not_spill = outside.path().join("horizon-bash-elsewhere.log");
        std::fs::write(&not_spill, "x").unwrap();
        let tool_state = ToolSessionState::new(root.path().canonicalize().unwrap());
        let read = |path: &std::path::Path| {
            plan_tool_call(
                &tool_state,
                &ToolCallRequest {
                    call_id: crate::contract::ToolCallId("call-read".into()),
                    tool_id: "fs_read".into(),
                    input: serde_json::json!({ "path": path }).into(),
                    occurrence_id: crate::contract::OccurrenceId("call-read".into()),
                },
            )
        };
        let target = granted.join("Cargo.toml");
        std::assert_matches!(read(&target), ToolPlan::Approval(_));
        std::assert_matches!(read(&not_spill), ToolPlan::Approval(_));
        assert_eq!(read(&spill), ToolPlan::Automatic);

        let tool_state = crate::tools::ToolSessionBuilder::new(root.path().canonicalize().unwrap())
            .with_filesystem_grants(vec![horizon_sandbox::FilesystemGrant {
                path: outside.path().to_path_buf(),
                access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
                scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
                excluded_subpaths: Vec::new(),
            }])
            .build();
        assert_eq!(
            plan_tool_call(
                &tool_state,
                &ToolCallRequest {
                    call_id: crate::contract::ToolCallId("call-read".into()),
                    tool_id: "fs_read".into(),
                    input: serde_json::json!({ "path": target }).into(),
                    occurrence_id: crate::contract::OccurrenceId("call-read".into()),
                },
            ),
            ToolPlan::Automatic
        );
        let _ = std::fs::remove_file(spill);
    }

    #[test]
    fn fs_mutations_inside_the_plain_checkouts_gitdir_are_not_automatic() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        std::fs::create_dir_all(root_path.join(".git").join("hooks")).unwrap();
        std::fs::write(root_path.join("tracked.txt"), "before").unwrap();
        let tool_state = ToolSessionState::new(root_path.clone());
        let hook = root_path.join(".git/hooks/pre-commit");
        let tracked = root_path.join("tracked.txt");

        for (tool_id, input) in [
            (
                "fs_write",
                serde_json::json!({"path": hook, "content": "#!/bin/sh"}),
            ),
            (
                "fs_edit",
                serde_json::json!({"edits": [
                    {"path": tracked, "old_string": "before", "new_string": "after"},
                    {"path": root_path.join(".git/config"), "old_string": "a", "new_string": "b"},
                ]}),
            ),
        ] {
            let plan = plan_tool_call(
                &tool_state,
                &ToolCallRequest {
                    call_id: crate::contract::ToolCallId("call-gitdir".into()),
                    tool_id: tool_id.into(),
                    input: input.into(),
                    occurrence_id: crate::contract::OccurrenceId("call-gitdir".into()),
                },
            );
            let ToolPlan::Approval(approval) = plan else {
                panic!("{tool_id} into the gitdir must ask, got {plan:?}");
            };
            assert_eq!(approval.kind, ApprovalKind::Standard);
            assert!(
                approval.reason.contains("Git metadata directory"),
                "{}",
                approval.reason
            );
        }

        let plan = plan_tool_call(
            &tool_state,
            &ToolCallRequest {
                call_id: crate::contract::ToolCallId("call-tracked".into()),
                tool_id: "fs_write".into(),
                input: serde_json::json!({"path": tracked, "content": "after"}).into(),
                occurrence_id: crate::contract::OccurrenceId("call-tracked".into()),
            },
        );
        assert_eq!(plan, ToolPlan::Automatic);
    }

    #[test]
    fn bash_without_an_engaged_sandbox_falls_to_the_host_execution_approval() {
        if crate::tools::sandbox::ready() {
            // `plan_prepared_call` reads the live host's sandbox; the pure
            // half of this fallback is `bash_is_contained_exactly_when_the_
            // sandbox_is_ready_and_nothing_more_is_asked`.
            return;
        }
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let plan = plan_tool_call(
            &tool_state,
            &ToolCallRequest {
                call_id: crate::contract::ToolCallId("call-host".into()),
                tool_id: "bash".into(),
                input: serde_json::json!({"command": "echo hi"}).into(),
                occurrence_id: crate::contract::OccurrenceId("call-host".into()),
            },
        );
        let ToolPlan::Approval(approval) = plan else {
            panic!("bash without a sandbox must ask, got {plan:?}");
        };
        assert_eq!(approval.kind, ApprovalKind::Standard);
        assert!(
            approval
                .reason
                .contains("host process's ordinary authority")
        );
    }

    /// A session that may not write is refused the writes no sandbox can
    /// take back, before they run and with no approval to ask for, and is
    /// told why in its reason's words. Its reads still plan as before.
    #[test]
    fn a_session_that_may_not_write_is_refused_its_writes_without_an_approval() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        std::fs::write(root_path.join("tracked.txt"), "before").unwrap();
        let tracked = root_path.join("tracked.txt");
        let tool_state = crate::tools::ToolSessionBuilder::new(root_path.clone())
            .with_read_only(Some(crate::tools::ReadOnly::UntilOwnWorktree))
            .build();
        let plan = |tool_id: &str, input: serde_json::Value| {
            plan_tool_call(
                &tool_state,
                &ToolCallRequest {
                    call_id: crate::contract::ToolCallId(format!("call-{tool_id}")),
                    tool_id: tool_id.into(),
                    input: input.into(),
                    occurrence_id: crate::contract::OccurrenceId(format!("call-{tool_id}")),
                },
            )
        };

        for (tool_id, input) in [
            (
                "fs_write",
                serde_json::json!({"path": tracked, "content": "after"}),
            ),
            (
                "fs_edit",
                serde_json::json!({"edits": [
                    {"path": tracked, "old_string": "before", "new_string": "after"},
                ]}),
            ),
            (
                "bash",
                serde_json::json!({"command": "cargo build", "outside_sandbox": true}),
            ),
        ] {
            let ToolPlan::Reject(output) = plan(tool_id, input) else {
                panic!("{tool_id} must be refused before it runs");
            };
            let message = output.message().to_string();
            assert!(
                message.contains("worktree of its own"),
                "{tool_id}: {message}"
            );
        }
        assert_eq!(
            plan("fs_read", serde_json::json!({"path": tracked})),
            ToolPlan::Automatic
        );
        assert_eq!(std::fs::read_to_string(&tracked).unwrap(), "before");
    }

    /// A sandboxed `bash` call keeps running for a session that may not
    /// write -- its sandbox has nothing writable -- while one outside the
    /// sandbox would run on the host, and is refused.
    #[test]
    fn bash_writes_without_containment_exactly_when_it_runs_outside_the_sandbox() {
        let bash = |json: Value| {
            let request = ToolCallRequest {
                call_id: crate::contract::ToolCallId("call-bash".into()),
                tool_id: "bash".into(),
                input: json.into(),
                occurrence_id: crate::contract::OccurrenceId("call-bash".into()),
            };
            PreparedCall::new(&request).unwrap().input.clone()
        };
        let contained = bash(serde_json::json!({ "command": "ls" }));
        let outside = bash(serde_json::json!({ "command": "ls", "outside_sandbox": true }));
        assert!(!writes_without_containment("bash", &contained, true));
        assert!(writes_without_containment("bash", &contained, false));
        assert!(writes_without_containment("bash", &outside, true));
        for tool_id in ["fs_write", "fs_edit"] {
            assert!(
                writes_without_containment(tool_id, &ToolInput::External, true),
                "{tool_id}"
            );
        }
        for tool_id in ["fs_read", "web_fetch", "board_session", "task"] {
            assert!(
                !writes_without_containment(tool_id, &ToolInput::External, false),
                "{tool_id}"
            );
        }
    }

    #[test]
    fn primary_checkout_gates_fs_mutations_until_approved_while_bash_is_left_to_the_sandbox() {
        let root = tempfile::tempdir().unwrap();
        let tool_state = crate::tools::ToolSessionBuilder::new(root.path().to_path_buf())
            .with_primary_checkout(Some(root.path().to_path_buf()))
            .build();
        let request = |tool_id: &str, input: Value| ToolCallRequest {
            call_id: crate::contract::ToolCallId("call-primary".into()),
            tool_id: tool_id.into(),
            input: input.into(),
            occurrence_id: crate::contract::OccurrenceId("call-primary".into()),
        };

        let write = request(
            "fs_write",
            serde_json::json!({"path": root.path().join("src/lib.rs"), "content": "change"}),
        );
        let ToolPlan::Approval(approval) = plan_tool_call(&tool_state, &write) else {
            panic!("a primary-checkout fs write must ask before it runs");
        };
        assert_eq!(approval.kind, ApprovalKind::Standard);
        assert!(approval.reason.contains("shared primary checkout"));

        // bash is not classified by command text: the sandbox makes the
        // checkout read-only, so a write attempt becomes a containment
        // denial instead.
        assert_eq!(
            plan_tool_call(
                &tool_state,
                &request(
                    "bash",
                    serde_json::json!({"command": "sed -i 's/a/b/' src/lib.rs"})
                ),
            ),
            ToolPlan::Automatic
        );
        assert!(!tool_state.primary_checkout_writable());

        tool_state.approve_primary_checkout();
        assert!(tool_state.primary_checkout_writable());
        assert_eq!(plan_tool_call(&tool_state, &write), ToolPlan::Automatic);
    }

    #[test]
    fn an_unknown_tool_id_never_gets_an_approval_prompt() {
        // The 2026-07-19 dogfooding bug: an unrecognized tool id (not in
        // `tools::catalog::definitions` at all, e.g. the model calling
        // `write` instead of `fs_write`) used to default to
        // `ToolPermission::RequireApproval`, reaching a real human approval
        // prompt for a tool call that could never actually run.
        // `tools::execution::execute_agent_tool` (exercised separately, on
        // this same event, by `tools::processing::
        // process_agent_provider_event`) is the one place this now resolves
        // -- immediately, with a `ToolCallFinished` error result -- so this
        // seam must contribute nothing beyond the original event.
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir()).build();
        let events = policy_events(&requested("write"), &tool_state, SessionId::new());

        assert_eq!(
            events.len(),
            1,
            "only the original event, nothing else: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_)))
        );
    }

    #[test]
    fn mock_approval_required_always_gets_a_prompt_even_when_isolated() {
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir()).build();
        let events = policy_events(
            &requested("mock_approval_required"),
            &tool_state,
            SessionId::new(),
        );

        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_)))
        );
    }

    /// Both policy classes derive the same typed candidate shape.
    /// `horizon-agent-host` gates that candidate after this pure policy mapping
    /// returns.
    #[test]
    fn boundary_crossing_produces_the_same_events_as_always_ask() {
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir()).build();
        let session_id = SessionId::new();

        let boundary_events = policy_events(
            &requested("mock_boundary_crossing"),
            &tool_state,
            session_id,
        );
        let always_ask_events = policy_events(
            &requested("mock_approval_required"),
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
        std::assert_matches!(boundary_events[0], Event::ToolCallRequested(_));
        std::assert_matches!(boundary_events[1], Event::ApprovalRequested(_));
        std::assert_matches!(
            boundary_events[2],
            Event::StateChanged(SessionState::WaitingForApproval)
        );
        if let (Event::ApprovalRequested(boundary), Event::ApprovalRequested(always_ask)) =
            (&boundary_events[1], &always_ask_events[1])
        {
            assert_eq!(boundary.kind, always_ask.kind);
        } else {
            panic!("both must be ApprovalRequested");
        }
    }
}
