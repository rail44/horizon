//! Ordinary board writes against isolated temporary logs (no daemon sockets).
use horizon_board::{
    wire::{IngestReply, IngestRequest as Request, LogError},
    Comment, Position, Store,
};
use horizon_logd::writer::perform;
use std::path::PathBuf;

struct Board(PathBuf);
impl Board {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!(
                    "horizon-board-tasks-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))
                .join("events.jsonl"),
        )
    }
    fn write(&self, request: Request) -> Result<(IngestReply, Vec<u64>), LogError> {
        perform(&self.0, request)
    }
    fn add(&self, parent: Option<u64>) -> u64 {
        match self
            .write(Request::Add {
                title: "task".into(),
                body: "body".into(),
                parent,
                position: Position::Bottom,
            })
            .unwrap()
            .0
        {
            IngestReply::Item(item) => item.id,
            _ => unreachable!(),
        }
    }
    fn item(&self, id: u64) -> horizon_board::Item {
        Store::at(self.0.clone()).show(id).unwrap().unwrap()
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[test]
fn rank_changes_only_sibling_order_and_preserves_dependencies() {
    let b = Board::new();
    let parent = b.add(None);
    let other = b.add(None);
    let first = b.add(Some(parent));
    let second = b.add(Some(parent));
    b.write(Request::SetDependencies {
        id: first,
        depends_on: vec![other],
    })
    .unwrap();
    let untouched = b.item(other);
    b.write(Request::MoveItem {
        id: second,
        position: Position::Before(first),
    })
    .unwrap();
    assert!(b.item(second).rank < b.item(first).rank);
    assert_eq!(b.item(first).depends_on, vec![other]);
    assert_eq!(b.item(other), untouched);
    assert!(b
        .write(Request::MoveItem {
            id: first,
            position: Position::After(other)
        })
        .is_err());
    assert!(b
        .write(Request::MoveItem {
            id: first,
            position: Position::After(first)
        })
        .is_err());
}
#[test]
fn relationships_reject_missing_references_and_cycles() {
    let b = Board::new();
    let a = b.add(None);
    let c = b.add(Some(a));
    assert!(b
        .write(Request::SetParent {
            id: a,
            parent: Some(c),
            position: Position::Bottom
        })
        .is_err());
    b.write(Request::SetDependencies {
        id: a,
        depends_on: vec![c],
    })
    .unwrap();
    assert!(b
        .write(Request::SetDependencies {
            id: c,
            depends_on: vec![a]
        })
        .is_err());
    assert!(b
        .write(Request::SetDependencies {
            id: c,
            depends_on: vec![99]
        })
        .is_err());
    assert!(b
        .write(Request::SetDependencies {
            id: c,
            depends_on: vec![a, a]
        })
        .is_err());
}
#[test]
fn completion_is_independent_of_project_status_and_session_binding_is_atomic() {
    let b = Board::new();
    let id = b.add(None);
    b.write(Request::SetStatus {
        id,
        status: "waiting for artifact".into(),
    })
    .unwrap();
    b.write(Request::SetCompleted {
        id,
        completed: true,
    })
    .unwrap();
    b.write(Request::BindSession {
        id,
        session_id: "first".into(),
        review: false,
    })
    .unwrap();
    let reply = b
        .write(Request::BindSession {
            id,
            session_id: "second".into(),
            review: false,
        })
        .unwrap();
    assert!(reply.1.is_empty());
    assert_eq!(b.item(id).session_id.as_deref(), Some("first"));
    assert!(Store::at(b.0.clone())
        .list(None, false)
        .unwrap()
        .items
        .is_empty());
    assert_eq!(b.item(id).status, "waiting for artifact");
}
#[test]
fn a_new_review_replaces_only_the_review_session_binding() {
    let b = Board::new();
    let id = b.add(None);
    for (session_id, review) in [("task", false), ("review-one", true), ("review-two", true)] {
        b.write(Request::BindSession {
            id,
            session_id: session_id.into(),
            review,
        })
        .unwrap();
    }
    let item = b.item(id);
    assert_eq!(item.session_id.as_deref(), Some("task"));
    assert_eq!(item.review_session_id.as_deref(), Some("review-two"));
    let duplicate = b
        .write(Request::BindSession {
            id,
            session_id: "review-two".into(),
            review: true,
        })
        .unwrap();
    assert!(duplicate.1.is_empty());
    b.write(Request::BindSession {
        id,
        session_id: "replacement-task".into(),
        review: false,
    })
    .unwrap();
    assert_eq!(b.item(id).session_id.as_deref(), Some("task"));
}

#[test]
fn stable_message_delivery_preserves_duplicates_and_sparse_read_state() {
    let b = Board::new();
    let id = b.add(None);
    let message = Comment {
        id: "result:1".into(),
        source: Some("session:round:1".into()),
        author: "agent".into(),
        text: "same".into(),
        at: None,
    };
    b.write(Request::PostMessage {
        id,
        message: message.clone(),
    })
    .unwrap();
    assert!(b
        .write(Request::PostMessage {
            id,
            message: message.clone()
        })
        .unwrap()
        .1
        .is_empty());
    let mut conflicting = message;
    conflicting.text = "changed".into();
    assert!(b
        .write(Request::PostMessage {
            id,
            message: conflicting
        })
        .is_err());
    b.write(Request::Comment {
        id,
        author: "owner".into(),
        text: "same".into(),
    })
    .unwrap();
    b.write(Request::Comment {
        id,
        author: "owner".into(),
        text: "same".into(),
    })
    .unwrap();
    let before = b.item(id);
    assert_eq!(before.comments.len(), 3);
    let last = before.comments[2].id.clone();
    b.write(Request::MarkRead {
        id,
        reader: "owner".into(),
        message_id: last.clone(),
    })
    .unwrap();
    assert_eq!(
        Store::at(b.0.clone()).read_messages("owner").unwrap()[&id],
        std::collections::HashSet::from([last.clone()])
    );
    assert!(Store::at(b.0.clone())
        .read_messages("another reader")
        .unwrap()
        .is_empty());
    assert!(b
        .write(Request::MarkRead {
            id,
            reader: "owner".into(),
            message_id: last.clone()
        })
        .unwrap()
        .1
        .is_empty());
    b.write(Request::MarkRead {
        id,
        reader: "owner".into(),
        message_id: before.comments[0].id.clone(),
    })
    .unwrap();
    assert_eq!(b.item(id), before);
    assert_eq!(
        Store::at(b.0.clone()).read_messages("owner").unwrap()[&id],
        std::collections::HashSet::from([last.clone(), before.comments[0].id.clone()])
    );
}
#[test]
fn import_high_water_survives_without_creating_runnable_events() {
    let b = Board::new();
    std::fs::create_dir_all(b.0.parent().unwrap()).unwrap();
    let event = horizon_board::Envelope {
        schema: horizon_board::SCHEMA.into(),
        version: horizon_board::VERSION,
        at: 0,
        event: horizon_board::BoardEvent::ImportHighWater { id: 999 },
    };
    std::fs::write(
        &b.0,
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();
    assert_eq!(b.add(None), 1000);
    let report = Store::at(b.0.clone()).events().unwrap();
    assert_eq!(report.sequences, vec![1, 2]);
    b.write(Request::AdvanceCursor {
        consumer: "organizer".into(),
        position: 2,
    })
    .unwrap();
    b.write(Request::AdvanceCursor {
        consumer: "organizer".into(),
        position: 1,
    })
    .unwrap();
    assert_eq!(Store::at(b.0.clone()).cursor("organizer").unwrap(), 2);
}
