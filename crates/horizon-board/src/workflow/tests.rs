use super::*;

fn task(key: &str, dependencies: &[&str]) -> PlannedTask {
    PlannedTask {
        key: key.into(),
        title: key.into(),
        instructions: "Implement and test".into(),
        acceptance: vec!["The behavior works".into()],
        depends_on: dependencies.iter().map(|s| (*s).into()).collect(),
    }
}

fn plan() -> Plan {
    Plan {
        summary: "A concrete goal".into(),
        acceptance: vec!["Goal works end to end".into()],
        tasks: vec![task("b", &["a"]), task("a", &[])],
        decisions: vec![],
    }
}

fn apply_to(flow: &mut Workflow, mutation: Mutation) {
    *flow = apply(Some(flow), mutation).unwrap();
}

fn start(flow: &mut Workflow) {
    let work = flow.next_work().unwrap();
    apply_to(
        flow,
        Mutation::Start {
            token: "attempt".into(),
            session: "session".into(),
            work,
        },
    );
}

fn report(flow: &mut Workflow, report: Report) {
    apply_to(
        flow,
        Mutation::Report {
            token: "attempt".into(),
            session: "session".into(),
            report,
        },
    );
}

fn finish(flow: &mut Workflow) {
    apply_to(
        flow,
        Mutation::Finish {
            token: "attempt".into(),
        },
    );
}

#[test]
fn goal_answer_replan_dependencies_and_results_form_one_flow() {
    let mut flow = apply(None, Mutation::Enable).unwrap();
    let mut proposed = plan();
    proposed.decisions.push(Decision {
        key: "scope".into(),
        question: "Include offline use?".into(),
        context: "Offline storage changes the implementation".into(),
        recommendation: "Start online".into(),
        consequence: "Offline users cannot use it yet".into(),
    });
    start(&mut flow);
    report(
        &mut flow,
        Report::Plan {
            plan: proposed.clone(),
        },
    );
    assert!(
        flow.next_work().is_none(),
        "a tool report must not start another turn"
    );
    finish(&mut flow);
    assert_eq!(flow.label(), "needs your answer");
    assert!(flow.next_work().is_none());
    apply_to(
        &mut flow,
        Mutation::Answer {
            key: "scope".into(),
            text: "Online is enough for now".into(),
        },
    );
    assert_eq!(flow.next_work(), Some(Work::Plan));
    start(&mut flow);
    report(&mut flow, Report::Plan { plan: proposed });
    finish(&mut flow);
    assert_eq!(flow.next_work(), Some(Work::Task { key: "a".into() }));
    for key in ["a", "b"] {
        assert_eq!(flow.next_work(), Some(Work::Task { key: key.into() }));
        start(&mut flow);
        report(
            &mut flow,
            Report::Task {
                summary: "Implemented".into(),
                checks: vec!["cargo test: passed".into()],
            },
        );
        assert!(flow.results.iter().all(|r| r.key != key));
        finish(&mut flow);
    }
    assert_eq!(flow.results.len(), 2);
    assert_eq!(flow.label(), "review results");
    assert!(flow.next_work().is_none());
}

#[test]
fn cycles_unknown_dependencies_and_rewriting_completed_work_are_rejected() {
    let mut flow = apply(None, Mutation::Enable).unwrap();
    let mut bad = plan();
    bad.tasks[1].depends_on.push("b".into());
    assert!(validate_plan(&flow, &bad).unwrap_err().contains("cycle"));
    bad.tasks[1].depends_on = vec!["missing".into()];
    assert!(validate_plan(&flow, &bad).is_err());
    flow.plan = Some(plan());
    flow.results.push(TaskResult {
        key: "a".into(),
        session: "s".into(),
        summary: "Done".into(),
        checks: vec![],
    });
    let mut revised = plan();
    revised.tasks[1].instructions = "Different scope".into();
    assert!(validate_plan(&flow, &revised)
        .unwrap_err()
        .contains("Preserve completed"));
}

#[test]
fn stopped_paused_and_foreign_attempts_cannot_silently_advance() {
    let mut flow = apply(None, Mutation::Enable).unwrap();
    start(&mut flow);
    let wrong = Mutation::Report {
        token: "attempt".into(),
        session: "other".into(),
        report: Report::Plan { plan: plan() },
    };
    assert!(apply(Some(&flow), wrong).is_err());
    apply_to(&mut flow, Mutation::Pause);
    assert!(apply(
        Some(&flow),
        Mutation::Report {
            token: "attempt".into(),
            session: "session".into(),
            report: Report::Plan { plan: plan() }
        }
    )
    .is_err());
    finish(&mut flow);
    assert!(flow.problem.is_some());
    assert!(flow.next_work().is_none());
    apply_to(&mut flow, Mutation::Resume);
    start(&mut flow);
    apply_to(
        &mut flow,
        Mutation::Interrupt {
            token: "attempt".into(),
            reason: "Daemon restarted".into(),
        },
    );
    assert_eq!(flow.label(), "blocked");
    assert!(flow.next_work().is_none());
    assert!(apply(
        Some(&flow),
        Mutation::Finish {
            token: "attempt".into()
        }
    )
    .is_err());
}

#[test]
fn milestone_operations_and_legacy_items_roundtrip_the_actual_binary_codec() {
    use crate::wire::{IngestReply, IngestRequest};
    use horizon_wire::WireCodec;
    use remoc::codec::Codec;
    let mut flow = apply(None, Mutation::Enable).unwrap();
    start(&mut flow);
    report(&mut flow, Report::Plan { plan: plan() });
    let request = IngestRequest::Workflow {
        id: 1,
        expected_revision: flow.revision,
        mutation: Mutation::Report {
            token: "token".into(),
            session: "session".into(),
            report: Report::Plan { plan: plan() },
        },
    };
    let mut bytes = Vec::new();
    <WireCodec as Codec>::serialize(&mut bytes, &request).unwrap();
    let decoded: IngestRequest = <WireCodec as Codec>::deserialize(&bytes[..]).unwrap();
    assert_eq!(decoded, request);
    for workflow in [None, Some(Box::new(flow))] {
        let item = crate::Item {
            id: 1,
            workflow,
            ..crate::Item::default()
        };
        let reply = IngestReply::Item(item);
        bytes.clear();
        <WireCodec as Codec>::serialize(&mut bytes, &reply).unwrap();
        let decoded: IngestReply = <WireCodec as Codec>::deserialize(&bytes[..]).unwrap();
        assert_eq!(decoded, reply);
    }
}
