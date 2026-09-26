use super::*;

// --- audit markers ------------------------------------------------------

#[test]
fn annotate_auto_approval_adds_tier_and_reason() {
    let mut output = Response::succeeded(FileWritten {
        path: "/tmp/x".into(),
        bytes_written: 0,
        created: false,
    });
    annotate_auto_approval(&mut output, "contained", "isolated worktree session");

    assert_eq!(output.to_json()["auto_approved"], true);
    assert_eq!(output.to_json()["policy_tier"], "contained");
    assert_eq!(
        output.to_json()["policy_reason"],
        "isolated worktree session"
    );
}

#[test]
fn annotate_sandboxed_records_the_flag() {
    let mut output = Response::succeeded(BashOutput {
        termination: BashTermination::Exited { exit_code: 0 },
        output: String::new(),
        truncated: false,
        output_file: None,
        message: None,
        note: None,
    });
    annotate_sandboxed(&mut output, true);
    assert_eq!(output.to_json()["sandboxed"], true);

    let mut output = Response::succeeded(BashOutput {
        termination: BashTermination::Exited { exit_code: 0 },
        output: String::new(),
        truncated: false,
        output_file: None,
        message: None,
        note: None,
    });
    annotate_sandboxed(&mut output, false);
    assert_eq!(output.to_json()["sandboxed"], false);
}

#[test]
fn containment_evidence_overrides_success_without_interpreting_payload_flags() {
    use crate::contract::{OccurrenceId, ToolCallId, ToolCallIdentity, ToolOutcome};
    let identity = ToolCallIdentity {
        call_id: ToolCallId("bash".into()),
        occurrence_id: OccurrenceId::new(),
    };
    let mut response = Response::succeeded(BashOutput {
        termination: BashTermination::Exited { exit_code: 0 },
        output: "is_error: true; denied by user".into(),
        truncated: false,
        output_file: None,
        message: None,
        note: None,
    });
    assert_eq!(
        identity.finish(response.clone()).outcome,
        ToolOutcome::Succeeded
    );
    annotate_denied_domains(&mut response, &["example.test".into()]);
    let result = identity.finish(response);
    assert_eq!(result.outcome, ToolOutcome::Failed);
    let decoded: crate::contract::ToolCallResult =
        serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
    assert_eq!(decoded, result);
    let body = decode::<BashOutput>(&decoded.output).unwrap();
    assert_eq!(body.exit_code(), Some(0));
    assert_eq!(
        decoded.output["denied_domains"],
        serde_json::json!(["example.test"])
    );
}
