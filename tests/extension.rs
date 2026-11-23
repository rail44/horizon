//! An extension shown in a pane of the shell: the Markdown view built as a
//! wasm component (`plugins/markdown`), loaded from its file, its state
//! kept in the model as it changes, its command carried out from the
//! palette, and a new pane of it made on a file found by typing.
//!
//! The components are built before these run (`scripts/gate.sh`):
//! `cargo build -p horizon-markdown-plugin -p horizon-theme-settings-plugin
//! --target wasm32-wasip2`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use horizon::workspace::{Opening, Placement, Shell, Spawn, css, view};
use horizon_control::contract::{EnvelopeBody, Invoke, Query};
use horizon_control::host::executor::ControlRequest;
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_ui::testing::click_at;
use horizon_workspace::commands::CommandId;
use horizon_workspace::{PluginPane, Workspace};
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

/// The extension's component, where cargo builds it.
fn component() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/wasm32-wasip2/debug/horizon_markdown_plugin.wasm")
}

/// A shell with the extensions it comes with, found as an executable in
/// `target/debug` finds them.
fn open() -> Opened {
    open_with(Opening::default())
}

/// The same, over what else `opening` says.
fn open_with(opening: Opening) -> Opened {
    let mut extensions =
        horizon::extension::own_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug"));
    assert_eq!(extensions.first(), Some(&component()));
    assert_eq!(extensions.len(), 6, "{extensions:?}");
    // (And those `opening` adds to them.)
    extensions.extend(opening.extensions.iter().cloned());
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
    // (They are read once the shell is drawn.)
    opened.until("the extensions", || opened.shell.extensions_read());
    opened
}

/// A copy of the extension in `directory`, for a test to change: its
/// component, which is the whole of it.
fn package(directory: &Path) -> PathBuf {
    let package = directory.join("markdown.wasm");
    std::fs::copy(component(), &package).unwrap();
    package
}

/// `component` with its manifest's title changed: a build of an
/// extension that is not the last one, with only its own file to tell
/// them apart by. The title is of the same length, so the component is
/// the same but for those bytes.
fn retitled(component: &Path) -> Vec<u8> {
    let (from, to) = (
        &b"\"title\":\"Markdown\""[..],
        &b"\"title\":\"Notebook\""[..],
    );
    let mut bytes = std::fs::read(component).unwrap();
    // (Wherever the bytes are: the build keeps them in more than one
    // place.)
    let found: Vec<usize> = (bytes.windows(from.len()).enumerate())
        .filter_map(|(at, window)| (window == from).then_some(at))
        .collect();
    assert!(!found.is_empty(), "the manifest's title");
    for at in found {
        bytes[at..at + to.len()].copy_from_slice(to);
    }
    bytes
}

struct Opened {
    app: App,
    shell: Shell,
}

impl Opened {
    fn model<R>(&self, read: impl FnOnce(&Workspace) -> R) -> R {
        self.shell.model.with_untracked(read)
    }

    /// The elements of a class in the cursor's pane, the extension's, in
    /// document order.
    fn all(&self, class: &str) -> Vec<NodeId> {
        let pane = (self.model(Workspace::cursor_pane_id))
            .and_then(|pane| self.shell.pane_element(pane))
            .expect("the cursor's pane, drawn");
        horizon_ui::testing::find(&self.app, pane, class)
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

    /// Asks the shell as the command line does, and lets what that
    /// changed take effect.
    fn ask(&self, command: &str, args: serde_json::Value) -> EnvelopeBody {
        let answer = self.shell.control(&ControlRequest::Invoke(Invoke {
            command: command.to_string(),
            args,
        }));
        self.app.run_effects();
        self.app.resolve();
        answer
    }

    /// Asks the shell as the command line does for what is answered once
    /// work off the UI thread is done, and waits for the answer.
    fn answered(&self, command: &str, args: serde_json::Value) -> EnvelopeBody {
        let (answer, answers) = std::sync::mpsc::channel();
        let request = ControlRequest::Invoke(Invoke {
            command: command.to_string(),
            args,
        });
        (self.shell).control_answering(&request, move |body| _ = answer.send(body));
        let body = std::cell::RefCell::new(None);
        self.until(command, || {
            let mut body = body.borrow_mut();
            if body.is_none() {
                *body = answers.try_recv().ok();
            }
            body.is_some()
        });
        body.into_inner().expect("an answer")
    }

    /// The extensions loaded, as `horizon extension list` is told them:
    /// each one's name with where it is from and its file.
    fn extensions(&self) -> Vec<(String, String, PathBuf)> {
        let answer = self.shell.control(&ControlRequest::Query(Query {
            what: "extensions".to_string(),
        }));
        listed(&answer)
    }

    /// `horizon new markdown <file>`, from `directory`.
    fn new_markdown(&self, directory: &Path, file: &str) -> EnvelopeBody {
        self.ask(
            "new",
            serde_json::json!({
                "plugin": "markdown",
                "arguments": [file],
                "directory": directory.to_string_lossy(),
                "activate": true,
            }),
        )
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

    fn type_text(&self, text: &str) {
        for character in text.chars() {
            self.key(Key::Character(character.to_string()));
        }
    }
}

#[test]
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
            name: "markdown".to_string(),
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
            "plugin": "markdown",
            "arguments": arguments,
            "directory": directory.to_string_lossy(),
            "activate": true,
        })
    };
    let answer = ask("new", new(serde_json::json!(["third.md"])));
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
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
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
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
        "plugin": "preview",
        "arguments": [package.to_string_lossy(), "third.md"],
        "directory": directory.to_string_lossy(),
        "activate": true,
    });
    let answer = ask("new", preview);
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
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
    std::fs::copy(component(), &package).unwrap();
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
        std::assert_matches!(answer, EnvelopeBody::Error { .. }, "{answer:?}");
    }

    // What is saved of the pane: the extension's name and its state.
    let saved = opened.model(|model| model.to_persisted_json().unwrap());
    assert!(saved.contains("markdown"), "{saved}");
    assert!(saved.contains("first.md"), "{saved}");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
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
                "plugin": "markdown",
                "arguments": ["first.md"],
                "directory": directory.to_string_lossy(),
                "activate": true,
            }),
        }));
        opened.app.run_effects();
        opened.app.resolve();
        answer
    };
    std::assert_matches!(new(), EnvelopeBody::Error { .. });
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
    std::fs::write(&package, retitled(&component())).unwrap();
    opened.until("the extension read again", || {
        // (The chooser lists what is loaded when it is opened.)
        opened.shell.execute(CommandId::NewTab);
        opened.app.resolve();
        let listed = opened.text().contains("Notebook");
        opened.key(Key::Named(NamedKey::Escape));
        listed
    });
    opened.until("the pane made again", || opened.text().contains("One"));
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
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
            name: "markdown".to_string(),
            title: "Markdown".to_string(),
            state: serde_json::Value::String(state),
        },
        Placement::NewTab,
    );
    shell.control(&ControlRequest::Invoke(Invoke {
        command: "new".to_string(),
        args: serde_json::json!({
            "plugin": "preview",
            "arguments": [package.to_string_lossy(), "first.md"],
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

/// The Markdown view the shell comes with is this extension: what the
/// view does with links, the way back and a file that changes, through a
/// pane.
#[test]
fn the_shell_s_own_markdown_view_is_the_extension() {
    let directory = std::env::temp_dir().join(format!("hzb-ext-own-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("index.md"),
        "---\ntitle: Index\nwritten: 2026-11-08\n---\n\nSee [a](a.md), [code](src/lib.rs).\n",
    )
    .unwrap();
    std::fs::write(directory.join("a.md"), "---\ntitle: Page A\n---\nbody\n").unwrap();
    let opened = open();
    let tabs = |opened: &Opened| opened.model(Workspace::tab_count);

    // A file there is not is refused; one there is, shown in a new tab
    // named for it.
    let missing = opened.new_markdown(&directory, "later.md");
    std::assert_matches!(missing, EnvelopeBody::Error(_), "{missing:?}");
    let answer = opened.new_markdown(&directory, "index.md");
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    opened.until("the file", || opened.text().contains("written 2026-11-08"));
    opened.until("the title", || opened.kept().0 == "index.md");
    assert_eq!(tabs(&opened), 2);

    // A link that is not to a Markdown file stays put, with a notice.
    let (x, y) = opened.middle(opened.all("link")[1]);
    click_at(&opened.app, x, y);
    opened.until("the notice", || {
        opened.text().contains("Not a Markdown file: src/lib.rs")
    });
    assert!(opened.kept().1.contains("index.md"));

    // A link to one opens in place: the pane's file moves with it, and
    // the back key returns.
    let (x, y) = opened.middle(opened.all("link")[0]);
    click_at(&opened.app, x, y);
    opened.until("the page", || opened.text().contains("Page A"));
    opened.until("the state", || opened.kept().1.contains("a.md"));
    assert_eq!(opened.kept().0, "a.md");
    opened.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        key: Key::Named(NamedKey::Backspace),
        code: keyboard_types::Code::Backspace,
        modifiers: Modifiers::default(),
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
        text: None,
    }));
    opened.until("the index again", || {
        opened.text().contains("written 2026-11-08")
    });
    opened.until("the state again", || opened.kept().1.contains("index.md"));

    // Asked for a file a pane shows already, that pane is the one.
    let answer = opened.new_markdown(&directory, "index.md");
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    assert_eq!(tabs(&opened), 2);

    // A file that went away is said in the pane, and one written in its
    // place is shown.
    std::fs::remove_file(directory.join("index.md")).unwrap();
    opened.until("the file gone", || opened.text().contains("index.md: "));
    std::fs::write(directory.join("index.md"), "---\ntitle: Now\n---\n").unwrap();
    opened.until("the file back", || opened.text().contains("Now"));
    assert_eq!(tabs(&opened), 2);
    let _ = std::fs::remove_dir_all(&directory);
}

/// The theme settings are an extension too: the seed is the shell's,
/// which draws the theme of it as it changes and writes it when asked
/// (`horizon:ui/theme`), and a pane of it keeps nothing.
#[test]
fn the_theme_settings_apply_a_seed_as_it_changes_and_save_it_when_asked() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let saved = Rc::new(RefCell::new(Vec::new()));
    let log = saved.clone();
    let opened = open_with(Opening {
        save_theme: Some(Rc::new(move |seed: &horizon_theme::seed::Seed| {
            log.borrow_mut().push(seed.accent_config_value());
            Ok(PathBuf::from("/somewhere/config.toml"))
        })),
        ..Opening::default()
    });
    let scheme = |opened: &Opened| opened.shell.theme.with_untracked(|theme| *theme.scheme());
    let before = scheme(&opened);
    let answer = opened.ask(
        "new",
        serde_json::json!({ "plugin": "theme-settings", "directory": "/", "activate": true }),
    );
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    // The derived colors, as swatches: sixteen, three, five, four, four.
    opened.until("the swatches", || opened.all("chip").len() == 32);
    assert!(opened.all("unsaved").is_empty());
    // (Nothing is kept of the pane but which plugin it shows.)
    assert_eq!(opened.kept().1, "");

    // The accent, chosen by name: the theme is drawn with that hue at
    // once, and nothing is written.
    let click = |class: &str, index: usize| {
        let (x, y) = opened.middle(opened.all(class)[index]);
        click_at(&opened.app, x, y);
        opened.app.run_effects();
        opened.app.resolve();
    };
    click("trigger", 0);
    opened.until("the choices", || !opened.all("option").is_empty());
    click("option", 0);
    opened.until("the accent drawn", || {
        scheme(&opened).accent != before.accent
    });
    opened.until("unsaved", || opened.all("unsaved").len() == 1);
    assert!(saved.borrow().is_empty());

    // The text contrast, from the keys of its slider.
    click("track", 0);
    opened.key(Key::Named(NamedKey::Home));
    opened.app.run_effects();
    let low = scheme(&opened).foreground;
    opened.key(Key::Named(NamedKey::End));
    opened.until("the contrast drawn", || scheme(&opened).foreground != low);

    // Save: the shell writes the seed, and the view says where once it
    // has.
    click("save", 0);
    opened.until("saved", || *saved.borrow() == ["red"]);
    opened.until("no longer unsaved", || opened.all("unsaved").is_empty());
    opened.until("where it was saved", || {
        opened.text().contains("Saved to /somewhere/config.toml")
    });

    // Asked for again, the pane there is is the one.
    let tabs = opened.model(Workspace::tab_count);
    let answer = opened.ask(
        "new",
        serde_json::json!({ "plugin": "theme-settings", "directory": "/", "activate": true }),
    );
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    assert_eq!(opened.model(Workspace::tab_count), tabs);
}

/// The extensions are read once the shell is drawn, not before: a pane of
/// one that is there already (kept from the last run) says so until they
/// have been, and is made then.
#[test]
fn a_pane_of_an_extension_waits_for_the_extensions_to_be_read() {
    let directory = std::env::temp_dir().join(format!("hzb-ext-later-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("first.md");
    std::fs::write(&file, "# One\n").unwrap();
    let extensions =
        horizon::extension::own_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug"));
    // The reading waits until it is let go, however soon it would end.
    let (let_go, held) = std::sync::mpsc::channel::<()>();
    let held = std::sync::Mutex::new(held);
    let read_extensions: horizon::workspace::ReadExtensions = Arc::new(move |files, own| {
        let _ = held.lock().unwrap().recv();
        horizon::extension::load_all(files, own)
    });
    let mut shell = None;
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                extensions,
                read_extensions: Some(read_extensions),
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
    // Drawn, with nothing read yet: a pane of the extension says it is
    // being read, and one asked for by name is not there to make.
    assert!(!opened.shell.extensions_read());
    let state = format!("{{path: some({:?})}}", file.to_string_lossy());
    opened.shell.create_plugin(
        PluginPane {
            name: "markdown".to_string(),
            title: "Markdown".to_string(),
            state: serde_json::Value::String(state),
        },
        Placement::NewTab,
    );
    opened.app.run_effects();
    opened.app.resolve();
    assert!(
        opened.text().contains("Reading this view's plugin."),
        "{}",
        opened.text()
    );
    let early = opened.new_markdown(&directory, "first.md");
    let EnvelopeBody::Error(error) = &early else {
        panic!("{early:?}");
    };
    assert!(
        format!("{error:?}").contains("still being read"),
        "{error:?}"
    );

    // Read: the pane is made on what it kept.
    let_go.send(()).unwrap();
    opened.until("the extensions", || opened.shell.extensions_read());
    opened.until("the file", || opened.text().contains("One"));
    assert!(!opened.text().contains("Reading this view's plugin."));
    let _ = std::fs::remove_dir_all(&directory);
}

/// The extensions an answer says are loaded.
fn listed(answer: &EnvelopeBody) -> Vec<(String, String, PathBuf)> {
    let EnvelopeBody::Extensions(extensions) = answer else {
        panic!("the extensions, not {answer:?}");
    };
    (extensions.extensions.iter())
        .map(|entry| {
            (
                entry.name.clone(),
                entry.source.clone(),
                PathBuf::from(&entry.file),
            )
        })
        .collect()
}

#[test]
fn an_extension_is_used_from_its_file_for_now_in_place_of_the_one_of_its_name() {
    let directory = std::env::temp_dir().join(format!("hzb-ext-use-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    // A build of the Markdown view that is not the shell's own: its title
    // tells them apart.
    let tried = directory.join("tried.wasm");
    std::fs::write(&tried, retitled(&component())).unwrap();
    // (The configuration read again names another extension, which says
    // when it has been read.)
    let named = fixture().to_string_lossy().into_owned();
    let reload = Box::new(move || {
        Ok(horizon_config::RawConfig {
            extensions: vec![named.clone()],
            ..horizon_config::RawConfig::default()
        })
    });
    let opened = open_with(Opening {
        reload: Some(reload),
        ..Opening::default()
    });
    let of = |name: &str| {
        (opened.extensions().into_iter())
            .find(|(named, ..)| named == name)
            .map(|(_, source, file)| (source, file))
    };
    // (The chooser lists what is loaded by its title.)
    let titled = |title: &str| {
        opened.shell.execute(CommandId::NewTab);
        opened.app.run_effects();
        opened.app.resolve();
        let listed = opened.text().contains(title);
        opened.key(Key::Named(NamedKey::Escape));
        opened.app.run_effects();
        opened.app.resolve();
        listed
    };
    assert_eq!(of("markdown"), Some(("own".to_string(), component())));
    assert!(titled("Markdown") && !titled("Notebook"));

    // Used: the file's in place of the shell's own, and said so.
    let args = serde_json::json!({ "file": tried.to_string_lossy() });
    let answer = opened.answered("extension-use", args.clone());
    assert!(
        listed(&answer).contains(&(
            "markdown".to_string(),
            "temporary".to_string(),
            tried.clone()
        )),
        "{answer:?}"
    );
    assert_eq!(
        of("markdown"),
        Some(("temporary".to_string(), tried.clone()))
    );
    assert!(titled("Notebook") && !titled("Markdown"));
    assert_eq!(opened.extensions().len(), 6);

    // A file that is no extension is refused, and nothing changes.
    let none = directory.join("none.wasm");
    std::fs::write(&none, b"not a component").unwrap();
    let refused = opened.answered(
        "extension-use",
        serde_json::json!({ "file": none.to_string_lossy() }),
    );
    std::assert_matches!(refused, EnvelopeBody::Error { .. });
    assert_eq!(
        of("markdown"),
        Some(("temporary".to_string(), tried.clone()))
    );

    // The configuration read again leaves it in.
    opened.shell.execute(CommandId::ReloadConfig);
    opened.until("the configuration's extension", || of("fixture").is_some());
    assert_eq!(
        of("fixture").map(|(source, _)| source).as_deref(),
        Some("configured")
    );
    assert_eq!(
        of("markdown"),
        Some(("temporary".to_string(), tried.clone()))
    );

    // Reset by its name: the shell's own again. One that is not in use
    // for now is refused.
    let refused = opened.answered("extension-reset", serde_json::json!({ "name": "wiki" }));
    std::assert_matches!(refused, EnvelopeBody::Error { .. });
    let answer = opened.answered("extension-reset", serde_json::json!({ "name": "markdown" }));
    assert!(
        listed(&answer).contains(&("markdown".to_string(), "own".to_string(), component())),
        "{answer:?}"
    );
    assert!(titled("Markdown") && !titled("Notebook"));

    // Used again, and let go of by the shell's own command.
    opened.answered("extension-use", args);
    assert_eq!(
        of("markdown"),
        Some(("temporary".to_string(), tried.clone()))
    );
    opened.shell.execute(CommandId::ResetExtensions);
    opened.until("the shell's own", || {
        of("markdown") == Some(("own".to_string(), component()))
    });
    let _ = std::fs::remove_dir_all(&directory);
}

/// The fixture extension's component, where cargo builds it.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/wasm32-wasip2/debug/horizon_fixture_plugin.wasm")
}

#[test]
fn an_extension_puts_a_view_over_the_workspace_and_hears_it_taken_away() {
    use horizon::workspace::Modal;

    let opened = open_with(Opening {
        extensions: vec![fixture()],
        ..Opening::default()
    });
    let answer = opened.ask(
        "new",
        serde_json::json!({
            "plugin": "fixture", "arguments": ["over"], "directory": "/", "activate": true,
        }),
    );
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    opened.until("the pane's view", || opened.text().contains("nothing over"));
    let over = || matches!(opened.shell.modal(), Some(Modal::Extension(_)));
    let show = || {
        assert!(opened.shell.execute_plugin("show-over"));
        opened.until("the view over the workspace", || {
            over() && opened.text().contains("over the workspace")
        });
        // (Laid out, it has the keys.)
        opened.app.run_effects();
        opened.app.resolve();
    };

    // The pane's command puts a view over the workspace. Escape, which
    // the view has no use for, leaves it: the view is gone from the
    // page, and the extension hears and lets go of it.
    show();
    opened.key(Key::Named(NamedKey::Escape));
    opened.until("the view left", || !over());
    opened.until("the extension told", || {
        opened.text().contains("taken away 1")
    });
    assert!(
        !opened.text().contains("over the workspace"),
        "{}",
        opened.text()
    );

    // The view has the keys, and takes itself away on one of its own.
    show();
    opened.key(Key::Character("c".into()));
    opened.until("the view left", || !over());
    opened.until("the extension told", || {
        opened.text().contains("taken away 2")
    });
    assert!(
        !opened.text().contains("over the workspace"),
        "{}",
        opened.text()
    );

    // Something else put over the workspace takes its place.
    show();
    opened.shell.execute(CommandId::NewTab);
    opened.app.run_effects();
    opened.app.resolve();
    std::assert_matches!(opened.shell.modal(), Some(Modal::Chooser(_)));
    opened.until("the extension told", || {
        opened.text().contains("taken away 3")
    });
    assert!(
        !opened.text().contains("over the workspace"),
        "{}",
        opened.text()
    );
    opened.key(Key::Named(NamedKey::Escape));
    assert_eq!(opened.shell.modal(), None);

    // The pane gone while its view is over the workspace, the view goes
    // with it.
    show();
    opened.shell.execute(CommandId::CloseActiveTab);
    opened.until("the view left", || !over());
    assert!(
        !opened.text().contains("over the workspace"),
        "{}",
        opened.text()
    );
}

#[test]
fn an_extensions_own_command_is_carried_out_with_no_pane_of_it() {
    use horizon::workspace::Modal;

    let opened = open_with(Opening {
        extensions: vec![fixture()],
        ..Opening::default()
    });
    let run = |name: &str| {
        let answer = opened.ask("plugin-command", serde_json::json!({ "name": name }));
        std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    };
    let over = || matches!(opened.shell.modal(), Some(Modal::Extension(_)));
    let there = || opened.text().contains("over the workspace");
    // (No pane of the fixture: the only pane is the shell's terminal.)
    assert_eq!(opened.model(|model| model.all_pane_ids().len()), 1);

    // A command that leaves nothing over the workspace: what was made to
    // carry it out is let go of.
    run("do-nothing");
    opened.until("let go of", || opened.shell.commanded() == 0);
    assert!(!over());

    // One that puts a view there: what carries it out stays for as long
    // as the view does, and is let go of once the view is left.
    run("show-over-alone");
    opened.until("the view over the workspace", || over() && there());
    assert_eq!(opened.shell.commanded(), 1);
    opened.app.run_effects();
    opened.app.resolve();
    opened.key(Key::Named(NamedKey::Escape));
    opened.until("the view left", || !over() && !there());
    opened.until("let go of", || opened.shell.commanded() == 0);

    // The view takes itself away on a key of its own.
    run("show-over-alone");
    opened.until("the view over the workspace", || over() && there());
    opened.app.run_effects();
    opened.app.resolve();
    opened.key(Key::Character("c".into()));
    opened.until("the view left", || !over() && !there());
    opened.until("let go of", || opened.shell.commanded() == 0);

    // A chord the configuration binds to it carries it out, whatever the
    // cursor's pane shows.
    let config = horizon_config::parse(
        r#"
        [keybindings]
        "Ctrl+Shift+O" = "show-over-alone"
        "#,
    )
    .unwrap();
    opened.shell.apply_config(&config);
    opened.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        text: None,
        key: Key::Character("O".into()),
        code: keyboard_types::Code::Unidentified,
        modifiers: Modifiers::CONTROL | Modifiers::SHIFT,
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
    }));
    opened.until("the view over the workspace", || over() && there());
    opened.app.run_effects();
    opened.app.resolve();
    opened.key(Key::Named(NamedKey::Escape));
    opened.until("the view left", || !over() && !there());
}
