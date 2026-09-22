use std::path::PathBuf;

use super::*;
use crate::event::{BoardEvent, Envelope};
use crate::model::Comment;

fn comment(id: &str, text: &str) -> Comment {
    Comment {
        id: id.to_string(),
        author: "tester".to_string(),
        text: text.to_string(),
        at: Some(7),
        source: None,
    }
}

fn item(id: u64, title: &str, rank: &str, status: &str, is_closed: bool) -> Item {
    Item {
        id,
        title: title.to_string(),
        rank: rank.to_string(),
        status: status.to_string(),
        is_closed,
        ..Item::default()
    }
}

/// A board with two open tasks, a closed one, a conversation, and a read
/// position — enough for every query to have something to say.
fn envelopes() -> Vec<Envelope> {
    sample_envelopes([
        BoardEvent::ItemStored {
            id: 1,
            item: item(1, "first", "n", "doing", false),
        },
        BoardEvent::ItemStored {
            id: 2,
            item: item(2, "second", "p", "", false),
        },
        BoardEvent::ItemStored {
            id: 3,
            item: item(3, "third", "q", "shipped", true),
        },
        BoardEvent::MessageAdded {
            id: 1,
            message: comment("m1", "hello"),
        },
        BoardEvent::MessageAdded {
            id: 1,
            message: comment("m2", "and again"),
        },
        BoardEvent::ReadAdvanced {
            id: 1,
            reader: "viewer".to_string(),
            message_id: "m2".to_string(),
        },
        BoardEvent::CursorAdvanced {
            consumer: "viewer".to_string(),
            position: 4,
        },
    ])
}

/// Writes `envelopes` out as the event log would hold them and opens a
/// file-backed store over the result.
fn file_store(envelopes: &[Envelope]) -> (Store, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "horizon-board-store-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut text = String::new();
    for envelope in envelopes {
        text.push_str(&serde_json::to_string(envelope).unwrap());
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    (Store::at(path.clone()), path)
}

fn assert_lists_match(memory: &ListResult, file: &ListResult) {
    assert_eq!(memory.items, file.items);
    assert_eq!(memory.statuses, file.statuses);
    assert_eq!(memory.skipped, file.skipped);
}

#[test]
fn in_memory_and_file_sources_answer_the_same_queries_alike() {
    let envelopes = envelopes();
    let memory = Store::in_memory(envelopes.clone());
    let (file, path) = file_store(&envelopes);

    for (status, include_closed) in [(None, false), (None, true), (Some("doing"), false)] {
        assert_lists_match(
            &memory.list(status, include_closed).unwrap(),
            &file.list(status, include_closed).unwrap(),
        );
    }
    for id in [1, 2, 3, 99] {
        assert_eq!(memory.show(id).unwrap(), file.show(id).unwrap());
    }
    assert_eq!(
        memory.read_positions("viewer").unwrap(),
        file.read_positions("viewer").unwrap()
    );
    assert_eq!(
        memory.cursor("viewer").unwrap(),
        file.cursor("viewer").unwrap()
    );

    // The queries themselves answered something, so "alike" is not two
    // empty results agreeing.
    let listed = memory.list(None, false).unwrap();
    assert_eq!(
        listed.items.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    // The status vocabulary spans every item, including the closed one the
    // list itself filtered out.
    assert_eq!(
        listed.statuses,
        vec!["doing".to_string(), "shipped".to_string()]
    );
    assert_eq!(memory.show(1).unwrap().unwrap().comments.len(), 2);
    assert_eq!(
        memory.read_positions("viewer").unwrap().get(&1).unwrap(),
        "m2"
    );

    std::fs::remove_file(path).unwrap();
}

#[test]
fn writes_on_an_in_memory_store_are_refused_without_reaching_a_daemon() {
    let store = Store::in_memory(envelopes());
    let before = store.list(None, true).unwrap().items;

    // A runtime with no IO driver: a write that tried to open the logd
    // socket would panic rather than quietly fail.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        for result in [
            store.add("new", "", None, Position::Top).await.map(|_| ()),
            store.comment(1, "tester", "hi").await,
            store.set_status(1, "done").await,
            store.set_closed(1, true, None).await,
            store.move_item(1, Position::Bottom).await.map(|_| ()),
            store.edit(1, Some("renamed".into()), None).await,
            store.bind_session(1, "session").await.map(|_| ()),
            store.mark_read(1, "viewer", "m1").await,
            store.advance_cursor("viewer", 9).await,
        ] {
            assert!(
                matches!(result, Err(StoreError::ReadOnly)),
                "expected a read-only refusal, got {result:?}"
            );
        }
        assert!(matches!(
            store.subscribe(None).await.err(),
            Some(StoreError::ReadOnly)
        ));
    });

    assert_eq!(store.list(None, true).unwrap().items, before);
}

#[test]
fn sample_envelopes_are_stamped_and_sequenced_deterministically() {
    let events = || {
        [
            BoardEvent::ItemStored {
                id: 1,
                item: item(1, "first", "n", "", false),
            },
            BoardEvent::ItemStored {
                id: 4,
                item: item(4, "second", "p", "", false),
            },
        ]
    };
    let envelopes = sample_envelopes(events());
    assert_eq!(envelopes, sample_envelopes(events()));

    let stamps: Vec<u64> = envelopes.iter().map(|e| e.at).collect();
    assert_eq!(stamps, vec![1_767_225_600_000, 1_767_225_660_000]);
    assert!(envelopes
        .iter()
        .all(|e| e.schema == crate::SCHEMA && e.version == crate::VERSION));

    let report = Store::in_memory(envelopes).events().unwrap();
    assert_eq!(report.sequences, vec![1, 2]);
    assert_eq!(report.line_count, 2);
    assert_eq!(report.max_id, Some(4));
    assert_eq!(report.skipped_summary(), None);
}
