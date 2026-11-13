//! The shell's agent pane over a real `horizon-agentd`, started for the
//! test with sockets, a data home and a configuration of its own, and a
//! provider that is a local stand-in (`scripts/mock-openai-provider.py`):
//! a message typed into the composer is answered in the pane.
//!
//! The daemons are the root workspace's binaries: build them there first
//! (`cargo build --workspace`).

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use horizon::agentd::{Agentd, RuntimeModels};
use horizon::workspace::{self, Modal, Opening};
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::{GetUntracked, WithUntracked};
use horizon_workspace::Workspace;
use horizon_workspace::commands::CommandId;
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

/// The stand-in provider, ended with the test.
struct Provider(Child);

impl Drop for Provider {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What Git sets for a hook to say which repository it runs for.
const GIT_LOCATION: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
];

/// The test's directory; the log daemon the board started on a socket in
/// it is ended with it.
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

fn press(app: &App, key: Key, modifiers: Modifiers) {
    app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        key,
        code: keyboard_types::Code::Unidentified,
        modifiers,
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
        text: None,
    }));
    app.resolve();
}

#[test]
fn a_message_typed_into_an_agent_pane_is_answered_by_a_real_agentd() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    let binary = |name: &str| {
        let binary = root.join("target/debug").join(name);
        assert!(
            binary.is_file(),
            "no {}: run `cargo build --workspace` first",
            binary.display()
        );
        binary
    };
    // A short path: a socket's is bounded.
    let scratch = Scratch(std::env::temp_dir().join(format!("hzba-{}", std::process::id())));
    let directory = scratch.0.clone();
    std::fs::create_dir_all(directory.join("project")).unwrap();
    // (A board's organizer works in a Git repository.) Run from a Git hook,
    // the test inherits the variables that point Git at the repository the
    // hook is for; neither this `git` nor the daemon's may follow them.
    let mut init = Command::new("git");
    for inherited in GIT_LOCATION {
        init.env_remove(inherited);
    }
    let made = init
        .args(["init", "--quiet"])
        .current_dir(directory.join("project"))
        .status()
        .expect("git");
    assert!(made.success());

    // A port nothing listens on, for the provider.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let config = directory.join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"default_model = {{ provider = "mock", model = "mock-model" }}
small_model = {{ provider = "mock", model = "mock-model" }}

[[providers]]
name = "mock"
kind = "openai-compatible"
base_url = "http://127.0.0.1:{port}/v1"
api_key_env = "HORIZON_TEST_MOCK_KEY"
"#
        ),
    )
    .unwrap();
    let _provider = Provider(
        Command::new("python3")
            .arg(root.join("scripts/mock-openai-provider.py"))
            .arg(port.to_string())
            .arg(directory.join("mock.log"))
            .env("HORIZON_ACP_CHECK_REPLY_DELAY", "0")
            .spawn()
            .expect("python3, for the stand-in provider"),
    );
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no provider on {port}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // What the daemons inherit: everything they keep or reach is under the
    // test's own directory.
    // SAFETY: nextest runs this test in a process of its own, and nothing
    // else in it has started yet.
    unsafe {
        std::env::set_var("HORIZON_AGENTD_BINARY", binary("horizon-agentd"));
        std::env::set_var("HORIZON_LOGD_BINARY", binary("horizon-logd"));
        std::env::set_var("HORIZON_CONFIG", &config);
        std::env::set_var("HORIZON_TEST_MOCK_KEY", "mock");
        std::env::set_var("HORIZON_DATA_HOME", directory.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", directory.join("run"));
        std::env::set_var("HORIZON_LOGD_SOCKET", directory.join("logd.sock"));
        // The model catalog is not fetched from the network: the stand-in
        // has none, and the copy kept is the test's own.
        std::env::set_var(
            "HORIZON_MODEL_CATALOG_URL",
            format!("http://127.0.0.1:{port}/model-catalog.json"),
        );
        std::env::set_var(
            "HORIZON_MODEL_CATALOG_CACHE",
            directory.join("model-catalog.json"),
        );
        for unset in [
            "OPENAI_API_KEY",
            "HORIZON_AGENT_EVENT_LOG",
            "HORIZON_AGENT_STATE_DB",
            "HORIZON_SOCKET",
            "HORIZON_SESSION_ID",
        ] {
            std::env::remove_var(unset);
        }
        for inherited in GIT_LOCATION {
            std::env::remove_var(inherited);
        }
        std::env::set_current_dir(directory.join("project")).unwrap();
    }
    std::fs::create_dir_all(directory.join("run")).unwrap();

    let agentd = Agentd::start(
        &directory.join("agentd.sock"),
        &directory.join("control.sock"),
    );
    let mut shell = None;
    let app = App::new(1200, 800, workspace::css(), || {
        let spawn: workspace::Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let opening = Opening {
            agents: Some(agentd.spawn()),
            models: Some(std::sync::Arc::new(RuntimeModels(agentd.clone()))),
            agent_runtime: Some(agentd.runtime()),
            ..Opening::default()
        };
        let (root, built) = workspace::view(spawn, opening);
        shell = Some(built);
        root
    });
    let shell = shell.unwrap();
    let text = || {
        let document = app.document();
        let document = document.borrow();
        document.root_element().text_content()
    };
    let until = |what: &str, met: &dyn Fn() -> bool| {
        let started = Instant::now();
        while !met() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "no {what} in:\n{}",
                text()
            );
            std::thread::sleep(Duration::from_millis(10));
            agentd.answer(std::task::Waker::noop(), &shell);
            app.resolve();
        }
    };
    app.resolve();

    // Workspace mode's `a`: an agent in a tab of its own, with the keys.
    press(&app, Key::Character("'".into()), Modifiers::CONTROL);
    press(&app, Key::Character("a".into()), Modifiers::default());
    assert_eq!(shell.model.with_untracked(Workspace::tab_count), 2);
    let pane = shell
        .model
        .with_untracked(|model| model.visible_panes()[0].id);
    assert!(shell.agent(pane).is_some());

    // What is typed is sent with Enter, shown, and answered.
    app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit(
        "hello there".to_string(),
    )));
    app.resolve();
    press(&app, Key::Named(NamedKey::Enter), Modifiers::default());
    until("the message", &|| text().contains("hello there"));
    until("the answer", &|| text().contains("ACP-OK"));
    // The tab is named for what the session is about: the message, and
    // then the small model's summary of it (the stand-in's one reply).
    let title = || {
        shell
            .model
            .with_untracked(|model| model.tab_summaries()[1].title.clone())
    };
    until("a title", &|| {
        ["hello there", "ACP-OK"].contains(&title().as_str())
    });

    // The model picker: the daemon's providers, then the chosen one's
    // models, and the choice goes to the daemon.
    let commit = |text: &str| {
        app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        app.resolve();
    };
    let enter = || press(&app, Key::Named(NamedKey::Enter), Modifiers::default());
    press(&app, Key::Character("'".into()), Modifiers::CONTROL);
    press(&app, Key::Character(":".into()), Modifiers::default());
    commit("switch model");
    enter();
    assert!(matches!(shell.modal(), Some(Modal::Models(_))));
    let rows = || {
        let document = app.document();
        let document = document.borrow();
        let mut names = Vec::new();
        let mut pending = vec![document.root_element().id];
        while let Some(id) = pending.pop() {
            let node = document.get_node(id).unwrap();
            let class = node.attr(horizon_ui::blitz_dom::local_name!("class"));
            if class.is_some_and(|classes| horizon_ui::testing::has_class(classes, "name")) {
                names.push(node.text_content());
            }
            pending.extend(node.children.iter().rev().copied());
        }
        names
    };
    until("the providers", &|| rows() == ["mock"]);
    enter();
    until("the provider's models", &|| rows() == ["mock-model"]);
    // Escape goes back a step before it leaves.
    press(&app, Key::Named(NamedKey::Escape), Modifiers::default());
    assert_eq!(rows(), ["mock"]);
    enter();
    until("the provider's models", &|| rows() == ["mock-model"]);
    enter();
    assert_eq!(shell.modal(), None);

    // `Reload Agent Runtime`: while the daemon is replaced the pane says
    // so, then it is back on the same session, with what was said in it,
    // and a further message is answered by the new daemon.
    let before = agentd.handle();
    shell.execute(CommandId::ReloadAgentRuntime);
    app.run_effects();
    app.resolve();
    assert!(text().contains("Reloading the agent runtime"), "{}", text());
    assert!(shell.agent(pane).is_none());
    until("the pane again", &|| shell.agent(pane).is_some());
    assert!(!agentd.handle().same_runtime(&before));
    assert_eq!(shell.model.with_untracked(Workspace::tab_count), 2);
    until("what was said", &|| text().contains("hello there"));
    commit("and again");
    enter();
    until("the second answer", &|| {
        text().matches("ACP-OK").count() >= 2
    });

    // The board's organizer: a session the daemon starts, which the
    // workspace did not have, in a pane beside the board.
    let agents = || {
        shell.model.with_untracked(|model| {
            (model.session_summaries().iter())
                .filter(|session| session.kind == horizon_workspace::SessionKind::Agent)
                .map(|session| session.id)
                .collect::<Vec<_>>()
        })
    };
    let shown = || {
        shell.model.with_untracked(|model| {
            (model.visible_panes().iter())
                .map(|pane| (pane.id, pane.session_id))
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(agents().len(), 1);
    shell.execute(CommandId::OpenBoard);
    app.run_effects();
    app.resolve();
    let board = shown()[0].0;
    assert!(shell.board_list(board).is_some());
    shell.execute(CommandId::OpenBoardOrganizer);
    until("the organizer", &|| agents().len() == 2);
    until("the organizer's pane", &|| {
        shown().len() == 2 && shell.agent(shown()[1].0).is_some()
    });
    let organizer = shown()[1].1.expect("a session");
    assert_ne!(
        Some(organizer),
        shell
            .model
            .with_untracked(|model| model.agent_session_id(pane))
    );
    // Asked for again, it is the same session, in the pane it has.
    shell.execute(CommandId::OpenBoardOrganizer);
    app.run_effects();
    app.resolve();
    std::thread::sleep(Duration::from_millis(300));
    app.resolve();
    assert_eq!(agents().len(), 2);
    assert_eq!(shown().len(), 2);

    // A task's session this workspace does not have: one the daemon
    // runs is taken in and shown; one it does not run is said so.
    let elsewhere = horizon_workspace::SessionId::new();
    let _elsewhere = (agentd.spawn())(workspace::Starting {
        session: elsewhere,
        from: None,
        directory: Some(directory.join("project")),
        role: None,
    });
    until("the daemon to have it", &|| {
        let running = agentd.running().expect("the daemon's sessions");
        running.iter().any(|agent| agent.session == elsewhere)
    });
    let notice = horizon_ui::reactive_graph::signal::RwSignal::new(None);
    shell.open_task_session(elsewhere, notice);
    until("the task's session", &|| agents().contains(&elsewhere));
    until("its pane", &|| {
        shown()
            .iter()
            .any(|(_, session)| *session == Some(elsewhere))
    });
    assert_eq!(notice.get_untracked(), None);
    shell.open_task_session(horizon_workspace::SessionId::new(), notice);
    until("the refusal", &|| notice.get_untracked().is_some());
    assert_eq!(agents().len(), 3);

    // The board says what the sessions its tasks are bound to are doing:
    // one this workspace has, one only the daemon has (which is taken
    // in), and one nobody runs.
    use horizon_board_view::activity::BoardSessionActivity;
    let only_the_daemon_s = horizon_workspace::SessionId::new();
    let _running = (agentd.spawn())(workspace::Starting {
        session: only_the_daemon_s,
        from: None,
        directory: Some(directory.join("project")),
        role: None,
    });
    until("the daemon to have it", &|| {
        let running = agentd.running().expect("the daemon's sessions");
        running
            .iter()
            .any(|agent| agent.session == only_the_daemon_s)
    });
    let nobody_s = horizon_workspace::SessionId::new();
    let board_store = horizon_board::Store::from_dir(&directory.join("project")).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (title, session) in [
                ("here", elsewhere),
                ("there", only_the_daemon_s),
                ("nowhere", nobody_s),
            ] {
                let task = (board_store.add(title, "", None, horizon_board::Position::Bottom))
                    .await
                    .unwrap();
                (board_store.bind_session(task.id, &session.as_uuid().to_string()))
                    .await
                    .unwrap();
            }
        });
    let list = shell.board_list(board).expect("the list");
    let told = |session| list.activity.get_untracked().get(&session).copied();
    let found = |activity: Option<BoardSessionActivity>| {
        activity.is_some_and(|activity| {
            !matches!(
                activity,
                BoardSessionActivity::Loading | BoardSessionActivity::Unavailable
            )
        })
    };
    until("what the workspace's session is doing", &|| {
        found(told(elsewhere))
    });
    until("the daemon's session, taken in", &|| {
        found(told(only_the_daemon_s)) && agents().contains(&only_the_daemon_s)
    });
    until("nobody's, out of reach", &|| {
        told(nobody_s) == Some(BoardSessionActivity::Unavailable)
    });
    assert!(text().contains("Unreachable"), "{}", text());

    // The daemon ends with the test.
    drop(app);
    assert!(agentd.handle().begin_reload());
    horizon_shell_runtime::wait_for_drain(&directory.join("agentd.sock"))
        .expect("the daemon drained");
    agentd.handle().stop_and_wait();
}
