use super::*;
use crate::Item;
use std::collections::HashMap;

const COMMIT: &str = "1111111111111111111111111111111111111111";

struct Board(HashMap<u64, Item>);
impl Board {
    fn new() -> Self {
        let mut board = Self(HashMap::from([(
            1,
            Item {
                id: 1,
                title: "Goal".into(),
                rank: "m".into(),
                ..Item::default()
            },
        )]));
        board.change(1, Mutation::Enable);
        board
    }
    fn change(&mut self, id: u64, mutation: Mutation) {
        for item in apply(&self.0, id, mutation, self.0.keys().copied().max().unwrap()).unwrap() {
            self.0.insert(item.id, item);
        }
    }
    fn flow(&self, id: u64) -> &Workflow {
        self.0[&id].workflow.as_ref().unwrap()
    }
    fn start(&mut self, id: u64) {
        let work = eligible_work(&self.0, id).unwrap();
        self.change(
            id,
            Mutation::Start {
                token: format!("attempt-{id}"),
                session: format!("session-{id}"),
                work,
            },
        );
    }
    fn report(&mut self, id: u64, report: Report) {
        self.change(
            id,
            Mutation::Report {
                token: format!("attempt-{id}"),
                session: format!("session-{id}"),
                report,
            },
        );
    }
    fn finish(&mut self, id: u64) {
        self.change(
            id,
            Mutation::Finish {
                token: format!("attempt-{id}"),
            },
        );
    }
    fn plan(&mut self, plan: PlanDraft) {
        self.start(1);
        self.report(1, Report::Plan { plan });
        self.finish(1);
    }
    fn id(&self, key: &str) -> u64 {
        self.0
            .values()
            .find(|i| {
                i.workflow
                    .as_ref()
                    .and_then(|w| w.task.as_ref())
                    .is_some_and(|t| t.key == key)
            })
            .unwrap()
            .id
    }
    fn implemented(&mut self, id: u64) {
        self.start(id);
        self.report(
            id,
            Report::Task {
                summary: "Implemented".into(),
                checks: vec!["test -s result.txt: passed".into()],
                commit: COMMIT.into(),
            },
        );
        self.finish(id);
    }
}

fn task(key: &str, deps: &[&str]) -> PlannedTask {
    PlannedTask {
        key: key.into(),
        title: key.into(),
        instructions: format!("Implement {key}"),
        acceptance: vec![format!("{key} works")],
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        scope: ChangeScope {
            paths: vec![format!("src/{key}.rs")],
            functions: vec![key.into()],
        },
        ..PlannedTask::default()
    }
}
fn decision() -> PlannedDecision {
    PlannedDecision {
        key: "scope".into(),
        question: "Include the extension?".into(),
        context: "Scope is unresolved".into(),
        recommendation: "Include it".into(),
        consequence: "Enables the extension task".into(),
        affected_tasks: vec!["c".into()],
    }
}
fn draft() -> PlanDraft {
    PlanDraft {
        summary: "Deliver the goal".into(),
        reason: "Initial decomposition".into(),
        acceptance: vec!["The complete feature works".into()],
        tasks: vec![task("c", &["a"]), task("b", &[]), task("a", &[])],
        decisions: vec![decision()],
        ..PlanDraft::default()
    }
}

#[test]
fn plans_create_real_board_items_and_block_only_affected_work() {
    let mut board = Board::new();
    board.plan(draft());
    let (a, b, c) = (board.id("a"), board.id("b"), board.id("c"));
    assert_eq!(board.0[&c].parent, Some(1));
    assert_eq!(board.0[&c].depends_on, vec![a]);
    assert_eq!(board.flow(1).plan.as_ref().unwrap().tasks, vec![c, b, a]);
    assert_eq!(board.flow(1).unanswered()[0].affected_tasks, vec![c]);
    assert_eq!(eligible_work(&board.0, c), None);
    board.start(a);
    board.start(b);
    assert!(board.flow(a).active.is_some() && board.flow(b).active.is_some());
    assert_eq!(board.flow(1).label(), "decisions pending");
    assert_ne!(board.0[&1].status, "blocked");
}

#[test]
fn task_scope_reservations_serialize_only_known_conflicts() {
    let mut plan = draft();
    plan.tasks[1].scope.paths = vec!["src/a.rs".into()];
    let mut board = Board::new();
    board.plan(plan);
    let (a, b) = (board.id("a"), board.id("b"));
    board.start(a);
    assert_eq!(eligible_work(&board.0, b), None);
    assert!(scopes_conflict(
        &ChangeScope {
            paths: vec!["src".into()],
            functions: vec![]
        },
        &ChangeScope {
            paths: vec!["src/a.rs".into()],
            functions: vec![]
        }
    ));
    assert!(!scopes_conflict(
        &ChangeScope {
            paths: vec!["src/a".into()],
            functions: vec![]
        },
        &ChangeScope {
            paths: vec!["src/ab".into()],
            functions: vec![]
        }
    ));
    assert!(scopes_conflict(
        &ChangeScope {
            paths: vec!["left".into()],
            functions: vec!["API".into()]
        },
        &ChangeScope {
            paths: vec!["right".into()],
            functions: vec!["API".into()]
        }
    ));
}

#[test]
fn a_reopened_decision_blocks_successors_of_integrated_work() {
    let mut plan = draft();
    plan.decisions[0].affected_tasks = vec!["a".into()];
    let mut board = Board::new();
    board.plan(plan);
    let (a, c) = (board.id("a"), board.id("c"));
    board
        .0
        .get_mut(&a)
        .unwrap()
        .workflow
        .as_mut()
        .unwrap()
        .integrated = Some(COMMIT.into());
    assert!(task_waits_for_decision(&board.0, c));
    assert!(eligible_work(&board.0, c).is_none());
}

#[test]
fn discussion_is_not_consent_and_clear_decisions_release_only_affected_work() {
    let mut plan = draft();
    plan.tasks[0].depends_on.clear();
    let mut board = Board::new();
    board.plan(plan);
    let c = board.id("c");
    board.change(
        1,
        Mutation::Answer {
            key: "scope".into(),
            text: "Would that be expensive?".into(),
        },
    );
    assert_eq!(board.flow(1).unanswered().len(), 1);
    board.start(1);
    board.report(
        1,
        Report::Discussion {
            reply: "It requires one small task".into(),
            resolution: None,
            acceptance: None,
        },
    );
    board.finish(1);
    assert!(eligible_work(&board.0, c).is_none());
    board.change(
        1,
        Mutation::Answer {
            key: "scope".into(),
            text: "Include it".into(),
        },
    );
    board.start(1);
    board.report(
        1,
        Report::Discussion {
            reply: "The extension is included".into(),
            resolution: Some("Include the extension".into()),
            acceptance: None,
        },
    );
    board.finish(1);
    assert!(board.flow(1).unanswered().is_empty());
    assert!(eligible_work(&board.0, c).is_some());
    assert_eq!(
        board.flow(1).plan.as_ref().unwrap().decisions[0]
            .messages
            .len(),
        4
    );
}

#[test]
fn a_new_owner_message_invalidates_a_provisional_resolution() {
    let mut board = Board::new();
    board.plan(draft());
    board.change(
        1,
        Mutation::Answer {
            key: "scope".into(),
            text: "Include it".into(),
        },
    );
    board.start(1);
    board.report(
        1,
        Report::Discussion {
            reply: "Included".into(),
            resolution: Some("Included".into()),
            acceptance: None,
        },
    );
    board.change(
        1,
        Mutation::Answer {
            key: "scope".into(),
            text: "Wait, explain the impact first".into(),
        },
    );
    board.finish(1);
    assert_eq!(board.flow(1).unanswered().len(), 1);
    assert!(matches!(
        eligible_work(&board.0, 1),
        Some(Work::Discuss { turn: 2, .. })
    ));
}

#[test]
fn restart_preserves_a_reply_arriving_during_a_readonly_plan() {
    let mut board = Board::new();
    board.plan(draft());
    board.change(1, Mutation::Replan);
    board.start(1);
    board.report(1, Report::Plan { plan: draft() });
    board.change(
        1,
        Mutation::Answer {
            key: "scope".into(),
            text: "Include it".into(),
        },
    );
    board.change(
        1,
        Mutation::Restart {
            token: "attempt-1".into(),
        },
    );
    assert!(board.flow(1).problem.is_none());
    assert!(matches!(
        eligible_work(&board.0, 1),
        Some(Work::Discuss { .. })
    ));
    let a = board.id("a");
    board.start(a);
    board.report(
        a,
        Report::Task {
            summary: "Provisional work".into(),
            checks: vec!["check".into()],
            commit: COMMIT.into(),
        },
    );
    board.change(
        a,
        Mutation::Restart {
            token: format!("attempt-{a}"),
        },
    );
    assert!(board.flow(a).result.is_none());
    assert!(board.flow(a).problem.is_some());
}

#[test]
fn existing_board_tasks_are_adopted_and_replanning_keeps_identity() {
    let mut board = Board::new();
    board.0.insert(
        40,
        Item {
            id: 40,
            title: "Existing task".into(),
            comments: vec![crate::Comment {
                author: "owner".into(),
                text: "Keep this context".into(),
                at: 1,
            }],
            ..Item::default()
        },
    );
    let mut plan = draft();
    plan.tasks[2].item_id = Some(40);
    board.plan(plan.clone());
    assert_eq!(board.id("a"), 40);
    assert_eq!(board.0[&40].comments.len(), 1);
    let count = board.0.len();
    board.change(1, Mutation::Replan);
    board.plan(plan);
    assert_eq!(board.0.len(), count);
    assert_eq!(board.id("a"), 40);
}

#[test]
fn unrelated_legacy_dependency_errors_do_not_block_a_new_plan() {
    let mut board = Board::new();
    board.0.insert(
        90,
        Item {
            id: 90,
            depends_on: vec![90],
            ..Item::default()
        },
    );
    board.plan(draft());
    assert_eq!(board.flow(1).plan.as_ref().unwrap().tasks.len(), 3);
}

#[test]
fn task_results_replan_without_losing_results_arriving_during_planning() {
    let mut board = Board::new();
    board.plan(draft());
    let a = board.id("a");
    board.start(a);
    board.change(1, Mutation::Replan);
    board.start(1);
    board.report(1, Report::Plan { plan: draft() });
    board.report(
        a,
        Report::Task {
            summary: "Done".into(),
            checks: vec!["test passed".into()],
            commit: COMMIT.into(),
        },
    );
    board.finish(a);
    board.finish(1);
    assert!(board.flow(1).plan_requested);
    assert_eq!(board.flow(a).result.as_ref().unwrap().commit, COMMIT);
    assert!(!board.flow(1).achieved);
}

#[test]
fn invalid_plans_cannot_rewrite_live_work_or_silently_change_acceptance() {
    let mut board = Board::new();
    board.plan(draft());
    board.start(board.id("a"));
    board.change(1, Mutation::Replan);
    board.start(1);
    let before = board.0.clone();
    for plan in {
        let mut live = draft();
        live.tasks[2].instructions = "Different work".into();
        let mut criteria = draft();
        criteria.acceptance = vec!["Weaker criterion".into()];
        let mut cycle = draft();
        cycle.tasks[2].depends_on = vec!["c".into()];
        vec![live, criteria, cycle]
    } {
        assert!(apply(
            &board.0,
            1,
            Mutation::Report {
                token: "attempt-1".into(),
                session: "session-1".into(),
                report: Report::Plan { plan }
            },
            100
        )
        .is_err());
        assert_eq!(board.0, before);
    }
}

#[test]
fn verified_task_integration_does_not_achieve_the_whole_milestone() {
    let mut board = Board::new();
    board.plan(draft());
    let a = board.id("a");
    board.implemented(a);
    board.start(a);
    board.change(
        a,
        Mutation::PrepareVerification {
            base: COMMIT.into(),
            head: COMMIT.into(),
        },
    );
    let verification = Verification {
        summary: "Verified".into(),
        commit: COMMIT.into(),
        checks: vec!["test -s result.txt".into()],
        evidence: vec![Evidence {
            criterion: "a works".into(),
            detail: "Observed expected output".into(),
            satisfied: true,
            decision: None,
            check: Some("test -s result.txt".into()),
        }],
        decisions: vec![],
    };
    board.report(a, Report::Verification { verification });
    board.finish(a);
    assert!(board.flow(a).integrated.is_none());
    for status in ["archived", "done"] {
        board.0.get_mut(&1).unwrap().status = status.into();
        assert!(eligible_work(&board.0, board.id("b")).is_none());
        assert!(apply(&board.0, a, Mutation::BeginIntegration, 100).is_err());
    }
    board.0.get_mut(&1).unwrap().status = "in-progress".into();
    board
        .0
        .get_mut(&1)
        .unwrap()
        .workflow
        .as_mut()
        .unwrap()
        .goal_revision += 1;
    assert!(apply(&board.0, a, Mutation::BeginIntegration, 100).is_err());
    board
        .0
        .get_mut(&1)
        .unwrap()
        .workflow
        .as_mut()
        .unwrap()
        .goal_revision -= 1;
    board.change(a, Mutation::BeginIntegration);
    // A pause cannot erase an integration that already started and completed.
    board.change(a, Mutation::Pause);
    board.change(
        a,
        Mutation::Integrated {
            commit: COMMIT.into(),
        },
    );
    assert_eq!(board.0[&a].status, "done");
    assert!(!board.flow(1).achieved);
    assert!(board.flow(1).plan_requested);
}

#[test]
fn verification_requires_complete_evidence_and_settled_human_evaluation() {
    let mut board = Board::new();
    board.plan(draft());
    let a = board.id("a");
    board.implemented(a);
    board.start(a);
    board.change(
        a,
        Mutation::PrepareVerification {
            base: COMMIT.into(),
            head: COMMIT.into(),
        },
    );
    for evidence in [
        vec![],
        vec![Evidence {
            criterion: "a works".into(),
            detail: "Owner liked it".into(),
            satisfied: true,
            decision: Some("scope".into()),
            check: None,
        }],
    ] {
        assert!(apply(
            &board.0,
            a,
            Mutation::Report {
                token: format!("attempt-{a}"),
                session: format!("session-{a}"),
                report: Report::Verification {
                    verification: Verification {
                        summary: "Verified".into(),
                        commit: COMMIT.into(),
                        checks: vec!["true".into()],
                        evidence,
                        decisions: vec![]
                    }
                }
            },
            100
        )
        .is_err());
    }
}

#[test]
fn foreign_reports_pause_and_late_finishes_cannot_advance_work() {
    let mut board = Board::new();
    board.start(1);
    assert!(apply(
        &board.0,
        1,
        Mutation::Report {
            token: "attempt-1".into(),
            session: "foreign".into(),
            report: Report::Plan { plan: draft() }
        },
        1
    )
    .is_err());
    board.change(1, Mutation::Pause);
    assert!(apply(
        &board.0,
        1,
        Mutation::Report {
            token: "attempt-1".into(),
            session: "session-1".into(),
            report: Report::Plan { plan: draft() }
        },
        1
    )
    .is_err());
    board.change(
        1,
        Mutation::Interrupt {
            token: "attempt-1".into(),
            reason: "Paused".into(),
        },
    );
    assert!(apply(
        &board.0,
        1,
        Mutation::Finish {
            token: "attempt-1".into()
        },
        1
    )
    .is_err());
}

#[test]
fn owner_order_survives_ai_priority_updates() {
    let mut board = Board::new();
    board.plan(draft());
    let (a, b) = (board.id("a"), board.id("b"));
    board
        .0
        .get_mut(&a)
        .unwrap()
        .workflow
        .as_mut()
        .unwrap()
        .before
        .push(b);
    let mut plan = draft();
    plan.priorities = vec![b, a];
    board.change(1, Mutation::Replan);
    board.plan(plan);
    let ids: Vec<_> = ordered_items(&board.0).iter().map(|i| i.id).collect();
    assert!(ids.iter().position(|id| *id == a) < ids.iter().position(|id| *id == b));
    assert!(board.0[&a].rank < board.0[&b].rank);
}

#[test]
fn workflow_and_ordinary_items_roundtrip_the_actual_binary_codec() {
    use crate::wire::{IngestReply, IngestRequest};
    use horizon_wire::WireCodec;
    use remoc::codec::Codec;
    let mut board = Board::new();
    board.plan(draft());
    let request = IngestRequest::Workflow {
        id: 1,
        expected_revision: 1,
        mutation: Mutation::Report {
            token: "token".into(),
            session: "session".into(),
            report: Report::Plan { plan: draft() },
        },
    };
    let mut bytes = Vec::new();
    <WireCodec as Codec>::serialize(&mut bytes, &request).unwrap();
    let decoded: IngestRequest = <WireCodec as Codec>::deserialize(&bytes[..]).unwrap();
    assert_eq!(decoded, request);
    for item in board.0.values().cloned().chain([Item::default()]) {
        let reply = IngestReply::Item(item);
        bytes.clear();
        <WireCodec as Codec>::serialize(&mut bytes, &reply).unwrap();
        let decoded: IngestReply = <WireCodec as Codec>::deserialize(&bytes[..]).unwrap();
        assert_eq!(decoded, reply);
    }
}
