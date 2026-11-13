//! A view shown from a wasm plugin: the board's task list, built in the
//! plugin over sample data, drawn in this document and driven here the
//! way `tests/board.rs` drives the same view built natively.
//!
//! The plugin is `../preview-plugin` built for `wasm32-wasip2`, which
//! `scripts/check-preview-plugin.sh` does before it runs these.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use horizon::background::Background;
use horizon::preview::{CSS, Preview, Target, view};
use horizon::wake::Wake;
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
        "no {}: run scripts/check-preview-plugin.sh",
        built.display()
    );
    built
}

mod support;

struct Opened {
    app: App,
    preview: Preview,
}

/// The preview `name` of the plugin at `path`, on a page that has the
/// theme and the pane's own rules and none of the view's.
fn open_at(path: &std::path::Path, name: &str) -> Opened {
    let css = [horizon_theme_css::CSS, CSS].concat();
    let mut made = None;
    let app = App::new(600, 500, &css, || {
        let (root, preview) = view(Background::default(), Wake::default());
        made = Some((root, preview));
        themed(Theme::built_in, root)
    });
    let (root, preview) = made.unwrap();
    // The pane has the keys before its plugin is read, as a pane of the
    // shell does: they go on to what the plugin shows.
    root.focus();
    preview.show(Target {
        path: path.to_path_buf(),
        name: Some(name.to_string()),
    });
    let opened = Opened { app, preview };
    // The file is compiled off this thread.
    opened.app.run_effects();
    opened.until("the plugin", || !opened.all("shown").is_empty());
    opened.app.resolve();
    opened
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

    /// The titles of the rows the preview shows (a shell around it has
    /// titles of its own).
    fn titles(&self) -> Vec<String> {
        let document = self.app.document();
        let document = document.borrow();
        let mut found = Vec::new();
        for preview in self.all("preview") {
            collect(&document, preview, "title", &mut found);
        }
        (found.into_iter())
            .map(|title| document.get_node(title).unwrap().text_content())
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
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview-plugin.sh"]
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
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview-plugin.sh"]
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
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview-plugin.sh"]
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

#[test]
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview-plugin.sh"]
fn a_plugin_s_file_replaced_is_read_and_shown_again() {
    // A copy, to replace without touching the build's.
    let copy = std::env::temp_dir().join(format!("hzb-preview-{}", std::process::id()));
    std::fs::create_dir_all(&copy).unwrap();
    let file = copy.join("plugin.wasm");
    std::fs::copy(plugin(), &file).unwrap();
    let opened = open_at(&file, "board-list");
    opened.until("the tasks", || !opened.titles().is_empty());
    let first = opened.all("shown")[0];

    // A build writes the file anew: what was shown stays until the new
    // one is read.
    std::fs::copy(plugin(), copy.join("next.wasm")).unwrap();
    std::fs::rename(copy.join("next.wasm"), &file).unwrap();
    opened.until("the file read again", || {
        opened
            .all("shown")
            .first()
            .is_some_and(|shown| *shown != first)
    });
    opened.until("the tasks again", || !opened.titles().is_empty());
    assert_eq!(opened.all("shown").len(), 1);

    // And when asked to.
    let second = opened.all("shown")[0];
    opened.preview.reload();
    opened.app.run_effects();
    opened.until("a reload", || {
        opened
            .all("shown")
            .first()
            .is_some_and(|shown| *shown != second)
    });
    let _ = std::fs::remove_dir_all(&copy);
}

#[test]
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview-plugin.sh"]
fn a_call_into_a_plugin_that_does_not_come_back_is_ended() {
    let started = Instant::now();
    let opened = open("stuck");
    let said = opened.all("failed");
    assert_eq!(said.len(), 1, "{}", opened.text());
    // (The limit is five seconds.)
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(started.elapsed() < Duration::from_secs(60));
}

#[test]
#[ignore = "needs the plugin built for wasm32-wasip2: scripts/check-preview-plugin.sh"]
fn the_control_plane_shows_a_preview_in_a_pane_of_the_shell() {
    use horizon::workspace::{Opening, Shell, Spawn, css, view as shell_view};
    use horizon_control::contract::{EnvelopeBody, Invoke};
    use horizon_control::host::executor::ControlRequest;
    use horizon_terminal_core::TerminalSize;
    use horizon_ui::reactive_graph::traits::WithUntracked;
    use horizon_workspace::commands::CommandId;
    use horizon_workspace::{PaneKind, ViewKind, Workspace};

    let mut shell: Option<Shell> = None;
    let app = App::new(1200, 800, &css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = shell_view(spawn, Opening::default());
        shell = Some(built);
        root
    });
    app.resolve();
    let shell = shell.unwrap();
    let opened = Opened {
        app,
        // (Not this test's: the panes' own are asked of the shell.)
        preview: view(Background::default(), Wake::default()).1,
    };
    let ask = |args: serde_json::Value| {
        let answer = shell.control(&ControlRequest::Invoke(Invoke {
            command: "preview".to_string(),
            args,
        }));
        opened.app.run_effects();
        opened.app.resolve();
        answer
    };
    let previews = || {
        shell.model.with_untracked(|model: &Workspace| {
            (model.all_pane_ids().into_iter())
                .filter(|pane| model.pane_kind(*pane) == Some(PaneKind::View(ViewKind::Preview)))
                .collect::<Vec<_>>()
        })
    };

    let answer =
        ask(serde_json::json!({ "path": plugin(), "name": "board-list", "activate": true }));
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    assert_eq!(previews().len(), 1);
    opened.until("the tasks", || !opened.titles().is_empty());
    assert_eq!(
        opened.titles(),
        ["first", "child one", "child two", "second"]
    );
    // The pane has the keys: the list's own move its selection.
    opened.character("j");
    assert_eq!(opened.selected(), Some(1));

    // The same file again is the same pane's, with the preview asked for.
    let answer = ask(serde_json::json!({ "path": plugin(), "name": "board-list-empty" }));
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    assert_eq!(previews().len(), 1);
    opened.until("the empty list", || opened.titles().is_empty());

    // The command reads the file again.
    let shown = opened.all("shown")[0];
    shell.execute(CommandId::ReloadPreview);
    opened.app.run_effects();
    opened.until("a reload", || {
        opened.all("shown").first().is_some_and(|now| *now != shown)
    });

    // What is not a file is refused, and opens nothing.
    let answer = ask(serde_json::json!({ "path": "/no/such/plugin.wasm" }));
    assert!(matches!(answer, EnvelopeBody::Error { .. }), "{answer:?}");
    assert_eq!(previews().len(), 1);
}
