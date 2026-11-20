//! The wiki view, an extension (`plugins/wiki`), in a pane of the shell
//! over a project's wiki on disk: a page drawn, a passage selected and
//! commented on, the thread it starts read and replied in, and what the
//! answering side writes arriving by the files.
//!
//! The component is built before these run (`scripts/gate.sh`):
//! `scripts/extensions.sh build`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horizon::workspace::{Opening, Shell, Spawn, css, view};
use horizon_control::contract::{EnvelopeBody, Invoke};
use horizon_control::host::executor::ControlRequest;
use horizon_terminal_core::TerminalSize;
use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_ui::testing::{click_at, select};
use horizon_ui::{App, El};
use horizon_wiki::Store;
use horizon_workspace::Workspace;
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Opened {
    _scratch: Scratch,
    project: PathBuf,
    store: Store,
    app: App,
    shell: Shell,
}

const PAGE: &str = "---\ntitle: Sandbox\nstatus: draft\n---\n\
The **helper** runs `nono` first.\n\n\
Then the agent, see [the other page](other.md).\n\n\
- one item\n- another item\n";

fn open(name: &str) -> Opened {
    open_at(name, 900)
}

/// A shell `width` wide with the extensions it comes with, over a project
/// (a repository) with a wiki of two pages, and a wiki pane on the page
/// `sandbox`: 900 has room for a margin beside the text, 500 has not.
fn open_at(name: &str, width: u32) -> Opened {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hzb-wiki-{name}-{}", std::process::id())));
    let project = scratch.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    // (Git's own variables, set where the tests run from a hook of git's,
    // would name another repository than the one made here.)
    // SAFETY: nextest runs each test in a process of its own.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            unsafe { std::env::remove_var(name) };
        }
    }
    // A data home of the test's own: the shell lends the extension the
    // wiki in it, and tells it where that is.
    unsafe { std::env::set_var("HORIZON_DATA_HOME", scratch.0.join("data")) };
    let made = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&project)
        .status()
        .unwrap();
    assert!(made.success());
    let store = Store::for_dir(&project).expect("the project's wiki");
    store.write_page("sandbox", PAGE).unwrap();
    store
        .write_page("other", "---\ntitle: Other\n---\nAnother page.\n")
        .unwrap();
    store
        .write_page("index", "---\ntitle: Overview\n---\nWhat the project is.\n")
        .unwrap();

    let extensions =
        horizon::extension::own_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug"));
    let mut shell = None;
    let app = App::new(width, 700, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                extensions,
                ..Opening::default()
            },
        );
        shell = Some(built);
        root
    });
    app.resolve();
    let opened = Opened {
        _scratch: scratch,
        project,
        store,
        app,
        shell: shell.unwrap(),
    };
    // (The extensions are read once the shell is drawn.)
    opened.until("the extensions", || opened.shell.extensions_read());
    let answer = opened.new_wiki(&["sandbox"]);
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    opened.until("the page", || opened.text().contains("runs"));
    opened
}

impl Opened {
    /// `horizon new wiki <arguments>`, from the project.
    fn new_wiki(&self, arguments: &[&str]) -> EnvelopeBody {
        let answer = self.shell.control(&ControlRequest::Invoke(Invoke {
            command: "new".to_string(),
            args: serde_json::json!({
                "plugin": "wiki",
                "arguments": arguments,
                "directory": self.project,
                "activate": true,
            }),
        }));
        self.app.run_effects();
        self.app.resolve();
        answer
    }

    fn model<R>(&self, read: impl FnOnce(&Workspace) -> R) -> R {
        self.shell.model.with_untracked(read)
    }

    /// What the cursor's pane keeps: its title and its state.
    fn kept(&self) -> (String, String) {
        self.model(|model| {
            let plugin = (model.cursor_pane_id())
                .and_then(|pane| model.pane_plugin(pane))
                .expect("the pane's plugin");
            (
                plugin.title.clone(),
                plugin.state.as_str().unwrap_or_default().to_string(),
            )
        })
    }

    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        while !met() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no {what} in:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(5));
            self.app.run_effects();
            self.app.resolve();
        }
    }

    fn text(&self) -> String {
        horizon_ui::testing::text(&self.app)
    }

    /// The elements of a class in the cursor's pane, the wiki's, in
    /// document order.
    fn all(&self, class: &str) -> Vec<NodeId> {
        let pane = (self.model(Workspace::cursor_pane_id))
            .and_then(|pane| self.shell.pane_element(pane))
            .expect("the cursor's pane, drawn");
        horizon_ui::testing::find(&self.app, pane, class)
    }

    fn text_of(&self, node: NodeId) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.get_node(node).unwrap().text_content()
    }

    /// The element a block's own text is in: its first child.
    fn text_root(&self, block: usize) -> El {
        let block = self.all("block")[block];
        let document = self.app.document();
        let document = document.borrow();
        El(document.get_node(block).unwrap().children[0])
    }

    /// A node's box: its left, top, width and height.
    fn bounds(&self, node: NodeId) -> (f64, f64, f64, f64) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (bounds.x, bounds.y, bounds.width, bounds.height)
    }

    /// Selects `passage` in the first block and sends `comment` on it.
    fn comment_on(&self, passage: &str, comment: &str) {
        let threads = self.all("thread").len();
        let root = self.text_root(0);
        let at = (self.text_of(self.all("block")[0]).find(passage)).expect("the passage");
        select(&self.app, (root, at), (root, at + passage.len()));
        self.until("the offer", || !self.all("offer").is_empty());
        self.key(Key::Character("c".into()));
        // (Beside the text, or in the dialog.)
        self.until("the field", || {
            !self.all("new").is_empty() || !self.all("dialog").is_empty()
        });
        self.commit(comment);
        self.key(Key::Named(NamedKey::Enter));
        self.until("the thread", || self.all("thread").len() > threads);
    }

    fn click(&self, node: NodeId) {
        let (x, y, width, height) = self.bounds(node);
        click_at(&self.app, (x + width / 2.) as f32, (y + height / 2.) as f32);
        self.app.run_effects();
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
        self.app.run_effects();
        self.app.resolve();
    }

    /// Text as an input method commits it into the focused field.
    fn commit(&self, text: &str) {
        self.app
            .dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        self.app.run_effects();
        self.app.resolve();
    }

    /// The dialog's `Close`. (A tab's button to close it has the class
    /// too.)
    fn close_of_the_dialog(&self) -> NodeId {
        horizon_ui::testing::find(&self.app, self.all("dialog")[0], "close")[0]
    }

    /// The threads on the page `sandbox`, as the log on disk has them.
    fn threads(&self) -> Vec<horizon_wiki::thread::Thread> {
        let page = horizon_wiki::page::page_id(&self.store.read_page("sandbox").unwrap()).unwrap();
        self.store.threads(page).unwrap()
    }
}

#[test]
fn a_page_is_asked_for_by_its_name_from_its_project() {
    let opened = open("asked");
    let tabs = |opened: &Opened| opened.model(Workspace::tab_count);
    assert_eq!(tabs(&opened), 2);
    // The tab is named for the page's file, which is what the pane keeps.
    opened.until("the title", || opened.kept().0 == "sandbox.md");
    let page = opened.store.page_path("sandbox").unwrap();
    assert!(
        opened.kept().1.contains(&*page.to_string_lossy()),
        "{:?}",
        opened.kept()
    );

    // A page there is not is refused, with the extension's own words.
    let missing = opened.new_wiki(&["no-such-page"]);
    let EnvelopeBody::Error(error) = &missing else {
        panic!("{missing:?}");
    };
    assert!(format!("{error:?}").contains("no page"), "{error:?}");
    // With no page named, the overview, in a pane of its own.
    let answer = opened.new_wiki(&[]);
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    opened.until("the overview", || {
        opened.text().contains("What the project is.")
    });
    assert_eq!(tabs(&opened), 3);
    // Asked for a page a pane shows already, that pane is the one.
    let answer = opened.new_wiki(&["sandbox"]);
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    assert_eq!(tabs(&opened), 3);
}

#[test]
fn a_page_is_its_header_and_its_blocks() {
    let opened = open("page");
    assert_eq!(opened.text_of(opened.all("heading")[0]), "Sandbox");
    // The front matter's other fields, as chips.
    assert_eq!(opened.text_of(opened.all("fields")[0]), "status draft");
    // A paragraph, a paragraph, and each item of the list on its own.
    let blocks: Vec<String> = (opened.all("block").into_iter())
        .map(|block| opened.text_of(block))
        .collect();
    assert_eq!(
        blocks,
        [
            "The helper runs nono first.",
            "Then the agent, see the other page.",
            "one item",
            "another item"
        ]
    );

    // A link to another page of the wiki is followed in place: the pane
    // keeps the page it went to, and goes back from it.
    opened.click(opened.all("link")[0]);
    opened.until("the other page", || opened.text().contains("Another page."));
    opened.until("the page kept", || opened.kept().1.contains("other.md"));
    opened.key(Key::Character("h".into()));
    opened.until("the first page", || opened.text().contains("runs"));
    opened.until("the page kept again", || {
        opened.kept().1.contains("sandbox.md")
    });
}

#[test]
fn a_selected_passage_is_commented_on_and_its_thread_replied_in() {
    let opened = open("thread");
    // "helper", selected in the first block's text: the offer to comment
    // is set after the block, and `c` takes it.
    let root = opened.text_root(0);
    select(&opened.app, (root, 4), (root, 10));
    opened.until("the offer", || opened.all("offer").len() == 1);
    opened.key(Key::Character("c".into()));
    opened.until("the field", || opened.all("new").len() == 1);
    assert!(opened.text_of(opened.all("new")[0]).contains("“helper”"));

    // What is typed and sent starts a thread on the passage: written to
    // the page's log, marked in the page, and open beside its block.
    opened.commit("Which helper?");
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the thread", || !opened.all("thread").is_empty());
    assert!(opened.all("new").is_empty());
    assert_eq!(opened.threads().len(), 1);
    assert_eq!(opened.text_of(opened.all("mark")[0]), "helper");
    let thread = opened.all("thread")[0];
    assert!(opened.text_of(thread).contains("1 comment"));
    assert!(opened.text_of(thread).contains("Which helper?"));
    // The page still reads as it did around the mark.
    assert_eq!(
        opened.text_of(opened.all("block")[0]),
        "The helper runs nono first."
    );

    // The pane has room for a margin: the thread is beside the text, level
    // with the block its passage is in.
    opened.until("the thread beside its block", || {
        let (block, card) = (
            opened.bounds(opened.all("block")[0]),
            opened.bounds(opened.all("thread")[0]),
        );
        card.0 >= block.0 + block.2 && (card.1 - block.1).abs() < 1.
    });

    // The head shuts the thread and opens it; opened, it has the field,
    // and what is sent from it is a reply.
    opened.click(opened.all("head")[0]);
    opened.until("the thread shut", || opened.all("post").is_empty());
    opened.click(opened.all("head")[0]);
    opened.until("the thread open", || opened.all("composer").len() == 1);
    opened.commit("The supervised one.");
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the reply", || opened.all("post").len() == 2);
    assert!(
        opened
            .text_of(opened.all("thread")[0])
            .contains("2 comments")
    );
    let threads = opened.threads();
    assert_eq!(threads[0].replies[0].body.value, "The supervised one.");

    // The answering side lists the comment it is answering beside the
    // log, which the view sees by the file.
    let id = threads[0].root.id.clone();
    opened.store.write_replying(&[id].into()).unwrap();
    opened.until("the answer on its way", || {
        !opened.all("replying").is_empty()
    });
    // Escape leaves the field.
    opened.key(Key::Named(NamedKey::Escape));
    opened.until("the field left", || opened.all("composer").is_empty());
}

#[test]
fn threads_in_the_margin_do_not_cover_each_other() {
    let opened = open("margin");
    // Two threads on the same block.
    opened.comment_on("helper", "Which helper?");
    opened.comment_on("nono", "Why first?");
    opened.until("the second below the first", || {
        let threads = opened.all("thread");
        let (first, second) = (opened.bounds(threads[0]), opened.bounds(threads[1]));
        second.1 >= first.1 + first.3
    });
    // Both open, the margin runs past the end of the text, and the page
    // scrolls to the end of it.
    assert_eq!(opened.all("post").len(), 2);
    let last = opened.bounds(opened.all("thread")[1]);
    let row = opened.bounds(opened.all("row")[0]);
    let text = opened.bounds(opened.all("text")[0]);
    assert!(last.1 + last.3 > text.1 + text.3, "{last:?} {text:?}");
    assert!(row.1 + row.3 >= last.1 + last.3 - 1., "{row:?} {last:?}");
}

#[test]
fn a_narrow_pane_has_its_threads_in_the_text() {
    let opened = open_at("narrow", 500);
    opened.comment_on("helper", "Which helper?");
    assert!(opened.all("aside").is_empty());
    let (block, card) = (
        opened.bounds(opened.all("block")[0]),
        opened.bounds(opened.all("thread")[0]),
    );
    assert!(card.1 >= block.1 + block.3, "{block:?} {card:?}");
    assert!((card.0 - block.0).abs() < 1., "{block:?} {card:?}");
}

#[test]
fn the_dialog_keeps_a_new_thread_open_and_closes() {
    let opened = open("dialog");
    // The header's button goes to the other layout, and says the way back.
    assert_eq!(
        opened.text_of(opened.all("layout")[0]),
        "Comments in a dialog"
    );
    opened.click(opened.all("layout")[0]);
    opened.until("the other layout", || {
        opened.text_of(opened.all("layout")[0]) == "Comments in the margin"
    });

    // A new comment is written in the dialog, which stays open on the
    // thread it starts; in the page the thread is its head line alone, in
    // the text and not beside it.
    opened.comment_on("helper", "Which helper?");
    opened.until("the dialog", || opened.all("dialog").len() == 1);
    let dialog = opened.all("dialog");
    assert!(opened.text_of(dialog[0]).contains("“helper”"));
    assert!(opened.text_of(dialog[0]).contains("Which helper?"));
    let thread = opened.all("thread")[0];
    assert!(!opened.text_of(thread).contains("Which helper?"));
    let (block, card) = (opened.bounds(opened.all("block")[0]), opened.bounds(thread));
    assert!(card.1 >= block.1 + block.3, "{block:?} {card:?}");

    // A reply goes to the thread, and shows in the dialog.
    opened.commit("And why?");
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the reply", || opened.all("post").len() == 2);
    assert_eq!(opened.threads()[0].replies[0].body.value, "And why?");

    // `Close` leaves nothing open, and the thread's head opens it again.
    opened.click(opened.close_of_the_dialog());
    opened.until("the dialog closed", || opened.all("dialog").is_empty());
    opened.click(opened.all("head")[0]);
    opened.until("the dialog again", || opened.all("dialog").len() == 1);
    opened.click(opened.close_of_the_dialog());
    opened.until("the dialog closed again", || {
        opened.all("dialog").is_empty()
    });

    // Back in the margin, the thread is open there as it was left, and
    // its head shuts it and opens it on its field.
    opened.click(opened.all("layout")[0]);
    opened.until("the first layout", || {
        opened.text_of(opened.all("layout")[0]) == "Comments in a dialog"
    });
    opened.until("the thread open in the margin", || {
        opened.all("aside").len() == 1 && opened.all("post").len() == 2
    });
    assert!(opened.all("dialog").is_empty());
    opened.click(opened.all("head")[0]);
    opened.until("the thread shut", || opened.all("post").is_empty());
    opened.click(opened.all("head")[0]);
    opened.until("the thread open on its field", || {
        opened.all("post").len() == 2 && opened.all("composer").len() == 1
    });
}

#[test]
fn threads_move_between_the_margin_and_the_text_as_the_pane_is_resized() {
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};

    let opened = open("resized");
    opened.comment_on("helper", "Which helper?");
    opened.comment_on("nono", "Why first?");
    let resize = |width: u32| {
        let document = opened.app.document();
        (document.borrow_mut()).set_viewport(Viewport::new(width, 700, 1., ColorScheme::Light));
    };
    // Narrower, wider, and again: the margin is not displayed while there
    // is none, and its threads are made again each time it is.
    for _ in 0..3 {
        resize(500);
        opened.until("no margin", || opened.all("aside").is_empty());
        assert_eq!(opened.all("thread").len(), 2);
        resize(900);
        opened.until("a margin", || opened.all("aside").len() == 2);
        assert_eq!(opened.all("thread").len(), 2);
    }
    // A thread opened and shut in the margin after that.
    opened.click(opened.all("head")[0]);
    opened.click(opened.all("head")[0]);
    assert_eq!(opened.all("thread").len(), 2);
}
