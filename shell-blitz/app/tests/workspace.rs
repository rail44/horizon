//! The shell around the views, over `horizon-workspace`'s model with a
//! real shell in each terminal pane: splits, the mode's cursor, tabs, the
//! palette, a boundary dragged, and what closing and exiting leave.

use std::time::{Duration, Instant};

use horizon_blitz::workspace::{Modal, Opening, Shell, Spawn, css, view};
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_ui::testing::{Pointer, click_at, pointer};
use horizon_workspace::{PaneId, Workspace};
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
    let app = App::new(1200, 800, &css(), || {
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
        .is_some_and(|classes| classes.split(' ').any(|name| name == class));
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
    // A lone pane is not closed from the palette, no session is detached
    // to end, and the test's shell reports no directory to open.
    assert_eq!(opened.all("disabled").len(), 3);
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
