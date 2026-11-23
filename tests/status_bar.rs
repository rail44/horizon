//! The status bar, an extension the shell comes with, at the bottom of the
//! window: what it says where the keys are, and what it leaves out where
//! they do not fit.

use std::path::Path;
use std::time::{Duration, Instant};

use horizon::workspace::{Opening, Shell, Spawn, css, view};
use horizon_control::contract::{EnvelopeBody, Invoke};
use horizon_control::host::executor::ControlRequest;
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_workspace::Workspace;
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

struct Opened {
    app: App,
    shell: Shell,
    root: NodeId,
}

/// A shell with the extensions it comes with, the status bar shown.
fn open() -> Opened {
    let extensions =
        horizon::extension::own_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug"));
    let (mut shell, mut top) = (None, None);
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                extensions,
                ..Opening::default()
            },
        );
        (shell, top) = (Some(built), Some(root.0));
        root
    });
    app.resolve();
    let opened = Opened {
        app,
        shell: shell.unwrap(),
        root: top.unwrap(),
    };
    opened.until("the extensions", || opened.shell.extensions_read());
    opened.until("the status bar", || !opened.all("status-bar").is_empty());
    opened
}

impl Opened {
    fn all(&self, class: &str) -> Vec<NodeId> {
        horizon_ui::testing::find(&self.app, self.root, class)
    }

    /// A node's box: x, y, width, height.
    fn bounds(&self, node: NodeId) -> (f32, f32, f32, f32) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (
            bounds.x as f32,
            bounds.y as f32,
            bounds.width as f32,
            bounds.height as f32,
        )
    }

    /// What the status bar says.
    fn bar(&self) -> String {
        let document = self.app.document();
        let document = document.borrow();
        (self.all("status-bar").into_iter())
            .map(|node| document.get_node(node).unwrap().text_content())
            .collect()
    }

    /// Runs the effects and lays out until `met` holds: what the bar says
    /// is drawn by the effects of a layout.
    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        while !met() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no {what} in {:?}",
                self.bar()
            );
            std::thread::sleep(Duration::from_millis(5));
            self.app.run_effects();
            self.app.resolve();
        }
    }

    fn key(&self, key: Key, modifiers: Modifiers) {
        let text = match &key {
            Key::Character(text) if (modifiers - Modifiers::SHIFT).is_empty() => {
                Some(text.as_str().into())
            }
            _ => None,
        };
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            text,
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers,
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
        }));
        self.app.run_effects();
        self.app.resolve();
    }

    fn character(&self, character: &str) {
        self.key(Key::Character(character.into()), Modifiers::default());
    }

    /// The chord that turns workspace mode on and off.
    fn chord(&self) {
        self.key(Key::Character("'".into()), Modifiers::CONTROL);
    }

    fn mode(&self) -> bool {
        self.shell
            .model
            .with_untracked(Workspace::is_workspace_mode_active)
    }

    /// What the shell answers the control plane's `command`.
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
        body.into_inner().unwrap()
    }

    /// Gives a terminal the keys, as a click on it does.
    fn focus_a_terminal(&self) {
        let pane = (self
            .shell
            .model
            .with_untracked(|model| model.visible_panes()[0].id))
        .to_owned();
        let element = self.shell.pane_element(pane).expect("the pane is drawn");
        let (x, y, width, height) = self.bounds(element);
        horizon_ui::testing::click_at(&self.app, x + width / 2., y + height / 2.);
        self.app.run_effects();
        self.app.resolve();
    }
}

#[test]
fn the_bar_says_the_keys_of_where_the_keys_are() {
    let opened = open();
    opened.focus_a_terminal();
    // A terminal with the focus takes no key of its own: the chords the
    // configuration binds, workspace mode's first.
    opened.until("the chords", || {
        opened.bar().starts_with("ctrl+'workspace mode")
    });
    assert!(
        opened.bar().contains("ctrl+shift+vpaste into terminal"),
        "{}",
        opened.bar()
    );
    // In workspace mode, the mode's, the most wanted first.
    opened.chord();
    assert!(opened.mode());
    opened.until("the mode's keys", || {
        opened.bar().starts_with("hjklmoveentergoescbacktnew tab")
    });
    // Over the workspace, the palette's, and only those.
    opened.character(":");
    opened.until("the palette's keys", || {
        opened.bar() == "↑/↓moveenterrunescclose"
    });
    opened.key(Key::Named(NamedKey::Escape), Modifiers::default());
    opened.until("the mode's keys again", || {
        opened.bar().starts_with("hjklmove")
    });
}

#[test]
fn what_does_not_fit_in_the_bar_is_left_out_whole() {
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};

    let opened = open();
    (opened.app.document().borrow_mut()).set_viewport(Viewport::new(
        400,
        800,
        1.0,
        ColorScheme::Light,
    ));
    opened.app.resolve();
    opened.focus_a_terminal();
    opened.chord();
    opened.until("the mode's keys", || opened.all("status-hint").len() > 3);
    let bar = opened.bounds(opened.all("status-bar")[0]);
    let hints: Vec<_> = (opened.all("status-hint").into_iter())
        .map(|hint| opened.bounds(hint))
        .collect();
    let bottom = bar.1 + bar.3;
    // The first on the bar's one line, the last under it, out of sight:
    // none is cut across the bar's edge.
    assert!(hints[0].1 + hints[0].3 <= bottom, "{hints:?} in {bar:?}");
    let last = hints[hints.len() - 1];
    assert!(last.1 >= bottom, "{hints:?} in {bar:?}");
    assert!(
        hints
            .iter()
            .all(|hint| hint.1 + hint.3 <= bottom || hint.1 >= bottom),
        "{hints:?} in {bar:?}"
    );
    // And the panes are over the bar, not under it.
    let pane = opened.bounds(opened.all("pane")[0]);
    assert!(pane.1 + pane.3 <= bar.1, "{pane:?} over {bar:?}");
}

#[test]
fn the_status_bar_is_the_extension_of_its_name() {
    // The shell's own keys reach the bar through the document, as a
    // view's: none of the bar is the shell's.
    let opened = open();
    opened.focus_a_terminal();
    opened.until("the chords", || opened.bar().starts_with("ctrl+'"));
    let place = opened.all("status-place");
    assert_eq!(place.len(), 1);
    assert_eq!(opened.all("status-bar").len(), 1);
}

#[test]
fn a_status_bar_used_in_place_of_the_shells_is_shown_there() {
    // Another file of the status bar: what `horizon extension use` takes
    // in while the bar is being written.
    let directory = std::env::temp_dir().join(format!("hzb-status-bar-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let tried = directory.join("tried.wasm");
    let built = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/wasm32-wasip2/debug/horizon_status_bar_plugin.wasm");
    std::fs::copy(built, &tried).unwrap();

    let opened = open();
    opened.focus_a_terminal();
    opened.until("the chords", || opened.bar().starts_with("ctrl+'"));
    let before = opened.all("status-bar");
    let answer = opened.answered(
        "extension-use",
        serde_json::json!({ "file": tried.to_string_lossy() }),
    );
    std::assert_matches!(answer, EnvelopeBody::Extensions(_), "{answer:?}");
    // The bar shown is the one used, in place of the shell's: one bar, made
    // again, saying what the shell's did.
    opened.until("the bar made again", || {
        let now = opened.all("status-bar");
        now.len() == 1 && now != before && opened.bar().starts_with("ctrl+'")
    });
    std::fs::remove_dir_all(&directory).unwrap();
}
