//! The board's task list over a store handed in whole (which refuses
//! writes), driven the way an owner drives the GPUI shell's
//! (`src/board/list.rs`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use horizon_blitz::background::Background;
use horizon_blitz::board::list::{Asked, CSS, view};
use horizon_blitz::board::{BoardList, Source};
use horizon_board::{BoardEvent, Comment, Item, Store, sample_envelopes};
use horizon_theme_css::{Theme, themed};
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::GetUntracked;
use horizon_ui::testing::{Pointer, click_at, pointer};
use keyboard_types::{Key, Modifiers, NamedKey};

struct Opened {
    app: App,
    list: BoardList,
    asked: Rc<RefCell<Vec<Asked>>>,
}

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
fn sample() -> Vec<Item> {
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

fn open(items: Vec<Item>) -> Opened {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = asked.clone();
    let events = items
        .into_iter()
        .map(|item| BoardEvent::ItemStored { id: item.id, item });
    let source = Source::Ready(Store::in_memory(sample_envelopes(events)));
    let mut list = None;
    let css = [horizon_theme_css::CSS, CSS].concat();
    let app = App::new(600, 500, &css, || {
        let (root, state) = view(source, Background::default(), move |asked| {
            log.borrow_mut().push(asked)
        });
        list = Some(state);
        themed(Theme::built_in, root.focus())
    });
    let opened = Opened {
        app,
        list: list.unwrap(),
        asked,
    };
    // The store is read off the UI thread.
    opened.until("the tasks", || !opened.list.tasks().is_empty());
    opened
}

fn collect(document: &BaseDocument, id: NodeId, class: &str, found: &mut Vec<NodeId>) {
    let node = document.get_node(id).expect("a node");
    let named = (node.attr(local_name!("class")))
        .is_some_and(|classes| classes.split(' ').any(|name| name == class));
    if named {
        found.push(id);
    }
    for child in &node.children {
        collect(document, *child, class, found);
    }
}

impl Opened {
    /// Lays out (which takes in what the store answered) until `met`.
    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        loop {
            self.app.resolve();
            if met() {
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "no {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn all(&self, class: &str) -> Vec<NodeId> {
        let document = self.app.document();
        let document = document.borrow();
        let mut found = Vec::new();
        collect(&document, document.root_element().id, class, &mut found);
        found
    }

    fn text_of(&self, node: NodeId) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.get_node(node).unwrap().text_content()
    }

    /// The titles of the rows on the page, in order.
    fn titles(&self) -> Vec<String> {
        (self.all("title").into_iter())
            .map(|title| self.text_of(title))
            .collect()
    }

    fn middle(&self, node: NodeId) -> (f32, f32) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (
            (bounds.x + bounds.width / 2.) as f32,
            (bounds.y + bounds.height / 2.) as f32,
        )
    }

    fn key(&self, key: Key) {
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers: Modifiers::default(),
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }));
        self.app.resolve();
    }

    fn character(&self, character: &str) {
        self.key(Key::Character(character.into()));
    }

    fn selected(&self) -> Option<u64> {
        self.list.selected.get_untracked()
    }
}

#[test]
fn the_tasks_are_the_tree_in_rank_order_with_the_finished_behind_one_row() {
    let opened = open(sample());
    assert_eq!(
        opened.titles(),
        ["first", "child one", "child two", "second"]
    );
    assert_eq!(opened.list.visible(), [1, 2, 3, 4]);
    // The first row is the selected one.
    assert_eq!(opened.selected(), Some(1));
    assert_eq!(opened.all("selected"), vec![opened.all("row")[0]]);
    // The post nobody has read marks its row.
    assert_eq!(opened.all("unread"), vec![opened.all("row")[0]]);
    assert_eq!(opened.all("unread-dot").len(), 1);
    // A status is a chip, and a task without one a dot.
    let chips: Vec<String> = (opened.all("chip").into_iter())
        .map(|chip| opened.text_of(chip))
        .collect();
    assert_eq!(chips, ["doing", "review"]);

    // The finished task is behind the band, which `o` opens and shuts.
    let band = opened.all("band");
    assert_eq!(band.len(), 1);
    assert!(
        opened.text_of(band[0]).contains('1'),
        "{}",
        opened.text_of(band[0])
    );
    opened.character("o");
    assert_eq!(opened.titles().last().map(String::as_str), Some("finished"));
    opened.character("o");
    assert_eq!(opened.titles().len(), 4);
}

#[test]
fn the_keys_move_the_selection_fold_a_parent_and_ask_for_a_thread() {
    let opened = open(sample());
    opened.character("j");
    assert_eq!(opened.selected(), Some(2));
    opened.key(Key::Named(NamedKey::ArrowDown));
    assert_eq!(opened.selected(), Some(3));
    opened.character("k");
    assert_eq!(opened.selected(), Some(2));

    // `h` on a leaf selects its parent; again, it folds the parent, and
    // the children's rows go.
    opened.character("h");
    assert_eq!(opened.selected(), Some(1));
    opened.character("h");
    assert_eq!(opened.titles(), ["first", "second"]);
    opened.character("j");
    assert_eq!(opened.selected(), Some(4));
    opened.character("k");
    opened.character("l");
    assert_eq!(opened.titles().len(), 4);

    opened.character("j");
    opened.key(Key::Named(NamedKey::Enter));
    assert_eq!(*opened.asked.borrow(), [Asked::OpenThread(2)]);

    // A click selects, and the fold's own triangle folds.
    let (x, y) = opened.middle(opened.all("row")[3]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(opened.selected(), Some(4));
    let (x, y) = opened.middle(opened.all("disclosure")[0]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(opened.titles(), ["first", "second"]);
    assert_eq!(opened.selected(), Some(4));
}

#[test]
fn a_dragged_row_shows_where_it_would_land_among_its_siblings() {
    let opened = open(sample());
    let rows = opened.all("row");
    // The second top-level task, carried over the top half of the first.
    let (from_x, from_y) = opened.middle(rows[3]);
    let (to_x, to_y) = opened.middle(rows[0]);
    pointer(&opened.app, from_x, from_y, Pointer::Move);
    pointer(&opened.app, from_x, from_y, Pointer::Down);
    pointer(&opened.app, from_x, from_y - 10., Pointer::Drag);
    pointer(&opened.app, to_x, to_y - 8., Pointer::Drag);
    opened.app.resolve();
    assert_eq!(opened.all("carried"), vec![rows[3]]);
    assert_eq!(opened.all("ghost").len(), 1);
    assert_eq!(opened.all("above").len(), 1);
    // Over a row that is not its sibling there is nowhere to land.
    let (x, y) = opened.middle(rows[1]);
    pointer(&opened.app, x, y, Pointer::Drag);
    opened.app.resolve();
    assert!(opened.all("line").is_empty());

    // Dropped where it can land, the move is asked of the store, which
    // here refuses writes and says so.
    pointer(&opened.app, to_x, to_y - 8., Pointer::Drag);
    pointer(&opened.app, to_x, to_y - 8., Pointer::Up);
    opened.until("the refusal", || !opened.all("notice").is_empty());
    assert!(
        opened
            .text_of(opened.all("notice")[0])
            .contains("read-only")
    );
    assert!(opened.all("carried").is_empty());
    assert_eq!(opened.selected(), Some(4));
}
