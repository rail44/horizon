use super::*;

// --- audit markers ------------------------------------------------------

#[test]
fn annotate_auto_approval_adds_tier_and_reason() {
    let mut output = Response::succeeded(FileWritten {
        path: "/tmp/x".into(),
        bytes_written: 0,
        created: false,
        content_version: None,
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
fn refused_hosts_are_recorded_without_interpreting_payload_flags() {
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
    annotate_refused_hosts(&mut response, vec!["example.test".into()]);
    let result = identity.finish(response);
    // What the command itself did decides the outcome; a refused host is a
    // fact about the run, not a failure of its own.
    assert_eq!(result.outcome, ToolOutcome::Succeeded);
    let decoded: crate::contract::ToolCallResult =
        serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
    assert_eq!(decoded, result);
    assert_eq!(
        decoded.output["refused_hosts"],
        serde_json::json!(["example.test"])
    );
}
