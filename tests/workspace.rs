//! The shell around the views, over `horizon-workspace`'s model with a
//! real shell in each terminal pane: splits, the mode's cursor, tabs, the
//! palette, a boundary dragged, and what closing and exiting leave.

use std::time::{Duration, Instant};

use horizon::workspace::{Modal, Opening, Shell, Spawn, css, view};
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_ui::testing::{Pointer, click_at, pointer};
use horizon_workspace::{PaneId, SessionId, Workspace};
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

struct Opened {
    app: App,
    shell: Shell,
}

fn open() -> Opened {
    open_with(Opening::default())
}

fn open_with(opening: Opening) -> Opened {
    let mut shell = None;
    let app = App::new(1200, 800, css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(spawn, opening);
        shell = Some(built);
        root
    });
    app.resolve();
    Opened {
        app,
        shell: shell.unwrap(),
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

impl Opened {
    fn model<R>(&self, read: impl FnOnce(&Workspace) -> R) -> R {
        self.shell.model.with_untracked(read)
    }

    /// The active tab's panes, in the order of their places.
    fn panes(&self) -> Vec<PaneId> {
        self.model(|model| model.visible_panes().iter().map(|pane| pane.id).collect())
    }

    fn sessions(&self) -> usize {
        self.model(|model| model.session_summaries().len())
    }

    /// Closes a pane as `x` on it does.
    fn model_closing(&self, pane: PaneId) {
        self.chord();
        self.shell.model.update(|model| {
            model.commit_workspace_mode_to(pane);
        });
        self.chord();
        self.character("x");
    }

    fn mode(&self) -> bool {
        self.model(Workspace::is_workspace_mode_active)
    }

    /// The elements of a class, in document order.
    fn all(&self, class: &str) -> Vec<NodeId> {
        let document = self.app.document();
        let document = document.borrow();
        let mut found = Vec::new();
        collect(&document, document.root_element().id, class, &mut found);
        found
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

    fn middle(&self, node: NodeId) -> (f32, f32) {
        let (x, y, width, height) = self.bounds(node);
        (x + width / 2., y + height / 2.)
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
        self.app.resolve();
    }

    fn character(&self, character: &str) {
        self.key(Key::Character(character.into()), Modifiers::default());
    }

    fn named(&self, key: NamedKey) {
        self.key(Key::Named(key), Modifiers::default());
    }

    /// The chord that turns workspace mode on and off.
    fn chord(&self) {
        self.key(Key::Character("'".into()), Modifiers::CONTROL);
    }

    /// Text as an input method commits it into the focused field.
    fn commit(&self, text: &str) {
        self.app
            .dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        self.app.resolve();
    }

    /// Types a command into whatever has the focus, and Enter.
    fn run(&self, command: &str) {
        for character in command.chars() {
            self.character(&character.to_string());
        }
        self.named(NamedKey::Enter);
    }

    fn text(&self, pane: PaneId) -> String {
        self.shell.terminal(pane).expect("a terminal").text()
    }

    /// Lays out (which takes the sessions' frames in) until `met` holds.
    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        while !met() {
            assert!(started.elapsed() < Duration::from_secs(10), "no {what}");
            std::thread::sleep(Duration::from_millis(5));
            self.app.resolve();
        }
    }

    fn wait(&self, pane: PaneId, what: &str, met: impl Fn(&str) -> bool) {
        self.until(what, || met(&self.text(pane)));
    }

    fn prompt(&self, pane: PaneId) {
        self.wait(pane, "a prompt", |text| text.trim_end().ends_with('$'));
    }
}

#[test]
fn a_split_puts_a_second_terminal_beside_the_first_and_the_keys_go_to_it() {
    let opened = open();
    let first = opened.panes()[0];
    opened.prompt(first);

    opened.chord();
    assert!(opened.mode());
    opened.character("s");
    assert!(!opened.mode());
    let panes = opened.panes();
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0], first);
    let boxes: Vec<_> = (opened.all("pane").into_iter())
        .map(|pane| opened.bounds(pane))
        .collect();
    assert_eq!(boxes.len(), 2);
    let (left, right) = (boxes[0], boxes[1]);
    assert!(left.0 + left.2 <= right.0, "{left:?} {right:?}");
    assert!((left.2 - right.2).abs() < 1., "{left:?} {right:?}");
    assert_eq!(left.1, right.1);

    // The new pane has the focus.
    opened.prompt(panes[1]);
    opened.run("echo second-$((1+1))");
    opened.wait(panes[1], "the output", |text| text.contains("second-2"));
    assert!(!opened.text(first).contains("second"));
}

#[test]
fn the_modes_cursor_moves_between_panes_and_x_closes_the_one_it_is_on() {
    let opened = open();
    opened.chord();
    opened.character("s");
    let panes = opened.panes();

    // The cursor starts on the pane with the focus, and moves alone.
    opened.chord();
    let elements = opened.all("pane");
    assert_eq!(opened.all("cursor"), vec![elements[1]]);
    opened.character("h");
    assert_eq!(opened.all("cursor"), vec![elements[0]]);
    assert_eq!(opened.all("veil").len(), 2);
    assert!(opened.model(|model| model.is_active_pane(panes[1])));

    opened.character("x");
    assert!(!opened.mode());
    assert_eq!(opened.panes(), vec![panes[1]]);
    assert_eq!(opened.all("pane"), vec![elements[1]]);
    // Closing detached the session shown there: it goes on.
    assert_eq!(opened.sessions(), 2);
    assert_eq!(opened.model(Workspace::detached_session_count), 1);

    // The pane that stays has the keys.
    opened.prompt(panes[1]);
    opened.run("echo stayed");
    opened.wait(panes[1], "the output", |text| {
        text.lines().any(|line| line == "stayed")
    });

    // Ending the detached session is a command of its own.
    opened.chord();
    opened.character(":");
    opened.commit("terminate all");
    opened.named(NamedKey::Enter);
    assert_eq!(opened.sessions(), 1);
}

#[test]
fn tabs_open_cycle_and_are_picked_from_the_strip() {
    let opened = open();
    let first = opened.all("pane");
    opened.chord();
    opened.character("t");
    assert!(!opened.mode());
    assert_eq!(opened.model(Workspace::tab_count), 2);
    assert_eq!(opened.model(Workspace::active_tab_index), 1);
    let labels = opened.all("tab-label");
    assert_eq!(labels.len(), 2);
    assert_eq!(opened.all("active"), vec![labels[1]]);
    // The tab shown is the new one: the first tab's pane is off the page.
    let second = opened.all("pane");
    assert_eq!(second.len(), 1);
    assert_ne!(second, first);

    // Tab and Shift+Tab go around the strip, and the mode stays on.
    opened.chord();
    opened.named(NamedKey::Tab);
    assert_eq!(opened.model(Workspace::active_tab_index), 0);
    assert_eq!(opened.all("pane"), first);
    opened.key(Key::Named(NamedKey::Tab), Modifiers::SHIFT);
    assert_eq!(opened.model(Workspace::active_tab_index), 1);
    assert!(opened.mode());
    opened.named(NamedKey::Escape);
    assert!(!opened.mode());

    let (x, y) = opened.middle(labels[0]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(opened.model(Workspace::active_tab_index), 0);
    assert_eq!(opened.all("active"), vec![labels[0]]);
    // The first tab's terminal is the one it had, and has the keys.
    let pane = opened.panes()[0];
    opened.prompt(pane);
    opened.run("echo back");
    opened.wait(pane, "the output", |text| {
        text.lines().any(|line| line == "back")
    });
}

#[test]
fn the_palette_runs_the_command_chosen_from_what_is_typed() {
    let opened = open();
    opened.chord();
    opened.character(":");
    assert!(opened.shell.modal() == Some(Modal::Palette));
    // The mode stays on under the palette, and Escape goes back to it.
    opened.named(NamedKey::Escape);
    assert_eq!(opened.shell.modal(), None);
    assert!(opened.mode());

    opened.character(":");
    let every = opened.all("item").len();
    assert!(every > 4, "{every} commands");
    // Some cannot be run as things are: a lone pane is not closed from
    // the palette, no session is detached to end, no agent waits.
    let disabled = opened.all("disabled").len();
    assert!((3..every).contains(&disabled), "{disabled} of {every}");
    opened.commit("split d");
    assert_eq!(opened.all("item").len(), 1);
    opened.named(NamedKey::Enter);
    assert_eq!(opened.shell.modal(), None);
    assert!(!opened.mode());
    let boxes: Vec<_> = (opened.all("pane").into_iter())
        .map(|pane| opened.bounds(pane))
        .collect();
    assert_eq!(boxes.len(), 2);
    let (top, bottom) = (boxes[0], boxes[1]);
    assert!(top.1 + top.3 <= bottom.1, "{top:?} {bottom:?}");
    assert_eq!(top.0, bottom.0);
}

#[test]
fn a_boundary_dragged_changes_the_panes_shares_and_builds_nothing() {
    let opened = open();
    opened.chord();
    opened.character("s");
    let panes = opened.all("pane");
    let before = opened.bounds(panes[0]);

    let (x, y) = opened.middle(opened.all("divider")[0]);
    pointer(&opened.app, x, y, Pointer::Move);
    pointer(&opened.app, x, y, Pointer::Down);
    pointer(&opened.app, 500., y + 30., Pointer::Drag);
    pointer(&opened.app, 400., y + 60., Pointer::Drag);
    pointer(&opened.app, 400., y + 60., Pointer::Up);
    opened.app.resolve();
    let after = opened.bounds(panes[0]);
    assert!((before.2 - 597.5).abs() < 1., "{before:?}");
    assert!((after.2 - 400.).abs() < 4., "{after:?}");
    assert_eq!(opened.all("pane"), panes);
    // Released: a further move does nothing.
    pointer(&opened.app, 700., y, Pointer::Move);
    opened.app.resolve();
    assert_eq!(opened.bounds(panes[0]), after);
}

#[test]
fn a_shell_that_exits_takes_its_pane_and_the_last_leaves_an_empty_workspace() {
    let opened = open();
    let pane = opened.panes()[0];
    opened.prompt(pane);
    opened.run("exit");
    opened.until("an empty workspace", || {
        opened.model(Workspace::tab_count) == 0
    });
    assert_eq!(opened.sessions(), 0);
    assert!(opened.all("pane").is_empty());
    assert_eq!(opened.all("empty").len(), 1);

    // With no pane to take the keys, the palette opens without the chord.
    opened.character(":");
    assert!(opened.shell.modal() == Some(Modal::Palette));
    opened.commit("new tab");
    opened.named(NamedKey::Enter);
    assert_eq!(opened.model(Workspace::tab_count), 1);
    assert!(opened.all("empty").is_empty());
    let pane = opened.panes()[0];
    opened.prompt(pane);
    opened.run("echo again");
    opened.wait(pane, "the output", |text| {
        text.lines().any(|line| line == "again")
    });
}

#[test]
fn the_session_manager_puts_a_detached_session_back_and_ends_one() {
    let opened = open();
    let first = opened.panes()[0];
    opened.prompt(first);
    opened.run("echo kept-$((2+3))");
    opened.wait(first, "the output", |text| text.contains("kept-5"));
    let kept = opened.model(|model| model.terminal_session_id(first).unwrap());

    // A second pane, and the first closed: its session is detached.
    opened.chord();
    opened.character("s");
    opened.chord();
    opened.character("h");
    opened.character("x");
    assert_eq!(opened.model(Workspace::detached_session_count), 1);

    // The manager lists both, the detached one told apart.
    opened.chord();
    opened.character(":");
    opened.commit("manage");
    opened.named(NamedKey::Enter);
    assert_eq!(opened.shell.modal(), Some(Modal::Sessions));
    assert_eq!(opened.all("session").len(), 2);
    assert_eq!(opened.all("detached").len(), 1);

    // Enter on the detached session puts it in a pane beside the active
    // one, as it was left.
    opened.commit("#1");
    assert_eq!(opened.all("session").len(), 1);
    opened.named(NamedKey::Enter);
    assert_eq!(opened.shell.modal(), None);
    assert!(!opened.mode());
    assert_eq!(opened.model(Workspace::detached_session_count), 0);
    let panes = opened.panes();
    assert_eq!(panes.len(), 2);
    let back = (panes.iter().copied())
        .find(|pane| opened.model(|model| model.terminal_session_id(*pane)) == Some(kept))
        .expect("the session's pane");
    opened.wait(back, "what it showed", |text| text.contains("kept-5"));
    // It has the keys.
    opened.run("echo back-$((4+4))");
    opened.wait(back, "the output", |text| text.contains("back-8"));

    // Ctrl+Enter ends the chosen session, and the list stays.
    opened.chord();
    opened.character(":");
    opened.commit("manage");
    opened.named(NamedKey::Enter);
    opened.key(Key::Named(NamedKey::Enter), Modifiers::CONTROL);
    assert_eq!(opened.shell.modal(), Some(Modal::Sessions));
    assert_eq!(opened.sessions(), 1);
    assert_eq!(opened.all("session").len(), 1);
    assert_eq!(opened.panes().len(), 1);
    opened.named(NamedKey::Escape);
    assert_eq!(opened.shell.modal(), None);
}

#[test]
fn the_configurations_chords_run_commands_and_replace_the_modes() {
    let opened = open();
    let config = horizon_config::parse(
        r#"
        [keybindings]
        "Ctrl+Shift+D" = "split-down"
        "Ctrl+Shift+P" = "open-palette"
        "Alt+M" = "workspace-mode"
        "#,
    )
    .unwrap();
    opened.shell.apply_config(&config);

    // A bound chord runs its command with no mode on, and is not typed
    // into the terminal.
    let pane = opened.panes()[0];
    opened.prompt(pane);
    opened.key(
        Key::Character("D".into()),
        Modifiers::CONTROL | Modifiers::SHIFT,
    );
    let boxes: Vec<_> = (opened.all("pane").into_iter())
        .map(|pane| opened.bounds(pane))
        .collect();
    assert_eq!(boxes.len(), 2);
    assert!(boxes[0].1 + boxes[0].3 <= boxes[1].1, "{boxes:?}");

    opened.key(
        Key::Character("P".into()),
        Modifiers::CONTROL | Modifiers::SHIFT,
    );
    assert_eq!(opened.shell.modal(), Some(Modal::Palette));
    opened.named(NamedKey::Escape);

    // The mode's chord is the configured one, and no longer the built-in.
    opened.chord();
    assert!(!opened.mode());
    opened.key(Key::Character("m".into()), Modifiers::ALT);
    assert!(opened.mode());
}

#[test]
fn the_font_size_moves_and_a_reload_takes_the_configuration_in() {
    let reload = || {
        horizon_config::parse(
            r#"
            [terminal]
            font_size = 16.0
            [keybindings]
            "Alt+M" = "workspace-mode"
            "#,
        )
    };
    let opened = open_with(Opening {
        reload: Some(Box::new(reload)),
        ..Opening::default()
    });
    let pane = opened.panes()[0];
    opened.prompt(pane);
    let font_size = || opened.shell.theme.with_untracked(|theme| theme.font_size);
    let columns = || opened.shell.terminal(pane).unwrap().size().0;
    let at_thirteen = columns();

    // A step up: larger cells, so fewer of them across the same pane.
    opened.key(Key::Character("=".into()), Modifiers::CONTROL);
    assert_eq!(font_size(), 14.);
    opened.until("fewer columns", || columns() < at_thirteen);
    opened.key(Key::Character("-".into()), Modifiers::CONTROL);
    opened.key(Key::Character("-".into()), Modifiers::CONTROL);
    assert_eq!(font_size(), 12.);
    opened.until("more columns", || columns() > at_thirteen);
    opened.key(Key::Character("0".into()), Modifiers::CONTROL);
    assert_eq!(font_size(), 13.);
    opened.until("the columns it had", || columns() == at_thirteen);

    // Reload Config: the file's font size and chords, in place of those
    // in use.
    opened.chord();
    opened.character(":");
    opened.commit("reload config");
    opened.named(NamedKey::Enter);
    assert!(!opened.mode());
    assert_eq!(font_size(), 16.);
    opened.until("fewer columns", || columns() < at_thirteen);
    opened.chord();
    assert!(!opened.mode());
    opened.key(Key::Character("m".into()), Modifiers::ALT);
    assert!(opened.mode());
    opened.named(NamedKey::Escape);
    // What Reset goes back to is the file's size now.
    opened.key(Key::Character("=".into()), Modifiers::CONTROL);
    opened.key(Key::Character("0".into()), Modifiers::CONTROL);
    assert_eq!(font_size(), 16.);
}

#[test]
fn a_tabs_close_button_closes_it_and_keeps_its_session() {
    let opened = open();
    opened.chord();
    opened.character("t");
    assert_eq!(opened.model(Workspace::tab_count), 2);
    assert_eq!(opened.model(Workspace::active_tab_index), 1);

    // The first tab's, while the second is the one shown.
    let (x, y) = opened.middle(opened.all("close")[0]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(opened.model(Workspace::tab_count), 1);
    assert_eq!(opened.all("tab-label").len(), 1);
    assert_eq!(opened.sessions(), 2);
    assert_eq!(opened.model(Workspace::detached_session_count), 1);
    // The tab that stays is the one that was shown, with the keys.
    let pane = opened.panes()[0];
    opened.prompt(pane);
    opened.run("echo here");
    opened.wait(pane, "the output", |text| {
        text.lines().any(|line| line == "here")
    });
}

#[test]
fn the_control_plane_makes_lists_attaches_and_ends_sessions() {
    use horizon_control::contract::{EnvelopeBody, Invoke, Query};
    use horizon_control::host::executor::{ControlExecutor, ControlRequest};

    let opened = open();
    let (asking, requests) = horizon::control::bridge();
    // Asked from another thread, as the socket's listener asks, and
    // answered on this one at each poll.
    let ask = |request: ControlRequest| -> EnvelopeBody {
        std::thread::scope(|scope| {
            let asked = scope.spawn(|| asking.execute(request));
            while !asked.is_finished() {
                requests.answer(std::task::Waker::noop(), &opened.shell);
                opened.app.resolve();
                std::thread::sleep(Duration::from_millis(1));
            }
            asked.join().unwrap()
        })
    };
    let invoke = |command: &str, args: serde_json::Value| {
        ask(ControlRequest::Invoke(Invoke {
            command: command.to_string(),
            args,
        }))
    };
    let first = opened.panes()[0];
    let session = opened.model(|model| model.terminal_session_id(first).unwrap());
    let id = |session: SessionId| session.as_uuid().to_string();

    // A terminal beside the first, without taking the focus.
    let made = invoke("new-terminal", serde_json::json!({ "split": id(session) }));
    let EnvelopeBody::Ok {
        session_id: Some(made),
    } = made
    else {
        panic!("{made:?}");
    };
    let panes = opened.panes();
    assert_eq!(panes.len(), 2);
    assert!(opened.model(|model| model.is_active_pane(first)));
    assert_eq!(
        opened.model(|model| model.terminal_session_id(panes[1]).map(id)),
        Some(made.clone())
    );
    opened.prompt(panes[1]);

    let EnvelopeBody::Sessions(listed) = ask(ControlRequest::Query(Query {
        what: "sessions".to_string(),
    })) else {
        panic!("no sessions");
    };
    assert_eq!(listed.sessions.len(), 2);
    assert!(listed.sessions.iter().all(|session| session.attached));

    // Closed, the new one is detached; `attach` puts it back, and
    // `terminate-session` ends it.
    opened.model_closing(panes[1]);
    assert_eq!(opened.model(Workspace::detached_session_count), 1);
    let attached = invoke(
        "attach",
        serde_json::json!({ "session_id": made, "activate": true }),
    );
    assert!(
        matches!(attached, EnvelopeBody::Ok { session_id: None }),
        "{attached:?}"
    );
    assert_eq!(opened.panes().len(), 2);
    assert_eq!(opened.model(Workspace::detached_session_count), 0);
    let ended = invoke(
        "terminate-session",
        serde_json::json!({ "session_id": made }),
    );
    assert!(matches!(ended, EnvelopeBody::Ok { .. }), "{ended:?}");
    assert_eq!(opened.panes(), vec![first]);
    assert_eq!(opened.sessions(), 1);

    // What is not a command, and what this shell has no runtime for.
    for (command, args) in [
        ("no-such-command", serde_json::json!({})),
        ("new-agent", serde_json::json!({})),
        (
            "terminate-session",
            serde_json::json!({ "session_id": made }),
        ),
    ] {
        let answer = invoke(command, args);
        assert!(
            matches!(answer, EnvelopeBody::Error(_)),
            "{command}: {answer:?}"
        );
    }
}

#[test]
fn the_control_plane_opens_a_markdown_file_in_a_pane_that_follows_its_links() {
    use horizon_control::contract::{EnvelopeBody, Invoke};
    use horizon_control::host::executor::ControlRequest;

    let directory = std::env::temp_dir().join(format!("hzb-md-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let (first, second) = (directory.join("first.md"), directory.join("second.md"));
    std::fs::write(&first, "# One\n\n[to the second](second.md)\n").unwrap();
    std::fs::write(&second, "# Two\n").unwrap();

    let opened = open();
    // As at a poll of the page: the request answered, then the effects of
    // what it changed.
    let open_file = |path: &std::path::Path| {
        let answer = opened.shell.control(&ControlRequest::Invoke(Invoke {
            command: "markdown".to_string(),
            args: serde_json::json!({ "path": path, "activate": true }),
        }));
        opened.app.run_effects();
        opened.app.resolve();
        answer
    };
    let answer = open_file(&first);
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    assert_eq!(opened.model(Workspace::tab_count), 2);
    let pane = opened.panes()[0];
    let shown = |opened: &Opened| opened.model(|model| shown_file(model, pane));
    assert_eq!(shown(&opened), Some(first.clone()));
    // The tab is named for the file.
    let title = |opened: &Opened| opened.model(|model| model.tab_summaries()[1].title.clone());
    assert_eq!(title(&opened), "first.md");

    // A link followed moves the pane's file in the model with it.
    let (x, y) = opened.middle(opened.all("link")[0]);
    click_at(&opened.app, x, y);
    opened.app.resolve();
    assert_eq!(shown(&opened), Some(second.clone()));
    assert_eq!(title(&opened), "second.md");

    // Asked for a file a pane shows already, that pane is the one.
    let answer = open_file(&second);
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    assert_eq!(opened.model(Workspace::tab_count), 2);
    let missing = open_file(&directory.join("none.md"));
    assert!(matches!(missing, EnvelopeBody::Error(_)), "{missing:?}");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn the_board_opens_in_a_tab_and_a_task_s_thread_beside_its_list() {
    use horizon_board::{BoardEvent, Item, Store, sample_envelopes};
    use horizon_workspace::PaneKind;

    let tasks = [(1, "first", "a"), (2, "second", "b")].map(|(id, title, rank)| Item {
        id,
        title: title.to_string(),
        rank: rank.to_string(),
        ..Item::default()
    });
    let events = tasks
        .into_iter()
        .map(|item| BoardEvent::ItemStored { id: item.id, item });
    let opened = open_with(Opening {
        board: Some(horizon::board::Source::Ready(Store::in_memory(
            sample_envelopes(events),
        ))),
        ..Opening::default()
    });
    let kinds = |opened: &Opened| -> Vec<PaneKind> {
        opened.model(|model| model.visible_panes().iter().map(|pane| pane.kind).collect())
    };
    let plugins = |opened: &Opened| -> Vec<_> {
        (opened.panes().into_iter())
            .map(|pane| opened.shell.plugin(pane))
            .collect()
    };
    let task = |opened: &Opened, pane| {
        opened.model(|model| model.pane_plugin(pane).map(|plugin| plugin.state.clone()))
    };

    // The board's list, in a tab of its own, with the keys.
    opened.chord();
    opened.character(":");
    opened.commit("open board");
    opened.named(NamedKey::Enter);
    assert_eq!(opened.model(Workspace::tab_count), 2);
    assert_eq!(kinds(&opened), [PaneKind::View]);
    assert_eq!(plugins(&opened), [Some("board-list")]);
    let list_pane = opened.panes()[0];
    opened.until("the tasks", || {
        horizon_ui::testing::text(&opened.app).contains("second")
    });

    // Enter on a task: its thread, beside the list, with the focus.
    opened.named(NamedKey::Enter);
    assert_eq!(plugins(&opened), [Some("board-list"), Some("board-thread")]);
    let thread_pane = opened.panes()[1];
    assert_eq!(
        task(&opened, thread_pane),
        Some(serde_json::json!({"task": 1}))
    );
    assert!(opened.model(|model| model.is_active_pane(thread_pane)));
    opened.until("the task", || opened.all("actions").len() == 1);

    // The palette lists the commands of the plugin the cursor's pane
    // shows, and not another plugin's.
    opened.chord();
    opened.character(":");
    opened.commit("add board task");
    assert_eq!(opened.all("item").len(), 0);
    opened.named(NamedKey::Escape);
    // Back to the list from it, and the next task's thread takes the
    // same pane.
    opened.character(":");
    opened.commit("back to task list");
    assert_eq!(opened.all("item").len(), 1);
    opened.named(NamedKey::Enter);
    opened.app.run_effects();
    opened.app.resolve();
    assert!(!opened.mode());
    assert!(opened.model(|model| model.is_active_pane(list_pane)));
    opened.character("j");
    opened.named(NamedKey::Enter);
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(opened.panes(), vec![list_pane, thread_pane]);
    assert_eq!(
        task(&opened, thread_pane),
        Some(serde_json::json!({"task": 2}))
    );
    assert_eq!(plugins(&opened), [Some("board-list"), Some("board-thread")]);
    assert!(opened.model(|model| model.is_active_pane(thread_pane)));
    opened.until("the thread, made again", || {
        opened.all("actions").len() == 1
    });
}

#[test]
fn the_theme_settings_apply_a_seed_as_it_changes_and_save_it_when_asked() {
    use horizon::workspace::Placement;
    use horizon_workspace::PluginPane;
    use std::cell::RefCell;
    use std::rc::Rc;

    let saved = Rc::new(RefCell::new(Vec::new()));
    let log = saved.clone();
    let opened = open_with(Opening {
        save_theme: Some(Rc::new(move |seed: &horizon_theme::seed::Seed| {
            log.borrow_mut().push(seed.accent_config_value());
            Ok(std::path::PathBuf::from("/somewhere/config.toml"))
        })),
        ..Opening::default()
    });
    let scheme = |opened: &Opened| opened.shell.theme.with_untracked(|theme| *theme.scheme());
    let before = scheme(&opened);
    let settings = PluginPane {
        name: "theme-settings".to_string(),
        title: "Theme Settings".to_string(),
        state: serde_json::Value::Null,
    };
    opened.shell.create_plugin(settings, Placement::NewTab);
    opened.app.run_effects();
    opened.app.resolve();
    // The derived colors, as swatches: sixteen, three, five, four, four.
    assert_eq!(opened.all("chip").len(), 32);
    assert!(opened.all("unsaved").is_empty());

    // The accent, chosen by name: the theme is drawn with that hue at
    // once, and nothing is written.
    let click = |class: &str, index: usize| {
        let (x, y) = opened.middle(opened.all(class)[index]);
        click_at(&opened.app, x, y);
        opened.app.resolve();
    };
    click("trigger", 0);
    click("option", 0);
    let after = scheme(&opened);
    assert_ne!(after.accent, before.accent);
    assert_eq!(opened.all("unsaved").len(), 1);
    assert!(saved.borrow().is_empty());

    // The text contrast, from the keys of its slider.
    click("track", 0);
    opened.named(NamedKey::Home);
    let low = scheme(&opened).foreground;
    opened.named(NamedKey::End);
    assert_ne!(scheme(&opened).foreground, low);

    // Save writes the seed, and says where.
    click("save", 0);
    assert_eq!(*saved.borrow(), ["red"]);
    assert!(opened.all("unsaved").is_empty());
    let status = opened.all("status");
    assert_eq!(status.len(), 1);
}

#[test]
fn the_control_plane_opens_a_wiki_page_in_a_pane() {
    use horizon_control::contract::{EnvelopeBody, Invoke};
    use horizon_control::host::executor::ControlRequest;

    let directory = std::env::temp_dir().join(format!("hzb-wiki-{}", std::process::id()));
    let store = horizon_wiki::Store::at(directory.join("project"));
    store
        .write_page("index", "---\ntitle: Overview\n---\nWhat the project is.\n")
        .unwrap();
    let page = store.page_path("index").unwrap();

    let opened = open();
    let answer = opened.shell.control(&ControlRequest::Invoke(Invoke {
        command: "wiki".to_string(),
        args: serde_json::json!({ "path": page, "activate": true }),
    }));
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    opened.app.run_effects();
    opened.app.resolve();
    let pane = opened.panes()[0];
    assert_eq!(opened.shell.plugin(pane), Some("wiki"));
    assert_eq!(
        opened.model(|model| shown_file(model, pane)),
        Some(page.clone())
    );
    opened.until("the page", || opened.all("heading").len() == 1);
    // The tab is named for the page's file, and the pane comes back on it.
    assert_eq!(
        opened.model(|model| model.tab_summaries()[1].title.clone()),
        "index.md"
    );
    assert_eq!(opened.model(|model| shown_file(model, pane)), Some(page));
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn the_control_plane_switches_a_sessions_model_through_the_runtime() {
    use std::sync::{Arc, Mutex, mpsc};

    use horizon::workspace::Models;
    use horizon_control::contract::{EnvelopeBody, Invoke};
    use horizon_control::host::executor::ControlRequest;

    #[derive(Default)]
    struct Switched(Mutex<Vec<(SessionId, String, String)>>);
    impl Models for Switched {
        fn providers(&self) -> Result<Vec<horizon_acp::ProviderSummary>, String> {
            Ok(Vec::new())
        }
        fn models(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
        fn switch(&self, session: SessionId, provider: &str, model: &str) -> Result<(), String> {
            if model == "gone" {
                return Err("no such model".to_string());
            }
            (self.0.lock().unwrap()).push((session, provider.to_string(), model.to_string()));
            Ok(())
        }
    }

    let switched = Arc::new(Switched::default());
    let opened = open_with(Opening {
        models: Some(switched.clone()),
        ..Opening::default()
    });
    let session = SessionId::new();
    let ask = |model: &str| {
        let (reply, answer) = mpsc::channel();
        opened.shell.control_answering(
            &ControlRequest::Invoke(Invoke {
                command: "set-model".to_string(),
                args: serde_json::json!({
                    "session_id": session.as_uuid().to_string(),
                    "provider": "local",
                    "model": model,
                }),
            }),
            move |body| reply.send(body).unwrap(),
        );
        answer
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("an answer")
    };
    let answer = ask("small");
    assert!(matches!(answer, EnvelopeBody::Ok { .. }), "{answer:?}");
    assert_eq!(
        *switched.0.lock().unwrap(),
        [(session, "local".to_string(), "small".to_string())]
    );
    // What the runtime refuses is the answer.
    let answer = ask("gone");
    assert!(
        matches!(&answer, EnvelopeBody::Error(error) if error.message.contains("no such model")),
        "{answer:?}"
    );
}

#[test]
fn a_notification_a_program_asks_for_is_posted_unless_its_pane_is_looked_at() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let posted = Rc::new(RefCell::new(Vec::new()));
    let log = posted.clone();
    let opened = open_with(Opening {
        notify: Some(Rc::new(move |session, notification| {
            (log.borrow_mut()).push((session, notification.title, notification.body));
        })),
        ..Opening::default()
    });
    let first = opened.panes()[0];
    let session = opened.model(|model| model.terminal_session_id(first).unwrap());
    opened.prompt(first);

    // From the pane that has the keys, in a window that has the focus:
    // the owner is looking at it, and nothing is posted.
    opened.run(r"printf '\033]9;seen\007'; echo told-$((1+1))");
    opened.wait(first, "the command", |text| text.contains("told-2"));
    assert!(posted.borrow().is_empty(), "{:?}", posted.borrow());

    // From a pane that no longer has them: posted, for its session.
    opened.run(r"sleep 0.4; printf '\033]777;notify;Build;done\007'");
    opened.chord();
    opened.character("s");
    let second = opened.panes()[1];
    assert!(opened.model(|model| model.is_active_pane(second)));
    opened.until("the notification", || !posted.borrow().is_empty());
    assert_eq!(
        *posted.borrow(),
        [(session, Some("Build".to_string()), "done".to_string())]
    );

    // With the window away, also from the pane that has the keys.
    opened.prompt(second);
    horizon::window::set_active(false);
    opened.run(r"printf '\033]9;away\007'");
    opened.until("the second notification", || posted.borrow().len() == 2);
    let (from, title, body) = posted.borrow()[1].clone();
    assert_eq!(
        opened.model(|model| model.terminal_session_id(second)),
        Some(from)
    );
    assert_eq!((title, body.as_str()), (None, "away"));
    horizon::window::set_active(true);

    // A click on a banner brings its session's pane to the front.
    opened.shell.reveal_session(session);
    opened.app.run_effects();
    opened.app.resolve();
    assert!(opened.model(|model| model.is_active_pane(first)));
}

/// How many pixels of the page inside a node's box are not `background`.
fn inked(opened: &Opened, node: NodeId, background: [u8; 3]) -> usize {
    use anyrender::ImageRenderer as _;

    let (width, height) = (1200, 800);
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    let (x, y, wide, tall) = opened.bounds(node);
    let mut inked = 0;
    for row in (y as usize + 2)..((y + tall) as usize - 2) {
        for column in (x as usize + 2)..((x + wide) as usize - 2) {
            let at = (row * width as usize + column) * 4;
            if [pixels[at], pixels[at + 1], pixels[at + 2]] != background {
                inked += 1;
            }
        }
    }
    inked
}

#[test]
fn a_terminal_that_was_there_before_a_split_is_still_drawn_after_it() {
    let opened = open();
    let first = opened.panes()[0];
    opened.prompt(first);
    opened.run("echo before-the-split");
    opened.wait(first, "the output", |text| {
        text.contains("before-the-split")
    });
    let background = opened
        .shell
        .theme
        .with_untracked(|theme| theme.terminal().background);
    let screen = |at: usize| opened.all("terminal")[at];
    assert!(inked(&opened, screen(0), background) > 50, "drawn before");

    opened.chord();
    opened.character("s");
    let second = opened.panes()[1];
    opened.prompt(second);
    opened.wait(first, "what it showed", |text| {
        text.contains("before-the-split")
    });
    opened.app.resolve();
    assert_eq!(opened.all("terminal").len(), 2);
    let (left, right) = (
        inked(&opened, screen(0), background),
        inked(&opened, screen(1), background),
    );
    assert!(right > 5, "the new terminal is drawn: {right}");
    assert!(left > 50, "the first terminal is still drawn: {left}");
}

#[test]
fn block_art_has_no_seams_in_either_pane_of_a_split_at_a_fractional_scale() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};

    let blocks = r"clear; printf '\342\226\210\342\226\210\342\226\210\342\226\210\n\342\226\210\342\226\210\342\226\210\342\226\210\n\342\226\210\342\226\210\342\226\210\342\226\210\n'";
    for scale in [1.25f32, 1.5, 1.75, 2.] {
        let opened = open();
        {
            let document = opened.app.document();
            (document.borrow_mut()).set_viewport(Viewport::new(
                1200,
                800,
                scale,
                ColorScheme::Light,
            ));
        }
        opened.app.resolve();
        let first = opened.panes()[0];
        opened.prompt(first);
        opened.run(blocks);
        opened.wait(first, "the blocks", |text| text.contains("████"));
        opened.chord();
        opened.character("s");
        let second = opened.panes()[1];
        opened.prompt(second);
        opened.run(blocks);
        opened.wait(second, "the blocks", |text| text.contains("████"));
        opened.app.resolve();

        let (width, height) = (1200u32, 800u32);
        let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
        let mut pixels = Vec::new();
        let document = opened.app.document();
        renderer.render_to_vec(
            |scene| {
                let mut document = document.borrow_mut();
                blitz_paint::paint_scene(scene, &mut document, scale as f64, width, height, 0, 0)
            },
            &mut pixels,
        );
        if let Some(path) = std::env::var_os("HORIZON_TEST_DUMP") {
            let path = format!("{}-{scale}", path.to_string_lossy());
            std::fs::write(path, &pixels).unwrap();
        }
        let foreground = opened
            .shell
            .theme
            .with_untracked(|theme| theme.terminal().foreground);
        let at = |x: usize, y: usize| {
            let pixel = (y * width as usize + x) * 4;
            [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
        };
        for (pane, screen) in opened.all("terminal").into_iter().enumerate() {
            let (x, y, wide, _) = opened.bounds(screen);
            // The blocks are at the box's top left: the foreground's box
            // there, in device pixels.
            let (x, y, wide) = (x * scale, y * scale, wide * scale);
            let inked: Vec<(usize, usize)> = ((y as usize)..(y as usize + (50. * scale) as usize))
                .flat_map(|row| {
                    ((x as usize)..((x + wide / 2.) as usize)).map(move |column| (column, row))
                })
                .filter(|(column, row)| at(*column, *row) == foreground)
                .collect();
            assert!(
                inked.len() > 100,
                "pane {pane} at {scale}: {} pixels",
                inked.len()
            );
            let (x0, x1) = (
                inked.iter().map(|(x, _)| *x).min().unwrap(),
                inked.iter().map(|(x, _)| *x).max().unwrap(),
            );
            let (y0, y1) = (
                inked.iter().map(|(_, y)| *y).min().unwrap(),
                inked.iter().map(|(_, y)| *y).max().unwrap(),
            );
            let seams = (y0..=y1)
                .flat_map(|row| (x0..=x1).map(move |column| (column, row)))
                .filter(|(column, row)| at(*column, *row) != foreground)
                .count();
            assert_eq!(
                seams, 0,
                "pane {pane} at scale {scale}: box {x0}..{x1} x {y0}..{y1}"
            );
        }
    }
}

#[test]
fn a_terminals_copy_chord_is_not_typed_into_it() {
    let opened = open();
    let pane = opened.panes()[0];
    opened.prompt(pane);
    // Typed into the terminal, the chord would be the program's Ctrl+C,
    // which drops the line begun.
    for character in "echo cop".chars() {
        opened.character(&character.to_string());
    }
    opened.key(
        Key::Character("C".into()),
        Modifiers::CONTROL | Modifiers::SHIFT,
    );
    opened.run("ied");
    opened.wait(pane, "the line's output", |text| {
        text.lines().any(|line| line.trim_end() == "copied")
    });
}

#[test]
fn a_chord_bound_to_a_command_that_cannot_run_now_is_the_views_key() {
    let opened = open();
    let config = horizon_config::parse(
        r#"
        [keybindings]
        "z" = "cancel-agent-turn"
        "#,
    )
    .unwrap();
    opened.shell.apply_config(&config);
    let pane = opened.panes()[0];
    opened.prompt(pane);
    // No agent's turn is in flight: the key is typed into the terminal.
    opened.run("echo zz");
    opened.wait(pane, "the line's output", |text| {
        text.lines().any(|line| line.trim_end() == "zz")
    });
}

#[test]
fn tabs_are_shown_in_turn_in_the_mode_with_a_split_tab_among_them() {
    use horizon_workspace::commands::CommandId;
    let opened = open();
    for split in [
        CommandId::SplitRight,
        CommandId::SplitDown,
        CommandId::SplitRight,
    ] {
        opened.shell.execute(split);
        opened.app.run_effects();
        opened.app.resolve();
    }
    opened.shell.execute(CommandId::NewTab);
    opened.app.run_effects();
    opened.app.resolve();
    // The mode puts a veil over every pane, those of the tab not shown
    // too: they are not on the page then, and come back to it with what
    // was added inside them.
    opened.chord();
    for _ in 0..3 {
        opened.named(NamedKey::Tab);
    }
    assert_eq!(opened.panes().len(), 4);
    assert_eq!(opened.all("veil").len(), 4);
    let boxes: Vec<_> = (opened.all("veil").into_iter())
        .map(|veil| opened.bounds(veil))
        .collect();
    assert!(
        boxes
            .iter()
            .all(|(_, _, width, height)| *width > 0. && *height > 0.),
        "{boxes:?}"
    );
}

#[test]
fn a_shell_with_nothing_happening_has_nothing_to_draw() {
    use std::task::{Context, Waker};

    use horizon_ui::Live;
    use horizon_ui::blitz_dom::Document;

    let opened = open();
    let pane = opened.panes()[0];
    opened.prompt(pane);
    let Opened { app, shell } = opened;
    // As the window's own wiring does: the waker is handed over once.
    let handed = std::cell::Cell::new(false);
    app.on_poll(move |waker| {
        if !handed.replace(true) {
            shell.set_waker(waker.clone());
        }
    });
    let mut live = Live(app);
    // The window's turn: it asks the document whether there is something
    // to draw, and draws if so. It asks again after every draw.
    let turn = |live: &mut Live| {
        let draws = live.poll(Some(Context::from_waker(Waker::noop())));
        if draws {
            live.0.resolve();
        }
        draws
    };
    // What opening left to draw is drawn: the terminal, sized by the
    // first layout, answers with a frame a moment later.
    let (started, mut still) = (Instant::now(), 0);
    while still < 20 {
        assert!(started.elapsed() < Duration::from_secs(5), "never still");
        still = if turn(&mut live) { 0 } else { still + 1 };
        std::thread::sleep(Duration::from_millis(5));
    }
    // Then there is nothing, for as long as nothing happens.
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(5));
        assert!(!turn(&mut live));
    }
    // A frame from the session is something to draw.
    live.0.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        text: Some("a".into()),
        key: Key::Character("a".into()),
        code: keyboard_types::Code::Unidentified,
        modifiers: Modifiers::default(),
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
    }));
    while turn(&mut live) {}
    let started = Instant::now();
    while !turn(&mut live) {
        assert!(started.elapsed() < Duration::from_secs(5), "no echo drawn");
        std::thread::sleep(Duration::from_millis(5));
    }
    let terminal = shell.terminal(pane).expect("a terminal");
    assert!(
        terminal.text().trim_end().ends_with("$ a"),
        "{}",
        terminal.text()
    );
}

/// The file the document view in `pane` shows, as the model keeps it.
fn shown_file(model: &Workspace, pane: horizon_workspace::PaneId) -> Option<std::path::PathBuf> {
    let state = &model.pane_plugin(pane)?.state;
    serde_json::from_value(state["path"].clone()).ok()
}

#[test]
fn a_tab_with_a_tasks_thread_open_is_left_and_come_back_to() {
    use horizon_board::{BoardEvent, Item, Store, sample_envelopes};
    let tasks = [(1, "first", "a"), (2, "second", "b")].map(|(id, title, rank)| Item {
        id,
        title: title.to_string(),
        rank: rank.to_string(),
        ..Item::default()
    });
    let events = tasks
        .into_iter()
        .map(|item| BoardEvent::ItemStored { id: item.id, item });
    let opened = open_with(Opening {
        board: Some(horizon::board::Source::Ready(Store::in_memory(
            sample_envelopes(events),
        ))),
        ..Opening::default()
    });
    opened.chord();
    opened.character(":");
    opened.commit("open board");
    opened.named(NamedKey::Enter);
    opened.until("the tasks", || {
        horizon_ui::testing::text(&opened.app).contains("second")
    });
    opened.named(NamedKey::Enter);
    opened.until("the task", || opened.all("actions").len() == 1);

    // Away from it: the thread, off the page in a split whose boxes are
    // gone, goes on being laid out after.
    opened.chord();
    opened.named(NamedKey::Tab);
    assert_eq!(opened.model(Workspace::active_tab_index), 0);
    assert!(opened.all("actions").is_empty());
    opened.app.resolve();

    // And back: the thread is shown as it was.
    opened.named(NamedKey::Tab);
    assert_eq!(opened.model(Workspace::active_tab_index), 1);
    assert_eq!(opened.all("actions").len(), 1);
}
