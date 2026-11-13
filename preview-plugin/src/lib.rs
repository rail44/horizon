//! The named previews: each one of Horizon's views built over sample
//! data, for a preview pane of a running shell to show.
#![cfg(target_family = "wasm")]

use horizon::background::Background;
use horizon::board::{Source, list};
use horizon::parts::{Tone, chip};
use horizon_board::{BoardEvent, Comment, Item, Store, sample_envelopes};
use horizon_ui::{El, el};

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

mod gallery {
    horizon_ui::css_module! {"
        .gallery { display: flex; flex-direction: column; gap: var(--u24); height: 100%;
                   box-sizing: border-box; padding: var(--u24); overflow-y: scroll;
                   background: var(--background); color: var(--text);
                   font-family: var(--font-family); font-size: var(--body-size);
                   line-height: var(--body-line); }
        .part { display: flex; flex-direction: column; gap: var(--u8); }
        .name { font-weight: 600; }
        .line { display: flex; align-items: center; gap: var(--u8); }
        .said { font-size: var(--meta-size); line-height: var(--meta-line);
                color: var(--text-muted); }
    "}
}

/// One part of the gallery: its name, and lines of it as it is placed,
/// each with what the line is.
fn part(name: &str, lines: Vec<(&str, Vec<El>)>) -> El {
    let part = el("div")
        .class(gallery::PART)
        .child(el("div").class(gallery::NAME).text(name));
    for (said, shown) in lines {
        let line = el("div").class(gallery::LINE);
        for one in shown {
            line.child(one);
        }
        part.child(line.child(el("span").class(gallery::SAID).text(said)));
    }
    part
}

/// The parts views are made of (`horizon::parts`), each in every kind it
/// has.
fn parts() -> El {
    let tones = [
        (Tone::Muted, "muted"),
        (Tone::Accent, "accent"),
        (Tone::Success, "success"),
        (Tone::Info, "info"),
        (Tone::Warning, "warning"),
        (Tone::Danger, "danger"),
    ];
    el("div").class(gallery::GALLERY).child(part(
        "chip",
        vec![
            (
                "every tone",
                tones.iter().map(|(tone, name)| chip(*tone, name)).collect(),
            ),
            (
                "as views say things",
                vec![
                    chip(Tone::Accent, "doing"),
                    chip(Tone::Info, "review"),
                    chip(Tone::Accent, "3 unread"),
                    chip(Tone::Danger, "Failed"),
                    chip(Tone::Muted, "Stopped"),
                    chip(Tone::Muted, "Enter"),
                ],
            ),
        ],
    ))
}

fn view(name: &str) -> Option<El> {
    match name {
        "parts" => Some(parts()),
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
