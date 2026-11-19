//! The list of the workspace's sessions (`plugins/sessions`), in a pane
//! of the shell: the sessions by project and in the order their owner
//! would look at them, the keys that move the choice and search, and what
//! the list has the shell do to the chosen one.
//!
//! The extension is the one cargo builds
//! (`./scripts/extensions.sh build`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horizon::workspace::{Opening, Shell, Spawn, css, view};
use horizon_control::contract::{EnvelopeBody, Invoke};
use horizon_control::host::executor::ControlRequest;
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_workspace::types::{AgentStatus, Doing, ProgramState, ProgramStatus};
use horizon_workspace::{SessionId, SessionKind, Workspace};
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

struct Opened {
    app: App,
    shell: Shell,
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

/// A shell with the extensions it comes with.
fn open() -> Opened {
    open_with(Opening::default())
}

fn open_with(opening: Opening) -> Opened {
    let extensions =
        horizon::extension::own_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug"));
    let mut shell = None;
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                extensions,
                ..opening
            },
        );
        shell = Some(built);
        root
    });
    app.resolve();
    let opened = Opened {
        app,
        shell: shell.unwrap(),
    };
    opened.until("the extensions", || opened.shell.extensions_read());
    opened
}

impl Opened {
    fn model<R>(&self, read: impl FnOnce(&Workspace) -> R) -> R {
        self.shell.model.with_untracked(read)
    }

    /// The text of each element of a class, in document order.
    fn texts(&self, class: &str) -> Vec<String> {
        let document = self.app.document();
        let document = document.borrow();
        // (Within the list: the shell has classes of the same names.)
        let mut lists = Vec::new();
        collect(
            &document,
            document.root_element().id,
            "sessions",
            &mut lists,
        );
        let mut found = Vec::new();
        for list in lists {
            collect(&document, list, class, &mut found);
        }
        (found.into_iter())
            .map(|node| document.get_node(node).unwrap().text_content())
            .collect()
    }

    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        while !met() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no {what} in:\n{}",
                horizon_ui::testing::text(&self.app)
            );
            std::thread::sleep(Duration::from_millis(5));
            self.app.run_effects();
            self.app.resolve();
        }
    }

    fn key(&self, key: Key) {
        let text = match &key {
            Key::Character(text) => Some(text.as_str().into()),
            _ => None,
        };
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            text,
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers: Modifiers::default(),
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
        }));
        self.app.run_effects();
        self.app.resolve();
    }

    fn character(&self, character: &str) {
        self.key(Key::Character(character.into()));
    }

    /// Text as an input method commits it into the focused field.
    fn commit(&self, text: &str) {
        self.app
            .dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        self.app.run_effects();
        self.app.resolve();
    }

    /// A pane of the list, with the keys.
    fn list(&self) {
        let answer = self.shell.control(&ControlRequest::Invoke(Invoke {
            command: "new".to_string(),
            args: serde_json::json!({ "plugin": "sessions", "directory": "/", "activate": true }),
        }));
        std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
        self.app.run_effects();
        self.app.resolve();
    }

    /// The list over the workspace, as its extension's command puts it
    /// there, with the keys.
    fn manage(&self) {
        let answer = self.shell.control(&ControlRequest::Invoke(Invoke {
            command: "plugin-command".to_string(),
            args: serde_json::json!({ "name": "manage-sessions" }),
        }));
        std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
        self.app.run_effects();
        self.app.resolve();
        assert!(self.managed(), "{:?}", self.shell.modal());
    }

    /// Whether an extension's view is over the workspace.
    fn managed(&self) -> bool {
        matches!(
            self.shell.modal(),
            Some(horizon::workspace::Modal::Extension(_))
        )
    }

    /// The middle of the box the pane `pane` is drawn in.
    fn middle_of(&self, pane: horizon_workspace::PaneId) -> (f32, f32) {
        let element = self.shell.pane_element(pane).expect("the pane is drawn");
        let document = self.app.document();
        let document = document.borrow();
        let bounds = (document.get_client_bounding_rect(element)).expect("a box");
        (
            (bounds.x + bounds.width / 2.) as f32,
            (bounds.y + bounds.height / 2.) as f32,
        )
    }

    /// The elements of a class within the list, in document order.
    fn nodes(&self, class: &str) -> Vec<NodeId> {
        let document = self.app.document();
        let document = document.borrow();
        let mut lists = Vec::new();
        collect(
            &document,
            document.root_element().id,
            "sessions",
            &mut lists,
        );
        let mut found = Vec::new();
        for list in lists {
            collect(&document, list, class, &mut found);
        }
        found
    }

    /// The middle of an element of the page.
    fn middle(&self, node: NodeId) -> (f32, f32) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = (document.get_client_bounding_rect(node)).expect("a box");
        (
            (bounds.x + bounds.width / 2.) as f32,
            (bounds.y + bounds.height / 2.) as f32,
        )
    }

    /// A click, or a right-click, on the middle of the element of the
    /// list's class `class` whose text begins with `text`.
    fn press(&self, class: &str, text: &str, right: bool) {
        let texts = self.texts(class);
        let at = (texts.iter().position(|said| said.starts_with(text)))
            .unwrap_or_else(|| panic!("no {class} {text:?} in {texts:?}"));
        let (x, y) = self.middle(self.nodes(class)[at]);
        match right {
            true => horizon_ui::testing::right_click_at(&self.app, x, y),
            false => horizon_ui::testing::click_at(&self.app, x, y),
        }
        self.app.run_effects();
        self.app.resolve();
        self.app.run_effects();
        self.app.resolve();
    }

    /// [`press`](Self::press) as a window has it: the page is drawn
    /// between the pointer coming over the element, its button going
    /// down, and its button coming up.
    fn press_over_frames(&self, class: &str, text: &str, right: bool) {
        use horizon_ui::testing::{Pointer, pointer_with};
        let texts = self.texts(class);
        let at = (texts.iter().position(|said| said.starts_with(text)))
            .unwrap_or_else(|| panic!("no {class} {text:?} in {texts:?}"));
        let (x, y) = self.middle(self.nodes(class)[at]);
        for step in [Pointer::Move, Pointer::Down, Pointer::Up] {
            pointer_with(&self.app, x, y, step, right);
            for _ in 0..2 {
                self.app.run_effects();
                self.app.resolve();
            }
        }
    }

    /// The titles of the rows, in the order they are drawn.
    fn titles(&self) -> Vec<String> {
        self.texts("title")
    }

    /// The chosen row's title.
    fn chosen(&self) -> Option<String> {
        let chosen = self.texts("selected").into_iter().next()?;
        self.titles()
            .into_iter()
            .find(|title| chosen.starts_with(title.as_str()))
    }
}

/// A repository and a directory outside one, to work in.
struct Places {
    base: PathBuf,
    letters: PathBuf,
    notes: PathBuf,
}

impl Places {
    fn made(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("horizon-sessions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (letters, notes) = (base.join("letters"), base.join("notes"));
        std::fs::create_dir_all(letters.join(".git")).unwrap();
        std::fs::create_dir_all(&notes).unwrap();
        Self {
            base,
            letters,
            notes,
        }
    }
}

impl Drop for Places {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The sessions a test lists, beside the shell's first terminal: an
/// agent that waits to be allowed something, one derived from it at work,
/// a program that finished unseen, and an agent at rest elsewhere.
struct Made {
    first: SessionId,
    waiting: SessionId,
    derived: SessionId,
    finished: SessionId,
}

fn make(opened: &Opened, places: &Places) -> Made {
    let first = opened.model(|model| model.active_terminal_session_id().unwrap());
    let (waiting, derived) = (SessionId::new(), SessionId::new());
    let (finished, resting) = (SessionId::new(), SessionId::new());
    let (letters, notes) = (places.letters.clone(), places.notes.clone());
    let now = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
        .map_or(0, |since| since.as_millis() as u64);
    opened.shell.model.update(move |model| {
        model.set_session_derived_title(first, Some("shell"));
        model.set_session_directory(first, letters.clone());
        let mut add =
            |id, kind, title: &str, directory: &Path, doing, detail: Option<&str>, at: u64| {
                model.register_detached_session(kind, id);
                model.set_session_derived_title(id, Some(title));
                model.set_session_directory(id, directory.to_path_buf());
                // (`at` minutes after eight hours ago.)
                let at = now.saturating_sub(8 * 60 * 60_000) + at * 60_000;
                model.set_session_activity(id, Some(doing), detail.map(str::to_string), at);
            };
        let agent = |status| Doing::Agent(status);
        add(
            waiting,
            SessionKind::Agent,
            "build",
            &letters,
            agent(AgentStatus::WaitingForApproval),
            Some("Run bash"),
            100,
        );
        add(
            derived,
            SessionKind::Agent,
            "tests",
            &letters,
            agent(AgentStatus::Running),
            None,
            200,
        );
        let done = Doing::Program(ProgramStatus {
            state: ProgramState::Done,
            blocked_on: None,
            app: Some("codex".to_string()),
        });
        add(
            finished,
            SessionKind::Terminal,
            "measure",
            &letters,
            done,
            None,
            300,
        );
        add(
            resting,
            SessionKind::Agent,
            "wiki",
            &notes,
            agent(AgentStatus::Completed),
            Some("Updated two pages."),
            400,
        );
        model.set_session_parent(derived, waiting);
        model.mark_session_seen(resting);
    });
    opened.app.run_effects();
    opened.app.resolve();
    Made {
        first,
        waiting,
        derived,
        finished,
    }
}

#[test]
fn the_sessions_are_listed_by_project_with_what_waits_first() {
    let opened = open();
    let places = Places::made("listed");
    let made = make(&opened, &places);
    opened.list();
    opened.until("the rows", || opened.titles().len() == 5);

    // A project's sessions under its name: what waits on its owner, with
    // what was derived from it under it, then what finished unseen, then
    // what rests. Another project's after them.
    assert_eq!(
        opened.titles(),
        ["build", "tests", "measure", "shell", "wiki"]
    );
    let bands = opened.texts("band");
    assert!(
        bands[0].starts_with("letters") && bands[0].ends_with('4'),
        "{bands:?}"
    );
    assert!(bands[1].ends_with("notes1"), "{bands:?}");
    // Each row says what its session is and does, and its state.
    let rows = opened.texts("label");
    assert!(
        rows[0].starts_with("buildagentapproval") && rows[0].ends_with("Run bash"),
        "{rows:?}"
    );
    assert!(rows[1].contains("working"), "{rows:?}");
    assert!(
        rows[2].contains("codex") && rows[2].contains("unseen"),
        "{rows:?}"
    );
    assert!(
        rows[4].starts_with("wikiagent") && rows[4].ends_with("Updated two pages."),
        "{rows:?}"
    );
    // (A terminal whose program says nothing has no state to be in since;
    // the line for its words is there, empty.)
    assert_eq!(rows[3].trim_end_matches('\u{a0}'), "shellterminal");
    // A row is as tall whatever its session is doing or says.
    let heights = |opened: &Opened| -> Vec<i64> {
        let document = opened.app.document();
        let document = document.borrow();
        (opened.nodes("row").into_iter())
            .map(|row| {
                document
                    .get_client_bounding_rect(row)
                    .expect("a box")
                    .height
                    .round() as i64
            })
            .collect()
    };
    let tall = heights(&opened);
    assert!(tall.iter().all(|height| *height == tall[0]), "{tall:?}");

    // The keys: a step, and the next that waits, round the list.
    assert_eq!(opened.chosen().as_deref(), Some("build"));
    opened.character("j");
    assert_eq!(opened.chosen().as_deref(), Some("tests"));
    opened.character("n");
    assert_eq!(opened.chosen().as_deref(), Some("measure"));
    opened.character("n");
    assert_eq!(opened.chosen().as_deref(), Some("build"));
    opened.key(Key::Named(NamedKey::ArrowUp));
    assert_eq!(opened.chosen().as_deref(), Some("build"));

    // A search is typed after `/`, and finds by what a row says; a session
    // found stays under what it was derived from. Escape takes it back.
    opened.character("/");
    opened.until("the field", || !opened.texts("search").is_empty());
    opened.commit("codex");
    opened.until("the one found", || opened.titles() == ["measure"]);
    opened.key(Key::Named(NamedKey::Escape));
    opened.until("all of them", || opened.titles().len() == 5);
    opened.character("/");
    opened.commit("working");
    opened.until("the one found", || opened.titles() == ["build", "tests"]);
    opened.key(Key::Named(NamedKey::Enter));
    assert_eq!(opened.titles(), ["build", "tests"]);
    opened.key(Key::Named(NamedKey::Escape));
    opened.until("all of them", || opened.titles().len() == 5);

    // The model changes: the list follows it. Looked at, what finished
    // is at rest, and goes below what still works.
    opened
        .shell
        .model
        .update(|model| _ = model.mark_session_seen(made.finished));
    opened.until("the order", || {
        opened.titles() == ["build", "tests", "measure", "shell", "wiki"]
            && !opened.texts("label")[2].contains("unseen")
    });
    let _ = made.first;
}

#[test]
fn the_chosen_session_is_ended_once_the_list_is_told_so() {
    let opened = open();
    let places = Places::made("ended");
    let made = make(&opened, &places);
    opened.list();
    opened.until("the rows", || opened.titles().len() == 5);
    let has = |session: SessionId| opened.model(|model| model.session_kind(session).is_some());
    let foot = || opened.texts("foot").join(" ");

    // With what was derived from it: asked, and kept on any other key.
    assert_eq!(opened.chosen().as_deref(), Some("build"));
    opened.character("X");
    assert!(
        foot().contains("Terminate “build” and the 1 derived from it?"),
        "{}",
        foot()
    );
    opened.key(Key::Named(NamedKey::Escape));
    assert!(foot().contains("Go to"), "{}", foot());
    assert!(has(made.waiting) && has(made.derived));

    // One session: asked, and ended on `y`. The choice goes to what took
    // its place.
    opened.character("j");
    opened.character("x");
    assert!(
        foot().contains("Terminate “tests”? It ends what runs in it."),
        "{}",
        foot()
    );
    opened.character("y");
    opened.until("the session ended", || !has(made.derived));
    opened.until("the rows", || {
        opened.titles() == ["build", "measure", "shell", "wiki"]
    });
    assert_eq!(opened.chosen().as_deref(), Some("measure"));
    assert!(has(made.waiting));

    // With nothing derived from it, `X` ends the one.
    opened.character("k");
    opened.character("X");
    assert!(
        foot().contains("Terminate “build”? It ends what runs in it."),
        "{}",
        foot()
    );
    opened.character("y");
    opened.until("the session ended", || !has(made.waiting));
}

#[test]
fn the_chosen_session_is_gone_to_and_a_terminal_opened_where_it_works() {
    let opened = open();
    let places = Places::made("shown");
    let made = make(&opened, &places);
    opened.list();
    opened.until("the rows", || opened.titles().len() == 5);
    let shown = |session: SessionId| {
        opened.model(|model| model.pane_location_for_session(session).is_some())
    };

    // `t`: a terminal in a tab of its own.
    let tabs = opened.model(Workspace::tab_count);
    opened.character("t");
    opened.until("the terminal", || {
        opened.model(Workspace::tab_count) == tabs + 1
    });

    // Its tab closed, the keys are the list's again: Enter on a session
    // no pane shows puts it in a pane beside the list's, with the keys.
    opened
        .shell
        .execute(horizon_workspace::commands::CommandId::CloseActiveTab);
    opened.app.run_effects();
    opened.app.resolve();
    opened.until("the terminal's row", || opened.titles().len() == 6);
    assert!(!shown(made.finished));
    opened.character("n");
    assert_eq!(opened.chosen().as_deref(), Some("measure"));
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the session's pane", || shown(made.finished));
    assert_eq!(
        opened.model(Workspace::active_session_id),
        Some(made.finished)
    );
    // The list is still there beside it.
    assert_eq!(opened.titles().len(), 6);
}

#[test]
fn what_the_keys_do_the_pointer_does_on_the_rows_their_menus_and_the_band() {
    let opened = open();
    let places = Places::made("pointer");
    let made = make(&opened, &places);
    opened.list();
    opened.until("the rows", || opened.titles().len() == 5);
    let has = |session: SessionId| opened.model(|model| model.session_kind(session).is_some());
    let foot = || opened.texts("foot").join(" ");

    // A right-click on a row chooses it and opens its menu: what can be
    // done to it, each with its key. "With derived" is offered where some
    // are derived from it.
    opened.press("title", "tests", true);
    assert_eq!(opened.chosen().as_deref(), Some("tests"));
    assert_eq!(
        opened.texts("item"),
        ["Go toEnter", "Open a terminal theret", "Terminatex"]
    );
    opened.press("title", "build", true);
    assert_eq!(opened.chosen().as_deref(), Some("build"));
    assert_eq!(opened.texts("item").len(), 4);

    // An item does what its key does: asked, and kept from the band.
    opened.press("item", "Terminate with derived", false);
    assert!(opened.texts("item").is_empty());
    assert!(
        foot().contains("Terminate “build” and the 1 derived from it?"),
        "{}",
        foot()
    );
    opened.press("hint", "EscKeep", false);
    assert!(foot().contains("Go to"), "{}", foot());
    assert!(has(made.waiting) && has(made.derived));

    // A row's `⋯` opens the same menu; ended from the band.
    opened.press("title", "tests", true);
    opened.key(Key::Named(NamedKey::Escape));
    assert!(opened.texts("item").is_empty());
    let triggers = opened.nodes("trigger");
    let (x, y) = opened.middle(triggers[1]);
    horizon_ui::testing::click_at(&opened.app, x, y);
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(opened.texts("item").len(), 3);
    opened.press("item", "Terminate", false);
    opened.press("hint", "yTerminate", false);
    opened.until("the session ended", || !has(made.derived));
    opened.until("the rows", || opened.titles().len() == 4);

    // The band's keys are pressed too: the next that waits, round the
    // list from what took the ended one's place.
    assert_eq!(opened.chosen().as_deref(), Some("measure"));
    opened.press("hint", "nNext waiting", false);
    assert_eq!(opened.chosen().as_deref(), Some("build"));

    // A click on a row goes to its session.
    assert!(!opened.model(|model| model.pane_location_for_session(made.finished).is_some()));
    opened.press("title", "measure", false);
    opened.until("the session's pane", || {
        opened.model(Workspace::active_session_id) == Some(made.finished)
    });
}

#[test]
fn a_session_is_ended_with_the_pointer_as_a_window_has_it() {
    let opened = open();
    let places = Places::made("frames");
    let made = make(&opened, &places);
    opened.list();
    opened.until("the rows", || opened.titles().len() == 5);
    let has = |session: SessionId| opened.model(|model| model.session_kind(session).is_some());
    let foot = || opened.texts("foot").join(" ");

    // (The page drawn between a button going down and coming up, the menu
    // is still there for the click, and the row under it hears none.)
    opened.press_over_frames("title", "tests", true);
    assert_eq!(opened.texts("item").len(), 3);
    opened.press_over_frames("item", "Terminate", false);
    assert!(foot().contains("Terminate “tests”?"), "{}", foot());
    opened.press_over_frames("hint", "yTerminate", false);
    opened.until("the session ended", || !has(made.derived));
}

#[test]
fn the_session_manager_is_the_list_over_the_workspace() {
    let opened = open();
    let places = Places::made("managed");
    let made = make(&opened, &places);
    let manage = || {
        opened.manage();
        opened.until("the rows", || opened.titles().len() >= 4);
        // (Laid out, it has the keys.)
        opened.app.run_effects();
        opened.app.resolve();
    };
    let left = || {
        opened.until("the list left", || {
            !opened.managed() && opened.titles().is_empty()
        })
    };
    let has = |session: SessionId| opened.model(|model| model.session_kind(session).is_some());
    let panes = || opened.model(|model| model.all_pane_ids().len());

    // The list, with the keys, and what leaves it in its band. A session
    // ended, the list stays.
    manage();
    assert_eq!(
        opened.titles(),
        ["build", "tests", "measure", "shell", "wiki"]
    );
    let foot = opened.texts("foot").join(" ");
    assert!(foot.ends_with("EscClose"), "{foot}");
    opened.character("j");
    opened.character("x");
    opened.character("y");
    opened.until("the session ended", || !has(made.derived));
    assert!(opened.managed());
    opened.until("the rows", || opened.titles().len() == 4);

    // Gone to, a session no pane shows is in a pane beside the active
    // one, and the list is left: what carried the command out with it.
    assert_eq!(panes(), 1);
    opened.character("n");
    opened.character("n");
    assert_eq!(opened.chosen().as_deref(), Some("measure"));
    opened.key(Key::Named(NamedKey::Enter));
    left();
    assert_eq!(panes(), 2);
    assert_eq!(
        opened.model(Workspace::active_session_id),
        Some(made.finished)
    );
    opened.until("let go of", || opened.shell.commanded() == 0);

    // Escape leaves it, a search under way or not: the keys go back to
    // the pane that had them.
    manage();
    opened.character("/");
    opened.until("the field", || !opened.texts("search").is_empty());
    opened.commit("build");
    opened.until("the one found", || opened.titles() == ["build"]);
    opened.key(Key::Named(NamedKey::Escape));
    left();
    assert_eq!(
        opened.model(Workspace::active_session_id),
        Some(made.finished)
    );
    // (Asked for again, it is the list as it is first shown.)
    manage();
    assert_eq!(opened.titles().len(), 4);
    assert!(opened.texts("search").is_empty());
    // Escape with a question asked answers the question.
    opened.character("x");
    opened.key(Key::Named(NamedKey::Escape));
    assert!(opened.managed());
    opened.key(Key::Named(NamedKey::Escape));
    left();

    // A terminal opened where a session works leaves it too.
    manage();
    let tabs = opened.model(Workspace::tab_count);
    opened.character("t");
    left();
    assert_eq!(opened.model(Workspace::tab_count), tabs + 1);
    let _ = made.first;
}

/// A configuration with the list of sessions kept on the left, put in at
/// startup, and a layout of one view and no place for the workspace.
fn with_layouts() -> Opening {
    let config = horizon_config::parse(
        r#"
        startup_layout = "sidebar"

        [layouts.sidebar]
        split = "horizontal"
        children = [
          { weight = 1, view = "sessions" },
          { weight = 3, workspace = true },
        ]

        [layouts.alone]
        view = "sessions"

        [layouts.wrong]
        view = "terminal"
        "#,
    )
    .expect("a configuration");
    Opening {
        config,
        ..Opening::default()
    }
}

/// The panes showing the list of sessions.
fn lists(opened: &Opened) -> Vec<horizon_workspace::PaneId> {
    opened.model(|model| {
        (model.all_pane_ids().into_iter())
            .filter(|pane| {
                (model.pane_plugin(*pane)).is_some_and(|plugin| plugin.name == "sessions")
            })
            .collect()
    })
}

#[test]
fn the_startup_layout_puts_the_list_left_of_the_workspace_once_it_is_read() {
    let opened = open_with(with_layouts());
    let terminal = opened.model(|model| model.cursor_pane_id().expect("a pane"));
    opened.until("the list", || lists(&opened).len() == 1);
    opened.app.run_effects();
    opened.app.resolve();
    let list = lists(&opened)[0];

    // The list on the left, the workspace as it was on its right, the keys
    // where they were.
    let (first, weights) = opened.model(|model| match model.layout() {
        horizon_workspace::types::LayoutNode::Split { children, .. } => (
            children[0].node.first_key(),
            children
                .iter()
                .map(|child| child.weight)
                .collect::<Vec<_>>(),
        ),
        _ => panic!("expected the list beside the workspace"),
    });
    assert_eq!(first, horizon_workspace::types::NodeKey::Pane(list));
    assert_eq!(weights, vec![1.0, 3.0]);
    assert!(opened.model(|model| model.is_active_pane(terminal)));
    opened.until("the rows", || !opened.titles().is_empty());

    // Put in again from the palette: nothing changes, and no second list.
    let apply = |name: &str| {
        opened
            .shell
            .execute(horizon_workspace::commands::CommandId::ApplyLayout);
        opened.app.run_effects();
        opened.app.resolve();
        opened.commit(name);
        opened.key(Key::Named(NamedKey::Enter));
        opened.app.run_effects();
        opened.app.resolve();
    };
    apply("sidebar");
    assert_eq!(lists(&opened), vec![list]);
    assert!(opened.model(|model| model.is_active_pane(terminal)));

    // One that is not well formed for a layout changes nothing.
    apply("wrong");
    assert_eq!(lists(&opened), vec![list]);
    assert!(opened.model(|model| model.is_active_pane(terminal)));

    // One with no place for the workspace closes its panes; their sessions
    // stay, detached.
    let sessions = opened.model(|model| model.session_summaries().len());
    apply("alone");
    assert_eq!(
        opened.model(|model| model.all_pane_ids()),
        std::collections::HashSet::from([list])
    );
    assert_eq!(
        opened.model(|model| model.session_summaries().len()),
        sessions
    );
    assert_eq!(opened.model(Workspace::detached_session_count), 1);
}

/// Paints the list in a pane, in this machine's theme, to the file
/// `HORIZON_SESSIONS_OUT` names (a PPM): a picture for a person to look
/// at. With `HORIZON_SESSIONS_ASK` set, as it asks before it ends a
/// session; with `HORIZON_SESSIONS_MODAL`, as the session manager shows
/// it. Run by hand.
#[test]
#[ignore = "writes a picture for a person: set HORIZON_SESSIONS_OUT"]
fn paint_the_list() {
    use anyrender::ImageRenderer as _;

    let out = std::env::var("HORIZON_SESSIONS_OUT").expect("HORIZON_SESSIONS_OUT");
    let opened = open_with(Opening {
        config: horizon_config::load().clone(),
        ..Opening::default()
    });
    let places = Places::made("painted");
    make(&opened, &places);
    match std::env::var_os("HORIZON_SESSIONS_MODAL") {
        Some(_) => opened.manage(),
        None => opened.list(),
    }
    opened.app.run_effects();
    opened.app.resolve();
    opened.until("the rows", || opened.titles().len() == 5);
    if std::env::var_os("HORIZON_SESSIONS_ASK").is_some() {
        opened.character("X");
    }
    menu_if_asked(&opened);
    let (width, height) = (1200u32, 800u32);
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    let mut picture = format!("P6\n{width} {height}\n255\n").into_bytes();
    for [red, green, blue, _] in pixels.as_chunks::<4>().0 {
        picture.extend_from_slice(&[*red, *green, *blue]);
    }
    std::fs::write(out, picture).expect("the picture written");
}

/// For a picture: the menu of the row `HORIZON_SESSIONS_MENU` names
/// opened by a right-click, where it names one.
fn menu_if_asked(opened: &Opened) {
    if let Ok(title) = std::env::var("HORIZON_SESSIONS_MENU") {
        opened.press("title", &title, true);
        opened.until("the menu", || !opened.texts("item").is_empty());
        if let Ok(item) = std::env::var("HORIZON_SESSIONS_ITEM") {
            opened.press_over_frames("item", &item, false);
        }
    }
}

#[test]
fn the_list_left_of_the_tab_group_is_not_cut_by_what_it_opens() {
    use horizon_workspace::{PluginPane, Side};

    let opened = open();
    let places = Places::made("left");
    let made = make(&opened, &places);
    // The list on the left of the group, put there from the group's strip.
    opened.shell.model.update(|model| {
        model.enter_workspace_mode();
        model.move_cursor(horizon_workspace::Direction::Up);
    });
    opened.shell.create_plugin(
        PluginPane {
            name: "sessions".to_string(),
            title: "Sessions".to_string(),
            state: serde_json::Value::Null,
        },
        horizon::workspace::Placement::Split(Side::Left),
    );
    opened.app.run_effects();
    opened.app.resolve();
    opened.until("the rows", || opened.titles().len() == 5);
    let list = opened.model(|model| model.cursor_pane_id().expect("the list has the keys"));
    // (Laid out, the list has the keys.)
    opened.app.run_effects();
    opened.app.resolve();
    let beside = |opened: &Opened| {
        opened.model(|model| match model.layout() {
            horizon_workspace::types::LayoutNode::Split { children, .. } => children.len(),
            _ => 0,
        })
    };
    assert_eq!(beside(&opened), 2);

    // Enter on a session no pane shows: its pane is in the group, and the
    // list keeps its place and its width.
    opened.character("n");
    assert_eq!(opened.chosen().as_deref(), Some("measure"));
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the session's pane", || {
        opened.model(|model| model.pane_location_for_session(made.finished).is_some())
    });
    assert_eq!(beside(&opened), 2);
    let in_group = opened.model(|model| {
        let pane = model
            .pane_location_for_session(made.finished)
            .expect("a pane");
        model.active_tab().expect("a tab").root.first_pane() != Some(list)
            && model.tab_panes(pane).contains(&pane)
    });
    assert!(in_group);

    // `t` from the list: a terminal in a tab of the group, not a group
    // made of the list.
    let (x, y) = opened.middle_of(list);
    horizon_ui::testing::click_at(&opened.app, x, y);
    opened.app.run_effects();
    opened.app.resolve();
    assert!(opened.model(|model| model.is_active_pane(list)));
    let tabs = opened.model(Workspace::tab_count);
    opened.character("t");
    opened.until("the terminal", || {
        opened.model(Workspace::tab_count) == tabs + 1
    });
    assert_eq!(opened.model(|model| model.tab_groups().len()), 1);
    assert_eq!(beside(&opened), 2);
}

/// Paints the workspace put in a layout with the list of sessions on the
/// left at startup, in this machine's theme and font, to the file
/// `HORIZON_SIDEBAR_OUT` names (a PPM): a picture for a person to look at.
/// Run by hand.
#[test]
#[ignore = "writes a picture for a person: set HORIZON_SIDEBAR_OUT"]
fn paint_the_sidebar() {
    use anyrender::ImageRenderer as _;

    let out = std::env::var("HORIZON_SIDEBAR_OUT").expect("HORIZON_SIDEBAR_OUT");
    let mine = horizon_config::load();
    let mut opening = with_layouts();
    opening.config.theme = mine.theme.clone();
    opening.config.ui = mine.ui.clone();
    opening.config.terminal = mine.terminal.clone();
    let opened = open_with(opening);
    let places = Places::made("sidebar");
    make(&opened, &places);
    opened.until("the list", || lists(&opened).len() == 1);
    opened.until("the rows", || opened.titles().len() == 5);
    menu_if_asked(&opened);
    let (width, height) = (1200u32, 800u32);
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    let mut picture = format!("P6\n{width} {height}\n255\n").into_bytes();
    for [red, green, blue, _] in pixels.as_chunks::<4>().0 {
        picture.extend_from_slice(&[*red, *green, *blue]);
    }
    std::fs::write(out, picture).expect("the picture written");
}
