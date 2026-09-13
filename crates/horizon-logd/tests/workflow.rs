//! The real writer and file projection, without a socket or provider. These
//! tests exercise durable reservations and compare-and-swap across callers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use horizon_board::wire::{IngestReply, IngestRequest, LogError};
use horizon_board::workflow::{ChangeScope, Mutation, PlanDraft, PlannedTask, Report, Work};
use horizon_board::{Item, Position, Store};
use horizon_logd::writer::perform;

struct Board(PathBuf);

impl Board {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "horizon-workflow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path.join("events.jsonl"))
    }

    fn item(&self) -> Item {
        Store::at(self.0.clone()).show(1).unwrap().unwrap()
    }

    fn mutate(&self, mutation: Mutation) -> Item {
        self.mutate_id(1, mutation)
    }

    fn mutate_id(&self, id: u64, mutation: Mutation) -> Item {
        let loaded = Store::at(self.0.clone()).show(id).unwrap().unwrap();
        let revision = loaded.workflow.as_ref().map_or(0, |f| f.revision);
        let (IngestReply::Item(item), _) = perform(
            &self.0,
            IngestRequest::Workflow {
                id,
                expected_revision: revision,
                mutation,
            },
        )
        .unwrap() else {
            panic!("unexpected reply")
        };
        assert_eq!(
            item,
            Store::at(self.0.clone()).show(id).unwrap().unwrap(),
            "writer reply must match the durable projection"
        );
        item
    }

    fn add(&self) {
        perform(
            &self.0,
            IngestRequest::Add {
                title: "Goal".into(),
                body: "Deliver behavior".into(),
                parent: None,
                position: Position::Bottom,
            },
        )
        .unwrap();
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[test]
fn concurrent_coordinators_can_reserve_an_attempt_only_once() {
    let board = Board::new();
    board.add();
    let enabled = board.mutate(Mutation::Enable);
    let revision = enabled.workflow.unwrap().revision;
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|number| {
                let path = &board.0;
                scope.spawn(move || {
                    perform(
                        path,
                        IngestRequest::Workflow {
                            id: 1,
                            expected_revision: revision,
                            mutation: Mutation::Start {
                                token: format!("attempt-{number}"),
                                session: format!("session-{number}"),
                                work: Work::Plan,
                            },
                        },
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|e| matches!(e, LogError::InvalidWorkflow(_))));
    assert!(board.item().workflow.unwrap().active.is_some());
}

#[test]
fn saved_plan_and_task_result_survive_reload_and_do_not_close_the_milestone() {
    let board = Board::new();
    board.add();
    board.mutate(Mutation::Enable);
    board.mutate(Mutation::Start {
        token: "plan".into(),
        session: "planner".into(),
        work: Work::Plan,
    });
    let plan = PlanDraft {
        reason: "Initial decomposition".into(),
        summary: "Implement goal".into(),
        acceptance: vec!["Observable behavior".into()],
        tasks: vec![PlannedTask {
            key: "implement".into(),
            title: "Implement".into(),
            instructions: "Implement and test".into(),
            acceptance: vec!["Tests pass".into()],
            depends_on: vec![],
            scope: ChangeScope {
                paths: vec!["result.txt".into()],
                functions: vec!["fixture".into()],
            },
            ..PlannedTask::default()
        }],
        decisions: vec![],
        ..PlanDraft::default()
    };
    board.mutate(Mutation::Report {
        token: "plan".into(),
        session: "planner".into(),
        report: Report::Plan { plan },
    });
    assert_eq!(
        Store::at(board.0.clone())
            .list(None, true)
            .unwrap()
            .items
            .len(),
        1
    );
    board.mutate(Mutation::Finish {
        token: "plan".into(),
    });
    let task = board.item().workflow.unwrap().plan.unwrap().tasks[0];
    let events_before = horizon_board::read_events(&board.0).unwrap().line_count;
    assert_eq!(
        Store::at(board.0.clone())
            .show(task)
            .unwrap()
            .unwrap()
            .parent,
        Some(1)
    );
    board.mutate_id(
        task,
        Mutation::Start {
            token: "task".into(),
            session: "worker".into(),
            work: Work::Task {
                key: "implement".into(),
            },
        },
    );
    board.mutate_id(
        task,
        Mutation::Report {
            token: "task".into(),
            session: "worker".into(),
            report: Report::Task {
                summary: "Implemented".into(),
                checks: vec!["cargo test: 4 passed".into()],
                commit: "1111111111111111111111111111111111111111".into(),
            },
        },
    );
    board.mutate_id(
        task,
        Mutation::Finish {
            token: "task".into(),
        },
    );
    let task = Store::at(board.0.clone()).show(task).unwrap().unwrap();
    assert!(task.workflow.unwrap().result.is_some());
    assert!(board.item().workflow.unwrap().plan_requested);
    assert_ne!(board.item().status, "done");
    assert!(board.item().comments.is_empty());
    assert_eq!(
        horizon_board::read_events(&board.0).unwrap().line_count,
        events_before + 3
    );
}

#[test]
fn old_items_remain_readable_and_claim_does_not_compete_with_the_coordinator() {
    let board = Board::new();
    board.add();
    assert!(board.item().workflow.is_none());
    board.mutate(Mutation::Enable);
    perform(
        &board.0,
        IngestRequest::SetStatus {
            id: 1,
            status: "ready".into(),
        },
    )
    .unwrap();
    let (reply, _) = perform(
        &board.0,
        IngestRequest::Claim {
            who: "legacy worker".into(),
        },
    )
    .unwrap();
    assert_eq!(reply, IngestReply::MaybeItem(None));
}

#[test]
fn a_changed_goal_invalidates_old_reservations_and_cannot_change_mid_attempt() {
    let board = Board::new();
    board.add();
    let before = board.mutate(Mutation::Enable).workflow.unwrap().revision;
    perform(
        &board.0,
        IngestRequest::Edit {
            id: 1,
            title: None,
            body: Some("Changed goal".into()),
        },
    )
    .unwrap();
    let current = board.item();
    assert_eq!(current.body, "Changed goal");
    assert!(current.workflow.unwrap().revision > before);
    assert!(perform(
        &board.0,
        IngestRequest::Workflow {
            id: 1,
            expected_revision: before,
            mutation: Mutation::Start {
                token: "stale".into(),
                session: "planner".into(),
                work: Work::Plan
            },
        }
    )
    .is_err());
    board.mutate(Mutation::Start {
        token: "current".into(),
        session: "planner".into(),
        work: Work::Plan,
    });
    assert!(perform(
        &board.0,
        IngestRequest::Edit {
            id: 1,
            title: None,
            body: Some("Another goal".into())
        }
    )
    .is_err());
    assert_eq!(board.item().body, "Changed goal");
}
