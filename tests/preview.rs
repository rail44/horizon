//! A view shown from a wasm plugin: the board's task list, built in the
//! plugin over sample data, drawn in this document and driven here the
//! way `tests/board.rs` drives the same view built natively.
//!
//! The plugin is `../preview-plugin` built for `wasm32-wasip2`, which
//! `scripts/check-preview.sh` does before it runs these.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use horizon::preview::{CSS, view};
use horizon_theme_css::{Theme, themed};
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::testing::{Pointer, click_at, pointer};
use keyboard_types::{Key, Modifiers, NamedKey};

fn plugin() -> PathBuf {
    let built = std::env::var_os("HORIZON_PREVIEW_PLUGIN").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/wasm32-wasip2/debug/horizon_preview_plugin.wasm")
        },
        PathBuf::from,
    );
    assert!(
        built.is_file(),
        "no {}: run scripts/check-preview.sh",
        built.display()
    );
    built
}

struct Opened {
    app: App,
}

/// The preview `name` of the plugin at `path`, on a page that has the
/// theme and the pane's own rules and none of the view's.
fn open_at(path: &std::path::Path, name: &str) -> Opened {
    let css = [horizon_theme_css::CSS, CSS].concat();
    let app = App::new(600, 500, &css, || {
        themed(Theme::built_in, view(path, name).focus_within())
    });
    app.resolve();
    Opened { app }
}

fn open(name: &str) -> Opened {
    open_at(&plugin(), name)
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
    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        loop {
            self.app.resolve();
            if met() {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no {what} in:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn text(&self) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.root_element().text_content()
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

    fn titles(&self) -> Vec<String> {
        (self.all("title").into_iter())
            .map(|title| self.text_of(title))
            .collect()
    }

    /// Which of the rows on the page is the selected one.
    fn selected(&self) -> Option<usize> {
        let selected = self.all("selected");
        self.all("row")
            .iter()
            .position(|row| selected.contains(row))
    }

    fn bounds(&self, node: NodeId) -> (f64, f64, f64, f64) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (bounds.x, bounds.y, bounds.width, bounds.height)
    }

    fn middle(&self, node: NodeId) -> (f32, f32) {
        let (x, y, width, height) = self.bounds(node);
        ((x + width / 2.) as f32, (y + height / 2.) as f32)
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
}

#[test]
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview.sh"]
fn a_view_in_a_plugin_is_drawn_in_this_document_and_takes_its_keys() {
    let opened = open("board-list");
    // The store is read in the plugin, and taken in after a layout here.
    opened.until("the tasks", || !opened.titles().is_empty());
    assert_eq!(
        opened.titles(),
        ["first", "child one", "child two", "second"]
    );
    assert_eq!(opened.selected(), Some(0));
    let chips: Vec<String> = (opened.all("chip").into_iter())
        .map(|chip| opened.text_of(chip))
        .collect();
    assert_eq!(chips, ["doing", "review"]);

    // The plugin's style sheet came with it: a row is one line, its
    // status at the far end of it from its title.
    let row = opened.all("row")[0];
    let (title, tail) = (opened.all("title")[0], opened.all("tail")[0]);
    let (row, title, tail) = (
        opened.bounds(row),
        opened.bounds(title),
        opened.bounds(tail),
    );
    assert!(tail.0 >= title.0 + title.2, "{title:?} {tail:?}");
    assert!(tail.1 < row.1 + row.3 && title.1 < row.1 + row.3);
    assert!(tail.0 + tail.2 > row.0 + row.2 - 40., "{row:?} {tail:?}");

    // Keys reach the plugin's handlers, and its effects change the page.
    opened.character("j");
    assert_eq!(opened.selected(), Some(1));
    opened.key(Key::Named(NamedKey::ArrowDown));
    assert_eq!(opened.selected(), Some(2));
    opened.character("k");
    assert_eq!(opened.selected(), Some(1));
    // `h` on a leaf selects its parent; again, it folds the parent, and
    // the children's rows go.
    opened.character("h");
    assert_eq!(opened.selected(), Some(0));
    opened.character("h");
    assert_eq!(opened.titles(), ["first", "second"]);
    opened.character("l");
    assert_eq!(opened.titles().len(), 4);
    // The finished task is behind the band, which `o` opens and shuts.
    opened.character("o");
    assert_eq!(opened.titles().last().map(String::as_str), Some("finished"));
    opened.character("o");
    assert_eq!(opened.titles().len(), 4);

    // A click selects, and the fold's own triangle folds.
    let (x, y) = opened.middle(opened.all("row")[3]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(opened.selected(), Some(3));
    let (x, y) = opened.middle(opened.all("disclosure")[0]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(opened.titles(), ["first", "second"]);

    // What is typed into the field is the plugin's to send: the store of
    // a preview refuses the write, and the view says so.
    let (x, y) = opened.middle(opened.all("add")[0]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    opened.app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit(
        "a new task".to_string(),
    )));
    opened.app.resolve();
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the refusal", || !opened.all("notice").is_empty());
    assert!(
        opened
            .text_of(opened.all("notice")[0])
            .contains("read-only")
    );
}

#[test]
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview.sh"]
fn a_drag_in_a_plugin_s_view_asks_this_page_where_it_is() {
    let opened = open("board-list");
    opened.until("the tasks", || !opened.titles().is_empty());
    let rows = opened.all("row");
    // The second top-level task, carried over the top half of the first:
    // what is under the pointer, and where its row is, the plugin asks.
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
    pointer(&opened.app, to_x, to_y - 8., Pointer::Drag);
    pointer(&opened.app, to_x, to_y - 8., Pointer::Up);
    opened.until("the refusal", || !opened.all("notice").is_empty());
    assert!(opened.all("carried").is_empty());
}

#[test]
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview.sh"]
fn what_cannot_be_shown_is_said_in_the_pane() {
    // A preview the plugin does not have.
    let opened = open("no-such-preview");
    opened.app.run_effects();
    opened.app.resolve();
    let said = opened.all("failed");
    assert_eq!(said.len(), 1);
    assert!(opened.text_of(said[0]).contains("no-such-preview"));

    // A file that is not a plugin.
    let broken = std::env::temp_dir().join(format!("hzb-broken-{}.wasm", std::process::id()));
    std::fs::write(&broken, b"not a component").unwrap();
    let opened = open_at(&broken, "board-list");
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(opened.all("failed").len(), 1);
    assert!(opened.all("row").is_empty());
    let _ = std::fs::remove_file(&broken);
}
