//! The board's list over a project's own store: writes go through a real
//! `horizon-logd`, started for the test with a socket and a data home of
//! its own, and a write made elsewhere reaches the list by the log.
//!
//! The daemon is the root workspace's binary: build it there first
//! (`cargo build --workspace`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use horizon::background::Background;
use horizon::board::list::view;
use horizon::board::{BoardList, Source};
use horizon_board::{Position, Store};
use horizon_board_view::model::ListCommand;
use horizon_theme_css::{Theme, themed};
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::testing::click_at;
use keyboard_types::{Key, Modifiers, NamedKey};

/// The test's directory; the log daemon started on a socket in it is
/// ended with it.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let socket = self.0.join("logd.sock");
        // The daemon is the process started with this socket.
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Ok(command) = std::fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let command = String::from_utf8_lossy(&command);
            if command.contains("horizon-logd") && command.contains(&*socket.to_string_lossy()) {
                let _ = Command::new("kill").arg(entry.file_name()).status();
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

fn all(app: &App, class: &str) -> Vec<NodeId> {
    let document = app.document();
    let document = document.borrow();
    let mut found = Vec::new();
    collect(&document, document.root_element().id, class, &mut found);
    found
}

/// The titles of the rows on the page, in order.
fn titles(app: &App) -> Vec<String> {
    let rows = all(app, "title");
    let document = app.document();
    let document = document.borrow();
    (rows.into_iter())
        .map(|row| document.get_node(row).unwrap().text_content())
        .collect()
}

fn until(app: &App, what: &str, met: impl Fn() -> bool) {
    let started = Instant::now();
    loop {
        app.resolve();
        if met() {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no {what} in {:?}",
            titles(app)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn press(app: &App, key: Key) {
    app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        key,
        code: keyboard_types::Code::Unidentified,
        modifiers: Modifiers::default(),
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
        text: None,
    }));
    app.resolve();
}

#[test]
fn a_task_added_in_the_list_is_written_and_one_written_elsewhere_arrives() {
    let logd = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/horizon-logd");
    assert!(
        logd.is_file(),
        "no {}: run `cargo build --workspace` first",
        logd.display()
    );
    // A short path: a socket's is bounded.
    let scratch = Scratch(std::env::temp_dir().join(format!("hzbb-{}", std::process::id())));
    let project = scratch.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(scratch.0.join("run")).unwrap();
    // SAFETY: nextest runs this test in a process of its own, and nothing
    // else in it has started yet.
    unsafe {
        std::env::set_var("HORIZON_LOGD_BINARY", &logd);
        std::env::set_var("HORIZON_LOGD_SOCKET", scratch.0.join("logd.sock"));
        std::env::set_var("HORIZON_DATA_HOME", scratch.0.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", scratch.0.join("run"));
        // Run from a Git hook, the test inherits the variables that point
        // Git at the repository the hook is for; neither the `git` below
        // nor the board's may follow them.
        for inherited in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_PREFIX",
        ] {
            std::env::remove_var(inherited);
        }
    }
    let made = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&project)
        .status();
    assert!(made.is_ok_and(|status| status.success()), "git init");

    let mut list: Option<BoardList> = None;
    let css = horizon_theme_css::CSS;
    let app = App::new(600, 500, css, || {
        let source = Source::Root(project.clone());
        let (root, state) = view(source, Background::default(), |_| {});
        list = Some(state);
        themed(Theme::built_in, root.focus())
    });
    let list = list.unwrap();
    app.resolve();

    // A title typed into the field and Enter: the task is in the store,
    // and so in the list.
    let (x, y) = {
        let field = all(&app, "add")[0];
        let document = app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(field).expect("a box");
        (
            (bounds.x + bounds.width / 2.) as f32,
            (bounds.y + bounds.height / 2.) as f32,
        )
    };
    click_at(&app, x, y);
    app.resolve();
    app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit(
        "first task".to_string(),
    )));
    app.resolve();
    press(&app, Key::Named(NamedKey::Enter));
    until(&app, "the task added", || titles(&app) == ["first task"]);

    // A task written by something else reaches the list by the log.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let elsewhere = Store::from_dir(&project).unwrap();
    (runtime.block_on(elsewhere.add("second task", "", None, Position::Bottom))).unwrap();
    until(&app, "the task written elsewhere", || {
        titles(&app) == ["first task", "second task"]
    });

    // Moved up among its siblings, it is written where it went.
    list.execute(ListCommand::SelectNext);
    list.execute(ListCommand::MoveUp);
    until(&app, "the task moved", || {
        titles(&app) == ["second task", "first task"]
    });
    let stored = elsewhere.list(None, true).unwrap().items;
    let order: Vec<&str> = (horizon_board::tree_order(&stored, false).into_iter())
        .map(|(item, _)| item.title.as_str())
        .collect();
    assert_eq!(order, ["second task", "first task"]);
}
