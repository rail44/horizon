//! An extension shown in a pane of the shell: the Markdown view built as a
//! wasm component (`plugins/markdown`), loaded from its package, its state
//! kept in the model as it changes, its command carried out from the
//! palette, and a new pane of it made on a file found by typing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horizon::workspace::{Opening, Placement, Shell, Spawn, css, view};
use horizon_control::contract::{EnvelopeBody, Invoke};
use horizon_control::host::executor::ControlRequest;
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_ui::testing::click_at;
use horizon_workspace::commands::CommandId;
use horizon_workspace::{PluginPane, Workspace};
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

/// The extension's component, as `scripts/check-extension.sh` builds it.
fn component() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/wasm32-wasip2/debug/horizon_markdown_plugin.wasm")
}

/// A package of the extension in `directory`: its manifest and its
/// component.
fn package(directory: &Path) -> PathBuf {
    let package = directory.join("package");
    std::fs::create_dir_all(&package).unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/markdown/plugin.toml");
    std::fs::copy(manifest, package.join("plugin.toml")).unwrap();
    std::fs::copy(component(), package.join("markdown.wasm")).unwrap();
    package
}

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

impl Opened {
    fn model<R>(&self, read: impl FnOnce(&Workspace) -> R) -> R {
        self.shell.model.with_untracked(read)
    }

    fn all(&self, class: &str) -> Vec<NodeId> {
        let document = self.app.document();
        let document = document.borrow();
        let mut found = Vec::new();
        collect(&document, document.root_element().id, class, &mut found);
        found
    }

    fn middle(&self, node: NodeId) -> (f32, f32) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (
            (bounds.x + bounds.width / 2.) as f32,
            (bounds.y + bounds.height / 2.) as f32,
        )
    }

    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        while !met() {
            assert!(started.elapsed() < Duration::from_secs(20), "no {what}");
            std::thread::sleep(Duration::from_millis(5));
            self.app.run_effects();
            self.app.resolve();
        }
    }

    fn text(&self) -> String {
        horizon_ui::testing::text(&self.app)
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
        self.app.resolve();
    }

    fn type_text(&self, text: &str) {
        for character in text.chars() {
            self.key(Key::Character(character.to_string()));
        }
    }
}

#[test]
#[ignore = "needs the extension built for wasm32-wasip2: scripts/check-extension.sh"]
fn an_extension_is_shown_in_a_pane_that_keeps_its_state_and_carries_out_its_command() {
    let directory = std::env::temp_dir().join(format!("hzb-ext-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let (first, second) = (directory.join("first.md"), directory.join("second.md"));
    std::fs::write(&first, "# One\n\n[to the second](second.md)\n").unwrap();
    std::fs::write(&second, "# Two\n").unwrap();
    let package = package(&directory);

    let config = horizon_config::RawConfig {
        extensions: vec![package.to_string_lossy().into_owned()],
        ..horizon_config::RawConfig::default()
    };
    let mut shell = None;
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                config,
                ..Opening::default()
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

    // A pane of the extension on a file: the state is the extension's
    // own type, as text.
    let state = format!("{{path: some({:?})}}", first.to_string_lossy());
    opened.shell.create_plugin(
        PluginPane {
            name: "markdown-extension".to_string(),
            title: "Markdown".to_string(),
            state: serde_json::Value::String(state),
        },
        Placement::NewTab,
    );
    opened.until("the file", || opened.text().contains("One"));
    let pane = opened
        .model(|model| model.cursor_pane_id())
        .expect("the new pane");
    let kept = |opened: &Opened| {
        opened.model(|model| {
            let plugin = model.pane_plugin(pane).expect("the pane's plugin");
            (
                plugin.title.clone(),
                plugin.state.as_str().unwrap_or_default().to_string(),
            )
        })
    };
    // The extension titled the pane for its file.
    opened.until("the title", || kept(&opened).0 == "first.md");

    // A link followed: the state the pane keeps follows the extension's.
    let (x, y) = opened.middle(opened.all("link")[0]);
    click_at(&opened.app, x, y);
    opened.until("the second file", || opened.text().contains("Two"));
    opened.until("the state kept", || kept(&opened).1.contains("second.md"));
    assert_eq!(kept(&opened).0, "second.md");

    // The extension's command, by the name its manifest gives.
    assert!(opened.shell.execute_plugin("back"));
    opened.until("the first file again", || opened.text().contains("One"));
    opened.until("the state kept again", || {
        kept(&opened).1.contains("first.md")
    });

    // The file changed on disk: the host's watch wakes the extension's
    // task, and the pane reads it again.
    std::fs::write(&first, "# Uno\n").unwrap();
    opened.until("the file read again", || opened.text().contains("Uno"));

    // A new pane of the extension: chosen among the views, then the file
    // it is made on found by typing among the files under the directory
    // the shell works in.
    std::env::set_current_dir(&directory).unwrap();
    opened.shell.execute(CommandId::NewTab);
    opened.app.resolve();
    opened.type_text("markdown");
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the files listed", || opened.text().contains("2/2"));
    opened.type_text("sec");
    opened.until("the file found", || opened.text().contains("1/2"));
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the pane on the file", || opened.text().contains("Two"));
    let made = opened
        .model(|model| model.cursor_pane_id())
        .expect("the pane made");
    assert_ne!(made, pane);
    let state = opened.model(|model| {
        let plugin = model.pane_plugin(made).expect("the pane's plugin");
        plugin.state.as_str().unwrap_or_default().to_string()
    });
    assert!(state.contains("second.md"), "{state}");

    // The same from the command line: a pane of the extension made on a
    // file named from where the caller is, and the extension's command
    // by its name.
    let ask = |command: &str, args: serde_json::Value| {
        let answer = opened.shell.control(&ControlRequest::Invoke(Invoke {
            command: command.to_string(),
            args,
        }));
        opened.app.run_effects();
        opened.app.resolve();
        answer
    };
    std::fs::write(directory.join("third.md"), "# Three\n\n[on](second.md)\n").unwrap();
    let new = |arguments: serde_json::Value| {
        serde_json::json!({
            "plugin": "markdown-extension",
            "arguments": arguments,
            "directory": directory.to_string_lossy(),
            "activate": true,
        })
    };
    let answer = ask("new", new(serde_json::json!(["third.md"])));
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    opened.until("the pane on the third file", || {
        opened.text().contains("Three")
    });
    let third = opened
        .model(|model| model.cursor_pane_id())
        .expect("the pane made");
    assert_ne!(third, made);
    let (x, y) = opened.middle(*opened.all("link").last().expect("the link"));
    click_at(&opened.app, x, y);
    opened.until("the link followed", || {
        opened.model(|model| {
            let plugin = model.pane_plugin(third).expect("the pane's plugin");
            plugin
                .state
                .as_str()
                .unwrap_or_default()
                .contains("second.md")
        })
    });
    let answer = ask("plugin-command", serde_json::json!({ "name": "back" }));
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    opened.until("back on the third file", || {
        opened.model(|model| {
            let plugin = model.pane_plugin(third).expect("the pane's plugin");
            plugin
                .state
                .as_str()
                .unwrap_or_default()
                .contains("third.md")
        })
    });
    // An extension being written, shown in a preview pane from its
    // package's directory: the pane's alone, made on what the pane was
    // given, and read again when its component is built again.
    let preview = serde_json::json!({
        "path": package.to_string_lossy(),
        "arguments": ["third.md"],
        "directory": directory.to_string_lossy(),
        "activate": true,
    });
    let answer = ask("preview", preview);
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    let previewing = opened
        .model(|model| model.cursor_pane_id())
        .expect("the preview pane");
    assert_ne!(previewing, third);
    // (What the preview pane itself says, whatever the other panes do.)
    let shows = |text: &str| {
        let panes = opened.all("preview");
        let document = opened.app.document();
        let document = document.borrow();
        (panes.iter())
            .filter_map(|pane| document.get_node(*pane))
            .any(|pane| pane.text_content().contains(text))
    };
    opened.until("the package shown", || shows("Three"));
    let (x, y) = opened.middle(*opened.all("link").last().expect("the link"));
    click_at(&opened.app, x, y);
    opened.until("the link followed in the preview", || shows("Two"));
    std::fs::copy(component(), package.join("markdown.wasm")).unwrap();
    opened.until("the package read again", || shows("Three"));

    // What is not there is said.
    let refused = [
        ask("new", new(serde_json::json!(["missing.md"]))),
        ask("new", new(serde_json::json!([]))),
        ask("plugin-command", serde_json::json!({ "name": "no-such" })),
        ask(
            "new",
            serde_json::json!({ "plugin": "no-such", "directory": "/", "activate": false }),
        ),
    ];
    for answer in refused {
        assert!(matches!(answer, EnvelopeBody::Error { .. }), "{answer:?}");
    }

    // What is saved of the pane: the extension's name and its state.
    let saved = opened.model(|model| model.to_persisted_json().unwrap());
    assert!(saved.contains("markdown-extension"), "{saved}");
    assert!(saved.contains("first.md"), "{saved}");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
#[ignore = "needs the extension built for wasm32-wasip2: scripts/check-extension.sh"]
fn a_package_the_configuration_names_is_read_on_reload_and_again_when_built() {
    let directory = std::env::temp_dir().join(format!("hzb-ext-reload-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("first.md");
    std::fs::write(&file, "# One\n").unwrap();
    let package = package(&directory);

    // The shell starts with no extension; the configuration read again
    // names the package.
    let named = package.to_string_lossy().into_owned();
    let mut shell = None;
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let reload = Box::new(move || {
            Ok(horizon_config::RawConfig {
                extensions: vec![named.clone()],
                ..horizon_config::RawConfig::default()
            })
        });
        let (root, built) = view(
            spawn,
            Opening {
                reload: Some(reload),
                ..Opening::default()
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
    let new = || {
        let answer = opened.shell.control(&ControlRequest::Invoke(Invoke {
            command: "new".to_string(),
            args: serde_json::json!({
                "plugin": "markdown-extension",
                "arguments": ["first.md"],
                "directory": directory.to_string_lossy(),
                "activate": true,
            }),
        }));
        opened.app.run_effects();
        opened.app.resolve();
        answer
    };
    assert!(matches!(new(), EnvelopeBody::Error { .. }));
    opened.shell.execute(CommandId::ReloadConfig);
    opened.until("the extension loaded", || {
        matches!(new(), EnvelopeBody::Ok { .. })
    });
    opened.until("the file", || opened.text().contains("One"));
    let pane = opened
        .model(|model| model.cursor_pane_id())
        .expect("the pane");
    let title = |opened: &Opened| {
        opened.model(|model| model.pane_plugin(pane).expect("the plugin").title.clone())
    };
    opened.until("the title", || title(&opened) == "first.md");

    // The package built again: the extension is read again by itself, and
    // the pane that shows it is made again on the file it showed. (What
    // tells the new one from the old here is its manifest: the title the
    // view chooser lists it under.)
    let manifest = std::fs::read_to_string(package.join("plugin.toml")).unwrap();
    let renamed = manifest.replace("title = \"Markdown\"", "title = \"Notes\"");
    assert_ne!(manifest, renamed);
    std::fs::write(package.join("plugin.toml"), renamed).unwrap();
    std::fs::copy(component(), package.join("markdown.wasm")).unwrap();
    opened.until("the extension read again", || {
        // (The chooser lists what is loaded when it is opened.)
        opened.shell.execute(CommandId::NewTab);
        opened.app.resolve();
        let listed = opened.text().contains("Notes");
        opened.key(Key::Named(NamedKey::Escape));
        listed
    });
    opened.until("the pane made again", || opened.text().contains("One"));
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
#[ignore = "needs the extension built for wasm32-wasip2: scripts/check-extension.sh"]
fn an_extension_with_nothing_happening_has_nothing_to_draw() {
    use std::task::{Context, Waker};

    use horizon_ui::Live;
    use horizon_ui::blitz_dom::Document;

    let directory = std::env::temp_dir().join(format!("hzb-ext-still-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("first.md");
    std::fs::write(&file, "# One\n").unwrap();
    let package = package(&directory);
    let config = horizon_config::RawConfig {
        extensions: vec![package.to_string_lossy().into_owned()],
        ..horizon_config::RawConfig::default()
    };
    let mut shell = None;
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                config,
                ..Opening::default()
            },
        );
        shell = Some(built);
        root
    });
    app.resolve();
    let shell = shell.unwrap();
    // One pane of the extension, and one in a preview pane from its
    // package.
    let state = format!("{{path: some({:?})}}", file.to_string_lossy());
    shell.create_plugin(
        PluginPane {
            name: "markdown-extension".to_string(),
            title: "Markdown".to_string(),
            state: serde_json::Value::String(state),
        },
        Placement::NewTab,
    );
    shell.control(&ControlRequest::Invoke(Invoke {
        command: "preview".to_string(),
        args: serde_json::json!({
            "path": package.to_string_lossy(),
            "arguments": ["first.md"],
            "directory": directory.to_string_lossy(),
            "split": serde_json::Value::Null,
            "activate": false,
        }),
    }));
    // As the window's own wiring does: the waker is handed over once, and
    // the window asks the document whether there is something to draw,
    // draws if so, and asks again after every draw.
    let handed = std::cell::Cell::new(false);
    app.on_poll(move |waker| {
        if !handed.replace(true) {
            shell.set_waker(waker.clone());
        }
    });
    let mut live = Live(app);
    let turn = |live: &mut Live| {
        let draws = live.poll(Some(Context::from_waker(Waker::noop())));
        if draws {
            live.0.resolve();
        }
        draws
    };
    let still = |live: &mut Live, what: &str| {
        let (started, mut still) = (Instant::now(), 0);
        while still < 40 {
            assert!(started.elapsed() < Duration::from_secs(20), "{what}");
            still = if turn(live) { 0 } else { still + 1 };
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    still(&mut live, "never still");

    // The file changed: the extension is woken for it, shows it, and is
    // still again.
    std::fs::write(&file, "# Uno\n").unwrap();
    let started = Instant::now();
    while !horizon_ui::testing::text(&live.0).contains("Uno") {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "not read again"
        );
        turn(&mut live);
        std::thread::sleep(Duration::from_millis(5));
    }
    still(&mut live, "never still again");
    let _ = std::fs::remove_dir_all(&directory);
}
