//! The shell's agent pane over a real `horizon-agent-acp`, started as the
//! shell's child for the test with a data home and a configuration of its
//! own, and a
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
use horizon_control::contract::{EnvelopeBody, Invoke, Query};
use horizon_control::host::executor::ControlRequest;
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::{GetUntracked, WithUntracked};
use horizon_workspace::Workspace;
use horizon_workspace::commands::CommandId;
use horizon_workspace::types::{AgentStatus, Doing};
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

/// The test's `horizon-agent-acp` child, if one runs.
fn agent_child_of_now() -> Option<u32> {
    let own = std::process::id().to_string();
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .find_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
            let parent = status.lines().find_map(|line| line.strip_prefix("PPid:"))?;
            let command = std::fs::read(entry.path().join("cmdline")).ok()?;
            (parent.trim() == own
                && String::from_utf8_lossy(&command).contains("horizon-agent-acp"))
            .then_some(pid)
        })
}

/// The test's `horizon-agent-acp` child.
fn agent_child() -> u32 {
    agent_child_of_now().expect("the agent's process")
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
vendor = "openai"
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
        std::env::set_var("HORIZON_AGENT_ACP_BINARY", binary("horizon-agent-acp"));
        std::env::set_var("HORIZON_LOGD_BINARY", binary("horizon-logd"));
        std::env::set_var("HORIZON_CONFIG", &config);
        std::env::set_var("HORIZON_TEST_MOCK_KEY", "mock");
        std::env::set_var("HORIZON_DATA_HOME", directory.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", directory.join("run"));
        std::env::set_var("HORIZON_LOGD_SOCKET", directory.join("logd.sock"));
        // The project directory may be inside a repository (a sandbox keeps
        // its temporary files in the worktree): no checkpoints there.
        std::env::set_var("HORIZON_AGENT_TEST_NO_CHECKPOINTS", "1");
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
            "HORIZON_AGENT_SESSIONS_DIR",
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
        &directory.join("control.sock"),
        &directory.join("agent.log"),
    );
    let (mut shell, mut top) = (None, None);
    let app = App::new(1200, 800, "Test", workspace::css(), || {
        let spawn: workspace::Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let opening = Opening {
            agents: Some(agentd.spawn()),
            models: Some(std::sync::Arc::new(RuntimeModels(agentd.clone()))),
            agent_runtime: Some(agentd.runtime()),
            ..Opening::default()
        };
        let (root, built) = workspace::view(spawn, opening);
        (shell, top) = (Some(built), Some(root.0));
        root
    });
    let (shell, root) = (shell.unwrap(), top.unwrap());
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

    // Enter goes into the view's field; what is typed there is sent with
    // Enter, shown, and answered.
    app.resolve();
    press(&app, Key::Named(NamedKey::Enter), Modifiers::default());
    app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit(
        "hello there".to_string(),
    )));
    app.resolve();
    press(&app, Key::Named(NamedKey::Enter), Modifiers::default());
    until("the message", &|| text().contains("hello there"));
    until("the answer", &|| text().contains("ACP-OK"));
    // The tab is named for what the session is about: the message, and
    // then the title the agent gives the session, its small model's
    // summary of the message (the stand-in's one reply).
    let title = || {
        shell
            .model
            .with_untracked(|model| model.tab_summaries()[1].title.clone())
    };
    until("the agent's title", &|| title() == "ACP-OK");

    // What the session is doing is the model's, in the runtime's words:
    // the turn ended, in the pane that has the keys, so it has been
    // looked at. The tab is marked as a session at rest, and `horizon
    // sessions` says the same.
    let activity = || {
        shell.model.with_untracked(|model| {
            let session = model.active_session_id()?;
            model.session_activity(session).cloned()
        })
    };
    until("the turn's end", &|| {
        activity().is_some_and(|activity| {
            activity.doing == Doing::Agent(AgentStatus::Completed) && activity.seen
        })
    });
    until("the tab's mark", &|| {
        text().contains(&format!("✳ {}", title()))
    });
    let (reply, answer) = std::sync::mpsc::channel();
    shell.control_answering(
        &ControlRequest::Query(Query {
            what: "sessions".to_string(),
        }),
        move |body| reply.send(body).unwrap(),
    );
    let answer = answer
        .recv_timeout(Duration::from_secs(5))
        .expect("an answer");
    let EnvelopeBody::Sessions(sessions) = answer else {
        panic!("{answer:?}");
    };
    let told: Vec<_> = (sessions.sessions.iter())
        .filter(|session| session.kind == "agent")
        .filter_map(|session| session.activity.as_ref())
        .map(|activity| {
            (
                activity.state.as_str(),
                activity.attention.as_deref(),
                activity.seen,
            )
        })
        .collect();
    assert_eq!(told, [("completed", None, true)]);

    // The model picker: the providers and models the session's `model`
    // option offers, and the choice goes to the agent.
    let commit = |text: &str| {
        app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        app.resolve();
    };
    let enter = || press(&app, Key::Named(NamedKey::Enter), Modifiers::default());
    press(&app, Key::Character("'".into()), Modifiers::CONTROL);
    press(&app, Key::Character(":".into()), Modifiers::default());
    commit("switch model");
    enter();
    std::assert_matches!(shell.modal(), Some(Modal::Models(_)));
    // The names of the picker's rows: in the sheet over the workspace.
    let rows = || {
        let sheet = horizon_ui::testing::find_one(&app, root, "sheet");
        let names = horizon_ui::testing::find(&app, sheet, "name");
        let document = app.document();
        let document = document.borrow();
        (names.into_iter())
            .map(|name| document.get_node(name).unwrap().text_content())
            .collect::<Vec<_>>()
    };
    // The session's effort is above the providers: a model no catalog
    // lists takes every level.
    until("the providers", &|| rows() == ["Effort", "mock"]);
    let down = || press(&app, Key::Named(NamedKey::ArrowDown), Modifiers::default());
    enter();
    let levels = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    until("the levels", &|| rows() == levels);
    // Escape goes back a step before it leaves.
    press(&app, Key::Named(NamedKey::Escape), Modifiers::default());
    assert_eq!(rows(), ["Effort", "mock"]);
    down();
    enter();
    until("the provider's models", &|| rows() == ["mock-model"]);
    press(&app, Key::Named(NamedKey::Escape), Modifiers::default());
    assert_eq!(rows(), ["Effort", "mock"]);
    down();
    enter();
    until("the provider's models", &|| rows() == ["mock-model"]);
    enter();
    assert_eq!(shell.modal(), None);

    // `Switch Effort…` opens the picker at the levels; the level chosen
    // goes to the daemon, and the composer's chip says it.
    press(&app, Key::Character("'".into()), Modifiers::CONTROL);
    press(&app, Key::Character(":".into()), Modifiers::default());
    commit("switch effort");
    enter();
    std::assert_matches!(shell.modal(), Some(Modal::Effort(_)));
    until("the levels", &|| rows() == levels);
    commit("high");
    until("the level typed", &|| rows() == ["high", "xhigh"]);
    enter();
    assert_eq!(shell.modal(), None);
    until("the chip's effort", &|| {
        text().contains("mock-model · high")
    });

    // The control plane sets the same options, by the names of their
    // entries: an effort, and a model in a group of the `model` option.
    let session = (shell.model)
        .with_untracked(|model| model.pane_session_id(pane))
        .expect("the pane's session")
        .as_uuid()
        .to_string();
    let control = |command: &str, args: serde_json::Value| {
        let (reply, answer) = std::sync::mpsc::channel();
        shell.control_answering(
            &ControlRequest::Invoke(Invoke {
                command: command.to_string(),
                args,
            }),
            move |body| reply.send(body).unwrap(),
        );
        answer
            .recv_timeout(Duration::from_secs(5))
            .expect("an answer")
    };
    let answer = control(
        "set-effort",
        serde_json::json!({ "session_id": session, "level": "low" }),
    );
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    until("the effort set", &|| text().contains("mock-model · low"));
    let answer = control(
        "set-model",
        serde_json::json!({ "session_id": session, "provider": "mock", "model": "mock-model" }),
    );
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    let answer = control(
        "set-model",
        serde_json::json!({ "session_id": session, "provider": "mock", "model": "none-such" }),
    );
    std::assert_matches!(answer, EnvelopeBody::Error(_), "{answer:?}");

    // The agent's process ends: the shell starts it again, and the pane,
    // as it was drawn, is attached to its session again with
    // `session/resume` -- what was said in it is back, and a further
    // message is answered by the new process.
    let element = shell.pane_element(pane);
    let first = agent_child();
    let _ = Command::new("kill")
        .args(["-KILL", &first.to_string()])
        .status();
    until("another process", &|| {
        agent_child_of_now().is_some_and(|now| now != first)
    });
    until("what was said", &|| {
        text().contains("hello there") && !text().contains("reopen the session")
    });
    assert_eq!(shell.pane_element(pane), element);
    enter();
    commit("and again");
    enter();
    until("the second answer", &|| {
        text().matches("ACP-OK").count() >= 2
    });

    // The board, beside the agent.
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
    assert_eq!(shell.plugin(board), Some("board-list"));

    // A task's session this workspace does not have: one the daemon
    // runs is taken in and shown; one it does not run is said so.
    let started = horizon_workspace::SessionId::new();
    let spawned = (agentd.spawn())(workspace::Starting {
        session: started,
        from: None,
        directory: Some(directory.join("project")),
    });
    // (Under the name the agent gives it.)
    until("the daemon to have it", &|| {
        let named = spawned.session_id();
        let running = agentd.running().expect("the daemon's sessions");
        named != started && running.iter().any(|agent| agent.session == named)
    });
    let elsewhere = spawned.session_id();
    // What whoever asked is told went wrong.
    let notice = horizon_ui::reactive_graph::signal::RwSignal::new(None);
    let failed = horizon_ui::Callback::new(move |error: String| {
        horizon_ui::reactive_graph::traits::Set::set(&notice, Some(error))
    });
    shell.open_task_session(elsewhere, failed);
    until("the task's session", &|| agents().contains(&elsewhere));
    until("its pane", &|| {
        shown()
            .iter()
            .any(|(_, session)| *session == Some(elsewhere))
    });
    assert_eq!(notice.get_untracked(), None);
    shell.open_task_session(horizon_workspace::SessionId::new(), failed);
    until("the refusal", &|| notice.get_untracked().is_some());
    assert_eq!(agents().len(), 2);

    // The board says what the sessions its tasks are bound to are doing:
    // one this workspace has, one only the daemon has (which is taken
    // in), and one nobody runs.
    use horizon::board::activity::BoardSessionActivity;
    let started = horizon_workspace::SessionId::new();
    let running = (agentd.spawn())(workspace::Starting {
        session: started,
        from: None,
        directory: Some(directory.join("project")),
    });
    until("the daemon to have it", &|| {
        let named = running.session_id();
        let running = agentd.running().expect("the daemon's sessions");
        named != started && running.iter().any(|agent| agent.session == named)
    });
    let only_the_daemon_s = running.session_id();
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
    let told = |session| shell.session_activity(session);
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

    // `horizon new agent` is answered once the agent has named the new
    // session: with the name, which the workspace keeps it under and the
    // daemon runs it by.
    let (reply, answer) = std::sync::mpsc::channel();
    shell.control_answering(
        &ControlRequest::Invoke(Invoke {
            command: "new".to_string(),
            args: serde_json::json!({ "plugin": "agent" }),
        }),
        move |body| _ = reply.send(body),
    );
    let body = std::cell::RefCell::new(None);
    until("the new session's name", &|| {
        let mut body = body.borrow_mut();
        if body.is_none() {
            *body = answer.try_recv().ok();
        }
        body.is_some()
    });
    let Some(EnvelopeBody::Ok {
        session_id: Some(named),
    }) = body.into_inner()
    else {
        panic!("a session id");
    };
    let named = horizon_workspace::SessionId::from_uuid(named.parse().unwrap());
    assert!(agents().contains(&named));
    let running = agentd.running().expect("the daemon's sessions");
    assert!(running.iter().any(|agent| agent.session == named));

    // The agent runtime ends with the test.
    drop(app);
    agentd.handle().stop_and_wait();
}
