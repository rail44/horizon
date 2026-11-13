//! One board task's thread over a store handed in whole (which refuses
//! writes), driven the way an owner drives it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use horizon::background::Background;
use horizon::board::thread::{Asked, CSS, view};
use horizon::board::{BoardThread, Source};
use horizon_board::{BoardEvent, Comment, Item, Store, sample_envelopes};
use horizon_theme_css::{Theme, themed};
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::GetUntracked;
use horizon_ui::testing::click_at;
use horizon_workspace::SessionId;
use keyboard_types::{Key, Modifiers, NamedKey};

struct Opened {
    app: App,
    thread: BoardThread,
    asked: Rc<RefCell<Vec<Asked>>>,
}

fn post(id: &str, author: &str, text: &str) -> Comment {
    Comment {
        id: id.to_string(),
        author: author.to_string(),
        text: text.to_string(),
        at: None,
        source: None,
    }
}

/// A task with a body, three posts (the agent's long enough to fold) and
/// a bound session, and a second task with nothing.
fn sample(session: SessionId) -> Vec<Item> {
    let long: String = (1..=60).map(|line| format!("line {line}\n\n")).collect();
    vec![
        Item {
            id: 7,
            title: "the task".to_string(),
            body: "what it is about".to_string(),
            status: "review".to_string(),
            rank: "a".to_string(),
            comments: vec![
                post("c1", "owner", "a question"),
                post("c2", "agent", &long),
                post("c3", "system", "a note"),
            ],
            session_id: Some(session.as_uuid().to_string()),
            ..Item::default()
        },
        Item {
            id: 8,
            title: "another".to_string(),
            rank: "b".to_string(),
            ..Item::default()
        },
    ]
}

fn open(items: Vec<Item>, task: u64) -> Opened {
    open_at(items, task, 700)
}

/// The view `width` wide.
fn open_at(items: Vec<Item>, task: u64, width: u32) -> Opened {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = asked.clone();
    let events = items
        .into_iter()
        .map(|item| BoardEvent::ItemStored { id: item.id, item });
    let source = Source::Ready(Store::in_memory(sample_envelopes(events)));
    let mut thread = None;
    let css = [horizon_theme_css::CSS, CSS].concat();
    let app = App::new(width, 600, &css, || {
        let (root, state) = view(source, Background::default(), task, move |asked| {
            log.borrow_mut().push(asked)
        });
        thread = Some(state);
        themed(Theme::built_in, root.focus())
    });
    let opened = Opened {
        app,
        thread: thread.unwrap(),
        asked,
    };
    // The store is read off the UI thread.
    opened.until("the page", || {
        !opened.all("title").is_empty() || !opened.all("missing").is_empty()
    });
    opened
}

fn collect(document: &BaseDocument, id: NodeId, class: &str, found: &mut Vec<NodeId>) {
    let node = document.get_node(id).expect("a node");
    let named = (node.attr(local_name!("class")))
        .is_some_and(|classes| horizon_ui::testing::has_class(classes, class));
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

    fn texts(&self, class: &str) -> Vec<String> {
        (self.all(class).into_iter())
            .map(|node| self.text_of(node))
            .collect()
    }

    fn click(&self, node: NodeId) {
        let (x, y) = {
            let document = self.app.document();
            let document = document.borrow();
            let bounds = document.get_client_bounding_rect(node).expect("a box");
            (
                (bounds.x + bounds.width / 2.) as f32,
                (bounds.y + bounds.height / 2.) as f32,
            )
        };
        click_at(&self.app, x, y);
        self.app.resolve();
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

    /// Text as an input method commits it into the focused field.
    fn commit(&self, text: &str) {
        self.app
            .dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        self.app.resolve();
    }
}

#[test]
fn a_task_is_its_band_its_body_and_its_posts_and_what_is_shown_is_read() {
    let opened = open(sample(SessionId::new()), 7);
    assert_eq!(opened.texts("title"), ["the task"]);
    assert_eq!(opened.texts("author"), ["Owner", "Agent", "System"]);
    let actions = opened.texts("button");
    assert_eq!(actions, ["Reply", "Close", "Session"]);
    assert!(
        opened
            .text_of(opened.all("column")[0])
            .contains("what it is about")
    );
    // The status is a chip in the band.
    assert!(opened.texts("chip").contains(&"review".to_string()));

    // Posts come up unread; one that has been in view is read, and a long
    // one that is read folds to its opening lines.
    opened.until("the first post read", || {
        !opened.texts("chip").contains(&"Unread".to_string())
            || opened
                .texts("chip")
                .iter()
                .filter(|chip| *chip == "Unread")
                .count()
                < 3
    });
    opened.until("the long post folded", || !opened.all("more").is_empty());
    let folded = opened.text_of(opened.all("post")[1]);
    assert!(
        folded.contains("line 1") && !folded.contains("line 60"),
        "{folded}"
    );

    // The session's button asks the shell for the bound session.
    opened.click(opened.all("button")[2]);
    assert!(matches!(opened.asked.borrow()[..], [Asked::OpenSession(_)]));
}

#[test]
fn the_keys_move_the_post_cursor_fold_a_post_and_reach_the_fields() {
    let opened = open(sample(SessionId::new()), 7);
    opened.until("the long post folded", || !opened.all("more").is_empty());
    assert_eq!(opened.thread.cursor.get_untracked(), Some(0));
    assert_eq!(opened.all("current"), vec![opened.all("post")[0]]);
    opened.character("j");
    assert_eq!(opened.thread.cursor.get_untracked(), Some(1));
    // `e` opens the folded post under the cursor, and shuts it again.
    opened.character("e");
    opened.until("the post in full", || opened.all("more").is_empty());
    assert!(opened.text_of(opened.all("post")[1]).contains("line 60"));
    opened.character("e");
    opened.until("the post folded", || !opened.all("more").is_empty());
    opened.character("k");
    assert_eq!(opened.thread.cursor.get_untracked(), Some(0));

    // Enter goes to the reply field; what is sent from it is refused by
    // this store, which says so. Escape goes back to the posts.
    opened.key(Key::Named(NamedKey::Enter));
    opened.commit("an answer");
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the refusal", || !opened.all("notice").is_empty());
    assert!(
        opened
            .text_of(opened.all("notice")[0])
            .contains("read-only")
    );
    opened.key(Key::Named(NamedKey::Escape));
    opened.character("j");
    assert_eq!(opened.thread.cursor.get_untracked(), Some(1));
}

#[test]
fn a_task_that_is_not_there_says_so_and_another_is_shown_in_its_place() {
    let opened = open(sample(SessionId::new()), 99);
    assert_eq!(opened.texts("missing").len(), 1);
    opened.thread.show_task(8);
    opened.until("the other task", || opened.texts("title") == ["another"]);
    assert!(opened.all("missing").is_empty());
    assert!(opened.all("post").is_empty());
    assert_eq!(opened.texts("button"), ["Reply", "Close"]);
    opened.thread.show_task(7);
    opened.until("the first task", || opened.texts("title") == ["the task"]);
    assert_eq!(opened.all("post").len(), 3);
}

#[test]
fn the_actions_are_beside_the_title_or_in_a_narrow_band_under_it() {
    let bounds = |opened: &Opened, class: &str| {
        let node = opened.all(class)[0];
        let document = opened.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (bounds.x, bounds.y, bounds.width, bounds.height)
    };
    // Wide enough for both: the actions at the title's right, level with
    // it, and the field for a status at the end of the line under them.
    let opened = open(sample(SessionId::new()), 7);
    opened.app.resolve();
    assert!(!opened.thread.stacked.get_untracked());
    let (title, actions) = (bounds(&opened, "title"), bounds(&opened, "actions"));
    assert!(actions.0 >= title.0 + title.2, "{title:?} {actions:?}");
    assert!((actions.1 - title.1).abs() < 2., "{title:?} {actions:?}");
    let (chips, status) = (bounds(&opened, "chips"), bounds(&opened, "status"));
    assert!(status.0 > chips.0 + chips.2, "{chips:?} {status:?}");
    assert!(chips.1 >= title.1 + title.3, "{title:?} {chips:?}");
    assert_eq!(opened.all("actions").len(), 1);

    // Too narrow: the title has the row, and the actions are at the end
    // of the line under it, after the field.
    let opened = open_at(sample(SessionId::new()), 7, 420);
    opened.until("the band stacked", || opened.thread.stacked.get_untracked());
    opened.app.resolve();
    let (title, actions) = (bounds(&opened, "title"), bounds(&opened, "actions"));
    assert!(actions.1 >= title.1 + title.3, "{title:?} {actions:?}");
    let status = bounds(&opened, "status");
    assert!(actions.0 >= status.0 + status.2, "{status:?} {actions:?}");
    assert!(title.2 > 300., "the title has the row: {title:?}");
    assert_eq!(opened.all("actions").len(), 1);
    assert_eq!(opened.texts("button"), ["Reply", "Close", "Session"]);
}
