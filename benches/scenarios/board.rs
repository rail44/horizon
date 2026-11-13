//! The board's list and a task's thread over a store handed in whole,
//! with several numbers of tasks and of posts.

use horizon::background::Background;
use horizon::board::Source;
use horizon_board::{BoardEvent, Comment, Item, Store, sample_envelopes};
use horizon_theme_css::{Theme, themed};

use crate::{Frame, Scenario, Stage, character, wheel};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "board/select",
            unit: "tasks",
            sizes: &[10, 100, 1000],
            run: select,
        },
        Scenario {
            name: "board/list-scroll",
            unit: "tasks",
            sizes: &[10, 100, 1000],
            run: list_scroll,
        },
        Scenario {
            name: "board/thread-step",
            unit: "posts",
            sizes: &[10, 100, 1000],
            run: thread_step,
        },
        Scenario {
            name: "board/thread-scroll",
            unit: "posts",
            sizes: &[10, 100, 1000],
            run: thread_scroll,
        },
    ]
}

fn source(items: Vec<Item>) -> Source {
    let events = (items.into_iter()).map(|item| BoardEvent::ItemStored { id: item.id, item });
    Source::Ready(Store::in_memory(sample_envelopes(events)))
}

/// `count` open tasks, every tenth with two children.
fn tasks(count: usize) -> Vec<Item> {
    let mut items = Vec::new();
    for id in 1..=count as u64 {
        items.push(Item {
            id,
            title: format!("{id} 番目のタスク: 一覧の行に収まらない長さの題名を持つもの"),
            rank: format!("{id:08}"),
            status: match id % 3 {
                0 => "doing".to_string(),
                1 => "review".to_string(),
                _ => String::new(),
            },
            ..Item::default()
        });
        if id % 10 == 0 {
            for child in 1..=2 {
                items.push(Item {
                    id: id * 10_000 + child,
                    title: format!("{id} の子タスク {child}"),
                    rank: format!("{child:08}"),
                    parent: Some(id),
                    ..Item::default()
                });
            }
        }
    }
    items
}

fn list(count: usize) -> Stage {
    use horizon::board::list::view;
    let source = source(tasks(count));
    let mut list = None;
    let css = horizon_theme_css::CSS;
    let mut stage = Stage::new(600, 800, css, || {
        let (root, state) = view(source, Background::default(), |_| {});
        list = Some(state);
        themed(Theme::built_in, root.focus())
    });
    let list = list.unwrap();
    // The store is read off the UI thread.
    stage.until("the tasks", || !list.tasks().is_empty());
    stage
}

/// The selection moved down the list a row at a time.
fn select(count: usize) -> Vec<Frame> {
    let mut stage = list(count);
    for _ in 0..100.min(count - 1) {
        stage.frame(|app| character(app, 'j'));
    }
    stage.frames
}

fn list_scroll(count: usize) -> Vec<Frame> {
    let mut stage = list(count);
    for _ in 0..100 {
        stage.frame(|app| wheel(app, (300., 400.), -120.));
    }
    stage.frames
}

/// A task's thread of `posts` posts, laid out.
fn thread(posts: usize) -> Stage {
    use horizon::board::thread::{CSS, view};
    let mut task = Item {
        id: 1,
        title: "投稿の多いタスク".to_string(),
        rank: "a".to_string(),
        status: "doing".to_string(),
        ..Item::default()
    };
    for post in 0..posts {
        task.comments.push(Comment {
            id: format!("c{post}"),
            author: if post % 2 == 0 { "agent" } else { "owner" }.to_string(),
            text: format!(
                "{post} 番目の投稿です。\n\n- 確認した点を箇条書きで書く\n- `cargo nextest run` の結果を書く\n\n続きは次の投稿に書きます。"
            ),
            at: None,
            source: None,
        });
    }
    let source = source(vec![task]);
    let css = [horizon_theme_css::CSS, CSS].concat();
    let mut stage = Stage::new(700, 800, &css, || {
        let (root, _) = view(source, Background::default(), 1, |_| {});
        themed(Theme::built_in, root.focus())
    });
    stage.until_text("the page", "投稿の多いタスク");
    for _ in 0..4 {
        stage.settle();
    }
    stage
}

/// The cursor moved up a thread a post at a time.
fn thread_step(posts: usize) -> Vec<Frame> {
    let mut stage = thread(posts);
    for _ in 0..100.min(posts - 1) {
        stage.frame(|app| character(app, 'k'));
    }
    stage.frames
}

/// The wheel turned through a task's thread of `posts` posts.
fn thread_scroll(posts: usize) -> Vec<Frame> {
    let mut stage = thread(posts);
    for step in 0..100 {
        // Up from the end and back down to it.
        let up = if step < 50 { 120. } else { -120. };
        stage.frame(|app| wheel(app, (350., 400.), up));
    }
    stage.frames
}
