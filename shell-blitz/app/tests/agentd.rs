//! The shell's agent pane over a real `horizon-agentd`, started for the
//! test with sockets, a data home and a configuration of its own, and a
//! provider that is a local stand-in (`scripts/mock-openai-provider.py`):
//! a message typed into the composer is answered in the pane.
//!
//! The daemons are the root workspace's binaries: build them there first
//! (`cargo build --workspace` at the repository's root).

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use horizon_blitz::agentd::{Agentd, RuntimeModels};
use horizon_blitz::workspace::{self, Modal, Opening};
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_workspace::Workspace;
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

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary = |name: &str| {
        let binary = root.join("target/debug").join(name);
        assert!(
            binary.is_file(),
            "no {}: run `cargo build --workspace` at the repository's root first",
            binary.display()
        );
        binary
    };
    // A short path: a socket's is bounded.
    let scratch = Scratch(std::env::temp_dir().join(format!("hzba-{}", std::process::id())));
    let directory = scratch.0.clone();
    std::fs::create_dir_all(directory.join("project")).unwrap();

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
api_key_env = "HORIZON_BLITZ_TEST_MOCK_KEY"
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
        std::env::set_var("HORIZON_BLITZ_TEST_MOCK_KEY", "mock");
        std::env::set_var("HORIZON_DATA_HOME", directory.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", directory.join("run"));
        std::env::set_var("HORIZON_LOGD_SOCKET", directory.join("logd.sock"));
        for unset in [
            "OPENAI_API_KEY",
            "HORIZON_AGENT_EVENT_LOG",
            "HORIZON_AGENT_STATE_DB",
            "HORIZON_SOCKET",
            "HORIZON_SESSION_ID",
        ] {
            std::env::remove_var(unset);
        }
        std::env::set_current_dir(directory.join("project")).unwrap();
    }
    std::fs::create_dir_all(directory.join("run")).unwrap();

    let agentd = Agentd::start(
        &directory.join("agentd.sock"),
        &directory.join("control.sock"),
    );
    let mut shell = None;
    let app = App::new(1200, 800, &workspace::css(), || {
        let spawn: workspace::Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let opening = Opening {
            agents: Some(agentd.spawn()),
            models: Some(std::sync::Arc::new(RuntimeModels(agentd.handle.clone()))),
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
            if class == Some("name") {
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

    // The daemon ends with the test.
    drop(app);
    assert!(agentd.handle.begin_reload());
    horizon_shell_runtime::wait_for_drain(&directory.join("agentd.sock"))
        .expect("the daemon drained");
    agentd.handle.stop_and_wait();
}
