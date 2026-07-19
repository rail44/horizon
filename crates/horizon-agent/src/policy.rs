//! The approval trust model's policy seam (`docs/agent-approval-design.md`).
//! [`horizon_events_for_provider_event`]'s `RequireApproval` arm is the
//! single point where `Event::ApprovalRequested` is emitted for a
//! provider-requested tool call; [`classify_call`] is the per-call trust
//! predicate that arm consults, replacing the old per-tool-id-only
//! `ToolPermission::RequireApproval` ("bash always asks") with "this
//! particular call is contained, or it must ask".

use serde_json::Value;

use crate::contract::{
    ApprovalKind, ApprovalRequest, Error, Event, SessionId, SessionState, ToolPermission,
};
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
    /// fixture that exercises this classification and the shadow-mode
    /// judge wiring it drives (`judge::maybe_fire_shadow_judge`) until a
    /// real boundary-crossing tool exists.
    BoundaryCrossing,
    /// Always human (tier 3): irreversible/destructive by policy, or a
    /// contained-eligible tool call whose session isn't isolated / has no
    /// engaged sandbox available.
    AlwaysAsk,
}

/// The per-call trust predicate: pure, conservative, and explicit. `tool_id`
/// and `_input` are the call being classified (`_input` -- the raw call
/// arguments -- is reserved for a future per-argument boundary-crossing rule,
/// e.g. a network tool's target domain; no tool classified here needs it
/// yet); `session_isolated` is whether this call's session runs in a
/// daemon-created isolated worktree; `sandbox_available` is whether this
/// host can actually engage `horizon-sandbox`'s containment (checked, not
/// assumed -- see `horizon_sandbox::is_available`).
///
/// `config.write` always asks, regardless of isolation -- it edits
/// Horizon's own config file, not anything inside a session's workspace, so
/// worktree isolation buys it nothing.
pub(crate) fn classify_call(
    tool_id: &str,
    _input: &Value,
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
                Classification::Contained
            } else {
                Classification::AlwaysAsk
            }
        }
        // Test-only fixture -- see `Classification::BoundaryCrossing`'s doc
        // comment. Not sensitive to `session_isolated`/`sandbox_available`:
        // a boundary crossing is defined by running outside the containment
        // perimeter regardless of this session's own isolation.
        "mock.boundary_crossing" => Classification::BoundaryCrossing,
        // `config.write`, `mock.approval_required`, and anything else this
        // crate ever catalogs as `RequireApproval` in the future: always
        // ask unless explicitly classified above -- the conservative
        // default the design doc asks for.
        _ => Classification::AlwaysAsk,
    }
}

/// Merges Horizon's own auto-approval audit marker into a tool result's
/// `output` JSON -- additive only (`docs/agent-approval-design.md`'s
/// "Audit"): no new `Event` variant, just more keys on the existing `output`
/// object, immediately `json_extract`-queryable from the DuckDB projection
/// with no projection changes. A no-op if `output` isn't a JSON object
/// (every tool in this crate returns one, but this stays defensive rather
/// than panicking on a malformed caller).
pub(crate) fn annotate_auto_approval(output: &mut Value, tier: &str, reason: &str) {
    if let Some(map) = output.as_object_mut() {
        map.insert("auto_approved".to_string(), Value::Bool(true));
        map.insert("policy_tier".to_string(), Value::String(tier.to_string()));
        map.insert(
            "policy_reason".to_string(),
            Value::String(reason.to_string()),
        );
    }
}

/// Records whether a `bash` call actually ran under `horizon-sandbox`'s
/// containment -- additive, same convention as
/// [`annotate_auto_approval`]. Recorded for every bash result (manually
/// approved or auto-approved), not just auto-approved ones, so the audit
/// trail is honest either way.
pub(crate) fn annotate_sandboxed(output: &mut Value, sandboxed: bool) {
    if let Some(map) = output.as_object_mut() {
        map.insert("sandboxed".to_string(), Value::Bool(sandboxed));
    }
}

/// Records that a sandboxed `bash` call's network egress was refused by the
/// allowlist proxy for one or more domains (`docs/agent-approval-design.md`
/// leg 4b) -- additive, same convention as [`annotate_auto_approval`]/
/// [`annotate_sandboxed`]. Also forces `is_error: true`, overriding whatever
/// the wrapped shell pipeline's own exit-code-derived convention said:
/// backlog 59 -- a command like `curl ... | head` can exit `0` even though
/// the network call itself was refused, and a call the proxy actually
/// denied network access for is never a clean success from the agent's
/// point of view, whatever the shell's own exit code claims.
pub(crate) fn annotate_denied_domains(output: &mut Value, domains: &[String]) {
    if let Some(map) = output.as_object_mut() {
        map.insert(
            "denied_domains".to_string(),
            Value::Array(domains.iter().cloned().map(Value::String).collect()),
        );
        map.insert("is_error".to_string(), Value::Bool(true));
    }
}

/// Records that a human approved one or more network domains for this
/// session before this retry ran (`docs/agent-approval-design.md` leg 4b)
/// -- additive, same convention as [`annotate_auto_approval`]. Kept
/// distinct from `auto_approved`: this retry was a human decision, not an
/// auto-approval, so the audit trail must never conflate the two.
pub(crate) fn annotate_domain_approval(output: &mut Value, domains: &[String]) {
    if let Some(map) = output.as_object_mut() {
        map.insert("domain_approved".to_string(), Value::Bool(true));
        map.insert(
            "approved_domains".to_string(),
            Value::Array(domains.iter().cloned().map(Value::String).collect()),
        );
    }
}

pub fn horizon_events_for_provider_event(
    event: &Event,
    tool_state: &ToolSessionState,
    session_id: SessionId,
) -> Vec<Event> {
    let mut events = vec![event.clone()];
    if let Event::ToolCallRequested(request) = event {
        match crate::tools::permission_for_tool(&request.tool_id) {
            Some(ToolPermission::AutoAllowRead | ToolPermission::AutoAllowUi) => {}
            Some(ToolPermission::RequireApproval) => {
                let classification = classify_call(
                    &request.tool_id,
                    &request.input,
                    tool_state.is_isolated_worktree(),
                    horizon_sandbox::is_available(),
                );
                match classification {
                    // Contained: no approval prompt -- `tools::execution`'s
                    // own (separately computed, same predicate) classify
                    // call drives the actual auto-execution.
                    Classification::Contained => {}
                    Classification::BoundaryCrossing | Classification::AlwaysAsk => {
                        events.push(Event::ApprovalRequested(ApprovalRequest {
                            call_id: request.call_id.clone(),
                            reason: format!(
                                "`{}` requested Horizon approval for this tool call.",
                                request.tool_id
                            ),
                            kind: ApprovalKind::Standard,
                        }));
                        events.push(Event::StateChanged(SessionState::WaitingForApproval));
                        // Shadow-mode judge (`docs/agent-approval-design.md`'s
                        // "Judge design", implemented shadow-only per
                        // `crate::judge`'s module doc): fire-and-forget, after
                        // the events above are already decided -- this can
                        // never change what the human sees. Contained and
                        // AlwaysAsk calls never reach the judge (tier-1
                        // auto-approve and tier-3 irreversible are not its
                        // domain); network domain crossings are excluded by
                        // construction (leg 4b's `DomainDenialRetry` approval
                        // is emitted from an entirely separate seam in
                        // `horizon-sessiond`, never through this function).
                        if let Classification::BoundaryCrossing = classification {
                            crate::judge::maybe_fire_shadow_judge(tool_state, session_id, request);
                        }
                    }
                }
            }
            Some(ToolPermission::Deny) => {
                events.push(Event::Error(Error {
                    message: format!("Tool `{}` is denied by Horizon policy.", request.tool_id),
                }));
            }
            // An unknown tool id (not in `tools::catalog::definitions` at
            // all) must never reach a human approval prompt -- there is
            // nothing for a human to approve, and defaulting it to
            // `RequireApproval` (as this used to) was exactly the
            // 2026-07-19 dogfooding bug: the model called a nonexistent
            // `write` tool, a real `ApprovalRequested` reached the human,
            // and only *after* approving did the call fail with a bare
            // session `Event::Error` the model never saw as a tool outcome.
            // No event here at all: `tools::execution::execute_agent_tool`
            // (invoked separately, on this same `ToolCallRequested`, by
            // `tools::processing::process_agent_provider_event`) already
            // produces the one user- and model-visible outcome -- a
            // `ToolCallFinished` error result -- for this case.
            None => {}
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- classify_call: the trust predicate's classification table --------

    #[test]
    fn fs_write_and_edit_are_contained_only_when_isolated() {
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

    // --- horizon_events_for_provider_event ---------------------------------

    fn requested(tool_id: &str) -> Event {
        Event::ToolCallRequested(crate::contract::ToolCallRequest {
            call_id: crate::contract::ToolCallId("call-1".to_string()),
            tool_id: tool_id.to_string(),
            input: serde_json::json!({}),
        })
    }

    #[test]
    fn contained_fs_write_in_an_isolated_session_gets_no_approval_prompt() {
        let tool_state = ToolSessionState::new(std::env::temp_dir()).with_isolated_worktree(true);
        let events = horizon_events_for_provider_event(
            &requested("fs.write"),
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
        let events = horizon_events_for_provider_event(
            &requested("fs.write"),
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
        let tool_state = ToolSessionState::new(std::env::temp_dir()).with_isolated_worktree(true);
        let events =
            horizon_events_for_provider_event(&requested("write"), &tool_state, SessionId::new());

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
        let tool_state = ToolSessionState::new(std::env::temp_dir()).with_isolated_worktree(true);
        let events = horizon_events_for_provider_event(
            &requested("mock.approval_required"),
            &tool_state,
            SessionId::new(),
        );

        assert!(events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))));
    }

    /// The core shadow-mode guarantee: a `BoundaryCrossing`-classified call
    /// gets byte-for-byte the same events a plain `AlwaysAsk` call would --
    /// installing (and firing) a judge handle changes nothing about what
    /// the human sees. `judge_fires_for_a_boundary_crossing_call_but_not_a_
    /// contained_one` (in `judge`'s own tests) proves the judge really does
    /// activate for one and not the other; this test proves that activation
    /// has zero effect on this function's return value either way.
    #[test]
    fn boundary_crossing_produces_the_same_events_as_always_ask() {
        let tool_state = ToolSessionState::new(std::env::temp_dir()).with_isolated_worktree(true);
        let session_id = SessionId::new();

        let boundary_events = horizon_events_for_provider_event(
            &requested("mock.boundary_crossing"),
            &tool_state,
            session_id,
        );
        let always_ask_events = horizon_events_for_provider_event(
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

    // --- audit markers ------------------------------------------------------

    #[test]
    fn annotate_auto_approval_adds_tier_and_reason() {
        let mut output = serde_json::json!({ "path": "/tmp/x" });
        annotate_auto_approval(&mut output, "contained", "isolated worktree session");

        assert_eq!(output["auto_approved"], true);
        assert_eq!(output["policy_tier"], "contained");
        assert_eq!(output["policy_reason"], "isolated worktree session");
    }

    #[test]
    fn annotate_sandboxed_records_the_flag() {
        let mut output = serde_json::json!({ "exit_code": 0 });
        annotate_sandboxed(&mut output, true);
        assert_eq!(output["sandboxed"], true);

        let mut output = serde_json::json!({ "exit_code": 0 });
        annotate_sandboxed(&mut output, false);
        assert_eq!(output["sandboxed"], false);
    }
}
