//! An extension shown in a preview pane from its component's file: its
//! view drawn in this document and driven here, what it asks of the page
//! answered, the file read again when it is replaced, and a call that
//! does not come back ended.
//!
//! The extensions shown are the fixture (`plugins/fixture`), whose views
//! are each there for one thing the host does for a guest, and the
//! components' gallery (`plugins/gallery`). Both are built before these
//! run (`scripts/gate.sh`): `scripts/extensions.sh build`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Waker;
use std::time::{Duration, Instant};

use horizon::background::Background;
use horizon::plugin_host::Limits;
use horizon::preview::{Preview, Target, view};
use horizon::wake::Wake;
use horizon_theme_css::{Theme, themed};
use horizon_ui::App;
use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::Set;
use horizon_ui::testing::{Pointer, click_at, pointer};
use keyboard_types::{Key, Modifiers};

/// The component of the extension in `plugins/<name>`, where cargo builds
/// it.
fn component(name: &str) -> PathBuf {
    let built = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "target/wasm32-wasip2/debug/horizon_{name}_plugin.wasm"
    ));
    assert!(
        built.is_file(),
        "no {}: run scripts/extensions.sh build",
        built.display()
    );
    built
}

fn fixture() -> PathBuf {
    component("fixture")
}

mod support;

struct Opened {
    app: App,
    preview: Preview,
    /// The element of the view the test checks: what it looks under.
    root: NodeId,
}

/// A preview pane on the extension whose component is at `path`, made on
/// `arguments`, on a page that has the theme and the pane's own rules and
/// none of the view's.
fn open_at(path: &Path, arguments: &[&str]) -> Opened {
    open_with(path, arguments, false, Limits::default())
}

/// Told when what was done off the UI thread has come back.
struct Arrived(AtomicBool);

impl std::task::Wake for Arrived {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

/// [`open_at`]; with `read_first`, the component is compiled and back
/// before the page is first laid out, as when it was compiled before.
fn open_with(path: &Path, arguments: &[&str], read_first: bool, limits: Limits) -> Opened {
    let arrived = Arc::new(Arrived(AtomicBool::new(false)));
    let told = Wake::default();
    told.set(Waker::from(arrived.clone()));
    let mut made = None;
    let app = App::new(600, 500, "Test", horizon_theme_css::CSS, || {
        let target = RwSignal::new(None);
        let (root, preview) = view(
            target.into(),
            Background::new(told),
            limits,
            Wake::default(),
        );
        made = Some((root, preview, target));
        themed(Theme::built_in, root)
    });
    let (root, preview, target) = made.unwrap();
    // The pane has the keys before its extension is read, as a pane of
    // the shell does: they go on to what the extension shows.
    root.focus();
    target.set(Some(Target {
        arguments: arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect(),
        directory: PathBuf::new(),
        path: path.to_path_buf(),
    }));
    let opened = Opened {
        app,
        preview,
        root: root.0,
    };
    // The component is compiled off this thread.
    opened.app.run_effects();
    if read_first {
        let started = Instant::now();
        while !arrived.0.load(Ordering::Acquire) {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the component never came back"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    opened.until("the extension", || !opened.all("shown").is_empty());
    opened.app.resolve();
    opened
}

/// The fixture's view `name`.
fn open(name: &str) -> Opened {
    open_at(&fixture(), &[name])
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
        horizon_ui::testing::find(&self.app, self.root, class)
    }

    fn text_of(&self, node: NodeId) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.get_node(node).unwrap().text_content()
    }

    /// The titles of the rows the preview shows (a shell around it has
    /// titles of its own).
    fn titles(&self) -> Vec<String> {
        let found: Vec<NodeId> = (self.all("preview").into_iter())
            .flat_map(|preview| horizon_ui::testing::find(&self.app, preview, "title"))
            .collect();
        let document = self.app.document();
        let document = document.borrow();
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

    fn character(&self, character: &str) {
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key: Key::Character(character.into()),
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
}

#[test]
fn the_keys_reach_an_extension_read_before_the_pane_is_first_drawn() {
    // Compiled before (the cache has it), the extension is placed in the
    // frame the pane is given the keys in: the keys still go on to it.
    let opened = open_with(&fixture(), &["rows"], true, Limits::default());
    opened.until("the rows", || opened.titles().len() == 3);
    opened.character("j");
    assert_eq!(opened.selected(), Some(1));
}

#[test]
fn a_view_in_an_extension_is_drawn_in_this_document_and_takes_its_keys() {
    let opened = open("rows");
    opened.until("the rows", || opened.titles().len() == 3);
    assert_eq!(opened.titles(), ["first", "second", "third"]);
    assert_eq!(opened.selected(), Some(0));

    // The extension's style sheet came with it: a row is one line, its
    // tail at the far end of it from its title.
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

    // Keys reach the extension's handlers, and its effects change the
    // page.
    opened.character("j");
    assert_eq!(opened.selected(), Some(1));
    opened.character("j");
    assert_eq!(opened.selected(), Some(2));
    opened.character("k");
    assert_eq!(opened.selected(), Some(1));
    // A click selects.
    let (x, y) = opened.middle(opened.all("row")[0]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(opened.selected(), Some(0));
}

#[test]
fn an_extension_hears_what_is_selected_of_its_own_text() {
    let opened = open("selection");
    opened.until("the words", || opened.all("words").len() == 1);
    assert_eq!(opened.text_of(opened.all("said")[0]), "");
    // "words", selected as a drag over it would.
    let words = horizon_ui::El(opened.all("words")[0]);
    horizon_ui::testing::select(&opened.app, (words, 5), (words, 10));
    opened.until("the selection said", || {
        opened.text_of(opened.all("said")[0]) == "words"
    });
}

#[test]
fn the_components_are_drawn_from_an_extension_with_their_sheets() {
    let opened = open_at(&component("gallery"), &[]);
    opened.until("the chips", || opened.all("chip").len() >= 6);
    // A chip's sheet came with it: it has its padding.
    let document = opened.app.document();
    let document = document.borrow();
    let chip = document.get_node(opened.all("chip")[0]).unwrap();
    assert!(chip.final_layout().padding.left > 0.);
}

#[test]
fn a_drag_in_an_extension_s_view_asks_this_page_where_it_is() {
    let opened = open("rows");
    opened.until("the rows", || opened.titles().len() == 3);
    let rows = opened.all("row");
    // The third row, carried over the first: what is under the pointer
    // the extension asks this page.
    let (from_x, from_y) = opened.middle(rows[2]);
    let (to_x, to_y) = opened.middle(rows[0]);
    pointer(&opened.app, from_x, from_y, Pointer::Move);
    pointer(&opened.app, from_x, from_y, Pointer::Down);
    pointer(&opened.app, from_x, from_y - 10., Pointer::Drag);
    pointer(&opened.app, to_x, to_y, Pointer::Drag);
    opened.until("the row under the pointer", || {
        opened.all("under") == vec![rows[0]]
    });
    assert_eq!(opened.all("carried"), vec![rows[2]]);
    // Over another row, that one.
    let (x, y) = opened.middle(rows[1]);
    pointer(&opened.app, x, y, Pointer::Drag);
    opened.until("the other row", || opened.all("under") == vec![rows[1]]);
    pointer(&opened.app, x, y, Pointer::Up);
    opened.until("the row put down", || opened.all("carried").is_empty());
    assert!(opened.all("under").is_empty());
}

#[test]
fn what_cannot_be_shown_is_said_in_the_pane() {
    // A view the extension refuses to make a pane on, in its own words.
    let opened = open("no-such-view");
    opened.app.run_effects();
    opened.app.resolve();
    let said = opened.all("failed");
    assert_eq!(said.len(), 1);
    assert!(opened.text_of(said[0]).contains("no-such-view"));

    // What of the host's its manifest does not say it uses, whoever shows
    // it.
    let opened = open("sessions");
    opened.until("what it was refused", || !opened.all("failed").is_empty());
    let said = opened.text_of(opened.all("failed")[0]);
    assert!(
        said.contains("sessions.all is not in this plugin's uses"),
        "{said}"
    );

    // A file that is not an extension.
    let broken = std::env::temp_dir().join(format!("hzb-broken-{}.wasm", std::process::id()));
    std::fs::write(&broken, b"not a component").unwrap();
    let opened = open_at(&broken, &["rows"]);
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(opened.all("failed").len(), 1);
    assert!(opened.all("row").is_empty());
    let _ = std::fs::remove_file(&broken);
}

#[test]
fn an_extension_s_file_replaced_is_read_and_shown_again() {
    // A copy, to replace without touching the build's.
    let copy = std::env::temp_dir().join(format!("hzb-preview-{}", std::process::id()));
    std::fs::create_dir_all(&copy).unwrap();
    let file = copy.join("extension.wasm");
    std::fs::copy(fixture(), &file).unwrap();
    let opened = open_at(&file, &["rows"]);
    opened.until("the rows", || !opened.titles().is_empty());
    let first = opened.all("shown")[0];

    // A build writes the file anew: what was shown stays until the new
    // one is read.
    std::fs::copy(fixture(), copy.join("next.wasm")).unwrap();
    std::fs::rename(copy.join("next.wasm"), &file).unwrap();
    opened.until("the file read again", || {
        opened
            .all("shown")
            .first()
            .is_some_and(|shown| *shown != first)
    });
    opened.until("the rows again", || !opened.titles().is_empty());
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
fn a_call_into_an_extension_that_does_not_come_back_is_ended() {
    // A limit of the test's own: what is checked is that the call is ended
    // at it, not how long the host's own is.
    let limits = Limits {
        call: Duration::from_millis(300),
        ..Limits::default()
    };
    let started = Instant::now();
    let opened = open_with(&fixture(), &["stuck"], false, limits);
    let said = opened.all("failed");
    assert_eq!(said.len(), 1, "{}", opened.text());
    // Ended at the limit, not before it.
    assert!(started.elapsed() >= limits.call);
}

#[test]
fn the_control_plane_shows_a_preview_in_a_pane_of_the_shell() {
    use horizon::workspace::{Opening, Shell, Spawn, css, view as shell_view};
    use horizon_control::contract::{EnvelopeBody, Invoke};
    use horizon_control::host::executor::ControlRequest;
    use horizon_terminal_core::TerminalSize;
    use horizon_ui::reactive_graph::traits::WithUntracked;
    use horizon_workspace::Workspace;

    let (mut shell, mut top): (Option<Shell>, _) = (None, None);
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = shell_view(spawn, Opening::default());
        (shell, top) = (Some(built), Some(root.0));
        root
    });
    app.resolve();
    let shell = shell.unwrap();
    let opened = Opened {
        app,
        // (Not this test's: the panes' own are asked of the shell.)
        preview: view(
            RwSignal::new(None).into(),
            Background::default(),
            Limits::default(),
            Wake::default(),
        )
        .1,
        // (The shell's: the previews are in its panes.)
        root: top.unwrap(),
    };
    // (`horizon new preview <extension.wasm> [<argument>...] [--active]`,
    // as it reaches the shell.)
    let ask = |path: &Path, arguments: &[&str], activate: bool| {
        let mut given = vec![path.to_string_lossy().into_owned()];
        given.extend(arguments.iter().map(|argument| argument.to_string()));
        let answer = shell.control(&ControlRequest::Invoke(Invoke {
            command: "new".to_string(),
            args: serde_json::json!({
                "plugin": "preview",
                "arguments": given,
                "activate": activate,
            }),
        }));
        opened.app.run_effects();
        opened.app.resolve();
        answer
    };
    let previews = || {
        shell.model.with_untracked(|model: &Workspace| {
            (model.all_pane_ids().into_iter())
                .filter(|pane| {
                    model
                        .pane_plugin(*pane)
                        .is_some_and(|plugin| plugin.name == "preview")
                })
                .collect::<Vec<_>>()
        })
    };

    let answer = ask(&fixture(), &["rows"], true);
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    assert_eq!(previews().len(), 1);
    opened.until("the rows", || opened.titles().len() == 3);
    // The pane has the keys: the rows' own move their selection.
    opened.character("j");
    assert_eq!(opened.selected(), Some(1));

    // The same file again is the same pane's, made on what was asked for.
    let answer = ask(&fixture(), &["selection"], false);
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    assert_eq!(previews().len(), 1);
    opened.until("the other view", || {
        opened.all("words").len() == 1 && opened.titles().is_empty()
    });

    // The command reads the file again.
    let shown = opened.all("shown")[0];
    assert!(shell.execute_plugin("reload-preview"));
    opened.app.run_effects();
    opened.until("a reload", || {
        opened.all("shown").first().is_some_and(|now| *now != shown)
    });

    // What is not a file is refused, and opens nothing.
    let answer = ask(Path::new("/no/such/extension.wasm"), &[], false);
    std::assert_matches!(answer, EnvelopeBody::Error { .. }, "{answer:?}");
    assert_eq!(previews().len(), 1);
}
