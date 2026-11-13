//! The named previews: each one of Horizon's views built over sample
//! data, for a preview pane of a running shell to show.
#![cfg(target_family = "wasm")]

use horizon::background::Background;
use horizon::board::{Source, list};
use horizon_board::{BoardEvent, Comment, Item, Store, sample_envelopes};
use horizon_ui::El;

fn item(id: u64, title: &str, rank: &str, parent: Option<u64>, status: &str) -> Item {
    Item {
        id,
        title: title.to_string(),
        rank: rank.to_string(),
        parent,
        status: status.to_string(),
        ..Item::default()
    }
}

/// Two open tasks, the first with two children and a post nobody has
/// read, and one finished task.
fn tasks() -> Vec<Item> {
    let mut first = item(1, "first", "a", None, "doing");
    first.comments.push(Comment {
        id: "c1".to_string(),
        author: "agent".to_string(),
        text: "a post".to_string(),
        at: None,
        source: None,
    });
    vec![
        first,
        item(2, "child one", "a", Some(1), ""),
        item(3, "child two", "b", Some(1), "review"),
        item(4, "second", "b", None, ""),
        item(5, "finished", "c", None, "done"),
    ]
}

/// The board's task list over a store that holds `items` and refuses
/// writes.
fn board_list(items: Vec<Item>) -> El {
    let events = (items.into_iter()).map(|item| BoardEvent::ItemStored { id: item.id, item });
    let source = Source::Ready(Store::in_memory(sample_envelopes(events)));
    list::view(source, Background::default(), |_| {}).0
}

fn view(name: &str) -> Option<El> {
    match name {
        "parts" => Some(horizon::parts::gallery()),
        "board-list" => Some(board_list(tasks())),
        "board-list-empty" => Some(board_list(Vec::new())),
        // Never comes back: what the shell's limit on a call is for
        // (`tests/preview.rs`). Not among the names.
        "stuck" => {
            let mut turns = 0u64;
            loop {
                turns = std::hint::black_box(turns.wrapping_add(1));
            }
        }
        _ => None,
    }
}

horizon_ui::plugin!(
    names: vec![
        "board-list".to_string(),
        "board-list-empty".to_string(),
        "parts".to_string(),
    ],
    // (Each view brings its own style sheet.)
    css: String::new(),
    view: view,
);
