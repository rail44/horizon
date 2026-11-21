//! The shell around the views, over `horizon-workspace`'s model with a
//! real shell in each terminal pane: splits, the mode's cursor, tabs, the
//! palette, a boundary dragged, and what closing and exiting leave.

use std::time::{Duration, Instant};

use horizon::workspace::{Modal, Opening, Shell, Spawn, css, view};
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_ui::testing::{Pointer, click_at, pointer};
use horizon_workspace::{Cursor, PaneId, SessionId, SessionKind, Side, Workspace};
use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

struct Opened {
    app: App,
    shell: Shell,
    /// The element the shell's view returned: what the tests look under.
    root: NodeId,
}

fn open() -> Opened {
    open_with(Opening::default())
}

fn open_with(opening: Opening) -> Opened {
    let (mut shell, mut top) = (None, None);
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(spawn, opening);
        (shell, top) = (Some(built), Some(root.0));
        root
    });
    app.resolve();
    Opened {
        app,
        shell: shell.unwrap(),
        root: top.unwrap(),
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
    // Side by side, the boundary's line between them.
    assert!(
        (right.0 - (left.0 + left.2) - 1.).abs() < 0.5,
        "{left:?} {right:?}"
    );
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
fn shift_and_a_direction_swap_panes_and_m_moves_a_pane_to_another_tab() {
    let opened = open();
    opened.chord();
    opened.character("s");
    let panes = opened.panes();
    assert_eq!(panes.len(), 2);

    // In the mode, Shift and a direction swap what the cursor is on with
    // what it would move to; the cursor goes with it, and the mode stays.
    opened.chord();
    opened.key(Key::Character("H".into()), Modifiers::SHIFT);
    assert!(opened.mode());
    assert_eq!(opened.panes(), vec![panes[1], panes[0]]);
    let elements = opened.all("pane");
    assert_eq!(opened.all("cursor").len(), 1);
    assert_eq!(
        opened.bounds(opened.all("cursor")[0]),
        opened.bounds(elements[0])
    );
    opened.key(Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    assert_eq!(opened.panes(), panes);
    // At the edge, nothing.
    opened.key(Key::Character("L".into()), Modifiers::SHIFT);
    assert_eq!(opened.panes(), panes);
    opened.named(NamedKey::Escape);

    // Move to Tab, `m`: a new tab of the group first, the pane with the
    // focus a tab of its own there, shown, with the keys. The mode stays.
    let tabs = opened.model(Workspace::tab_count);
    opened.chord();
    opened.character("m");
    assert_eq!(opened.shell.modal(), Some(Modal::MoveTo));
    opened.named(NamedKey::Enter);
    assert_eq!(opened.shell.modal(), None);
    assert_eq!(opened.model(Workspace::tab_count), tabs + 1);
    assert_eq!(opened.panes(), vec![panes[1]]);
    assert!(opened.model(|model| model.is_active_pane(panes[1])));
    assert!(opened.mode());
    // Alone in its tab, a new tab leaves it where it is.
    opened.character("m");
    opened.named(NamedKey::Enter);
    assert_eq!(opened.model(Workspace::tab_count), tabs + 1);
    // The other tab, not shown, after it: the pane goes beside that tab's
    // active pane, and its own tab, empty, goes.
    opened.character("m");
    assert_eq!(opened.all("item").len(), 2);
    opened.named(NamedKey::ArrowDown);
    opened.named(NamedKey::Enter);
    assert_eq!(opened.model(Workspace::tab_count), tabs);
    assert_eq!(opened.panes(), panes);
    assert!(opened.model(|model| model.is_active_pane(panes[1])));
}

#[test]
fn a_pane_that_is_all_of_its_tab_has_a_tag_with_its_name() {
    let opened = open();
    assert_eq!(opened.panes().len(), 1);
    // Named by its tab's label, the pane still has its tag laid out --
    // the grip and its name -- to show when the pointer comes over it.
    let tags = opened.all("tag");
    assert_eq!(tags.len(), 1);
    let (_, _, width, height) = opened.bounds(tags[0]);
    assert!(
        width > 0. && height > 0.,
        "the tag is laid out: {width}x{height}"
    );
    assert_eq!(opened.all("tag-name").len(), 1);
}

#[test]
fn a_view_dragged_by_its_tag_swaps_with_another_or_is_a_new_tab() {
    use horizon_ui::testing::{Pointer, pointer};
    let opened = open();
    opened.chord();
    opened.character("s");
    let panes = opened.panes();
    assert_eq!(panes.len(), 2);
    let settle = || {
        for _ in 0..2 {
            opened.app.run_effects();
            opened.app.resolve();
        }
    };
    let hidden = || opened.all("hidden").len();

    // A pane of a split tab has a tag -- its tab's label names the other
    // pane, or both -- which shows while the pointer is over the pane.
    let tags = opened.all("tag");
    assert_eq!(tags.len(), 2);
    let elements = opened.all("pane");
    let from = opened.middle(tags[0]);
    let to = opened.middle(elements[1]);
    pointer(&opened.app, from.0, from.1, Pointer::Move);
    settle();
    // Taken and moved over the other pane, that pane is marked and the
    // view's title goes beside the pointer.
    pointer(&opened.app, from.0, from.1, Pointer::Down);
    settle();
    assert_eq!(hidden(), 1, "the title waits for a move");
    pointer(&opened.app, to.0, to.1, Pointer::Drag);
    settle();
    assert_eq!(opened.all("drop"), vec![elements[1]]);
    // (The view taken is faded by what is laid over it.)
    assert_eq!(opened.all("lifted").len(), 1);
    assert_eq!(hidden(), 0);
    // Dropped there, the two swap places.
    pointer(&opened.app, to.0, to.1, Pointer::Up);
    settle();
    assert_eq!(opened.panes(), vec![panes[1], panes[0]]);
    assert!(opened.all("drop").is_empty());
    assert_eq!(hidden(), 1);

    // A press that does not go far is no move.
    let tag = opened.middle(opened.all("tag")[0]);
    pointer(&opened.app, tag.0, tag.1, Pointer::Move);
    settle();
    pointer(&opened.app, tag.0, tag.1, Pointer::Down);
    pointer(&opened.app, tag.0 + 1., tag.1, Pointer::Drag);
    pointer(&opened.app, tag.0 + 1., tag.1, Pointer::Up);
    settle();
    assert_eq!(opened.panes(), vec![panes[1], panes[0]]);

    // Dropped on the group's strip, the view is a new tab of the group.
    let tabs = opened.model(Workspace::tab_count);
    let strip = opened.middle(opened.all("tabs")[0]);
    pointer(&opened.app, tag.0, tag.1, Pointer::Move);
    settle();
    pointer(&opened.app, tag.0, tag.1, Pointer::Down);
    pointer(&opened.app, strip.0, strip.1, Pointer::Drag);
    settle();
    assert_eq!(opened.all("drop"), opened.all("tabs"));
    pointer(&opened.app, strip.0, strip.1, Pointer::Up);
    settle();
    assert_eq!(opened.model(Workspace::tab_count), tabs + 1);
    assert_eq!(opened.panes(), vec![panes[1]]);

    // A pane that is all of its tab has no tag: its tab's label takes it.
    assert!(opened.all("titled").is_empty());
}

#[test]
fn a_view_dragged_onto_a_tabs_label_or_a_views_edge_goes_there() {
    use horizon_ui::testing::{Pointer, pointer};
    let opened = open();
    let settle = || {
        for _ in 0..2 {
            opened.app.run_effects();
            opened.app.resolve();
        }
    };
    let first = opened.panes()[0];
    opened.chord();
    opened.character("t");
    assert_eq!(opened.model(Workspace::tab_count), 2);
    let second = opened.panes()[0];
    // The second tab's label, its pane taken by it, onto the first's: the
    // first tab is shown on the way, and the pane goes into it, beside
    // its active pane.
    let labels = opened.all("tab-label");
    let (from, to) = (opened.middle(labels[1]), opened.middle(labels[0]));
    pointer(&opened.app, from.0, from.1, Pointer::Move);
    pointer(&opened.app, from.0, from.1, Pointer::Down);
    pointer(&opened.app, to.0, to.1, Pointer::Drag);
    settle();
    assert_eq!(opened.model(Workspace::active_tab_index), 0);
    assert_eq!(opened.all("drop"), vec![labels[0]]);
    pointer(&opened.app, to.0, to.1, Pointer::Up);
    settle();
    assert_eq!(opened.model(Workspace::tab_count), 1);
    assert_eq!(opened.panes(), vec![first, second]);

    // The second pane by its tag onto the first's lower edge: below it.
    let panes = opened.all("pane");
    let tag = opened.middle(opened.all("tag")[1]);
    let (x, y, width, height) = opened.bounds(panes[0]);
    let edge = (x + width / 2., y + height * 0.9);
    pointer(&opened.app, tag.0, tag.1, Pointer::Move);
    settle();
    pointer(&opened.app, tag.0, tag.1, Pointer::Down);
    pointer(&opened.app, edge.0, edge.1, Pointer::Drag);
    settle();
    assert_eq!(opened.all("to-bottom").len(), 1);
    pointer(&opened.app, edge.0, edge.1, Pointer::Up);
    settle();
    assert_eq!(opened.panes(), vec![first, second]);
    let (above, below) = (opened.bounds(panes[0]), opened.bounds(panes[1]));
    assert!(above.1 + above.3 <= below.1, "{above:?} {below:?}");
    assert_eq!(above.0, below.0);
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
    // (Two panes in a tab's card, a line between them.)
    let card = opened.bounds(opened.all("shown-tab")[0]);
    // (Each inside the card's border, the boundary's line between them.)
    assert!(
        (before.2 - (card.2 - 3.) / 2.).abs() < 1.,
        "{before:?} {card:?}"
    );
    // Its right edge where the boundary was let go of.
    assert!((after.0 + after.2 - 400.).abs() < 4., "{after:?}");
    assert_eq!(opened.all("pane"), panes);
    // Released: a further move does nothing.
    pointer(&opened.app, 700., y, Pointer::Move);
    opened.app.resolve();
    assert_eq!(opened.bounds(panes[0]), after);
}

#[test]
fn the_keys_of_the_mode_change_the_shares_of_the_split_around_the_cursor() {
    let opened = open();
    opened.chord();
    opened.character("s");
    let widths = || -> Vec<i64> {
        (opened.all("pane").into_iter())
            .map(|pane| opened.bounds(pane).2.round() as i64)
            .collect()
    };
    let even = widths();
    assert_eq!(even.len(), 2);
    assert!((even[0] - even[1]).abs() <= 1, "{even:?}");
    // In the mode, the cursor on the new pane, the right one: `>` widens
    // it, from the one before it.
    opened.chord();
    opened.character(">");
    let wider = widths();
    assert!(
        wider[1] > even[1] && wider[0] < even[0],
        "{even:?} {wider:?}"
    );
    // No split down around it: `+` changes nothing.
    opened.character("+");
    assert_eq!(widths(), wider);
    // `=` makes them even again.
    opened.character("=");
    assert_eq!(widths(), even);
    assert!(opened.mode());
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
fn a_workspace_with_no_tab_shows_the_views_a_tab_can_show_to_pick_from() {
    let opened = open();
    opened.model_closing(opened.panes()[0]);
    opened.until("an empty workspace", || {
        opened.model(Workspace::tab_count) == 0
    });
    let names = || -> Vec<String> {
        let choices = opened.all("choices");
        assert_eq!(choices.len(), 1);
        let names = horizon_ui::testing::find(&opened.app, choices[0], "name");
        let document = opened.app.document();
        let document = document.borrow();
        (names.into_iter())
            .map(|name| document.get_node(name).unwrap().text_content())
            .collect()
    };
    let every = names();
    assert!(every.iter().any(|name| name == "Terminal"), "{every:?}");
    assert!(every.iter().any(|name| name == "Board"), "{every:?}");

    // `:` is still the commands', and what is typed into them is theirs.
    opened.character(":");
    assert!(opened.shell.modal() == Some(Modal::Palette));
    opened.commit("split");
    assert_eq!(names(), every);
    opened.named(NamedKey::Escape);
    assert_eq!(opened.shell.modal(), None);

    // What is typed searches the views, not the mode's keys (`t`, `a`).
    for character in ["b", "o", "a"] {
        opened.character(character);
    }
    assert_eq!(names(), ["Board"]);
    assert_eq!(opened.model(Workspace::tab_count), 0);
    // Enter opens the one chosen, in a tab.
    opened.named(NamedKey::Enter);
    opened.until("a tab", || opened.model(Workspace::tab_count) == 1);
    assert!(opened.all("choices").is_empty());
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
    let made = invoke(
        "new",
        serde_json::json!({ "plugin": "terminal", "split": id(session) }),
    );
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
    std::assert_matches!(
        attached,
        EnvelopeBody::Ok { session_id: None },
        "{attached:?}"
    );
    assert_eq!(opened.panes().len(), 2);
    assert_eq!(opened.model(Workspace::detached_session_count), 0);
    let ended = invoke(
        "terminate-session",
        serde_json::json!({ "session_id": made }),
    );
    std::assert_matches!(ended, EnvelopeBody::Ok { .. }, "{ended:?}");
    assert_eq!(opened.panes(), vec![first]);
    assert_eq!(opened.sessions(), 1);

    // What is not a command, and what this shell has no runtime for.
    for (command, args) in [
        ("no-such-command", serde_json::json!({})),
        ("new", serde_json::json!({ "plugin": "agent" })),
        ("new", serde_json::json!({ "plugin": "no-such-plugin" })),
        // (What a plugin does not take is refused.)
        (
            "new",
            serde_json::json!({ "plugin": "terminal", "arguments": ["extra"] }),
        ),
        (
            "terminate-session",
            serde_json::json!({ "session_id": made }),
        ),
    ] {
        let answer = invoke(command, args);
        std::assert_matches!(answer, EnvelopeBody::Error(_), "{command}: {answer:?}");
    }
}

#[test]
fn the_board_opens_in_a_tab_and_a_task_s_thread_beside_its_list() {
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
    let shown = |opened: &Opened| -> Vec<String> {
        opened.model(|model| {
            (model.visible_panes().iter())
                .map(|pane| pane.plugin.name.clone())
                .collect()
        })
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
    assert_eq!(shown(&opened), ["board-list"]);
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
    std::assert_matches!(answer, EnvelopeBody::Ok { .. }, "{answer:?}");
    assert_eq!(
        *switched.0.lock().unwrap(),
        [(session, "local".to_string(), "small".to_string())]
    );
    // What the runtime refuses is the answer.
    let answer = ask("gone");
    std::assert_matches!(
        &answer, EnvelopeBody::Error(error) if error.message.contains("no such model"),
        "{answer:?}"
    );
}

/// A file whose making tells a program in a test's terminal to go on:
/// `until [ -e FILE ]` waits on it.
fn go_file(what: &str) -> std::path::PathBuf {
    let go = std::env::temp_dir().join(format!("hzn-go-{what}-{}", std::process::id()));
    let _ = std::fs::remove_file(&go);
    go
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

    // From a pane that no longer has them: posted, for its session. (The
    // program says it once the keys have gone, when told to.)
    let go = go_file("notify");
    opened.run(&format!(
        r"until [ -e {go} ]; do sleep 0.02; done; printf '\033]777;notify;Build;done\007'",
        go = go.display()
    ));
    opened.chord();
    opened.character("s");
    let second = opened.panes()[1];
    assert!(opened.model(|model| model.is_active_pane(second)));
    std::fs::write(&go, "").unwrap();
    opened.until("the notification", || !posted.borrow().is_empty());
    let _ = std::fs::remove_file(&go);
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

#[test]
fn what_a_program_says_of_its_state_is_on_its_tab() {
    use horizon_workspace::types::{Doing, ProgramState, ProgramStatus, SessionActivity};

    let opened = open();
    let first = opened.panes()[0];
    let session = opened.model(|model| model.terminal_session_id(first).unwrap());
    opened.prompt(first);
    let activity = || -> Option<SessionActivity> {
        opened.model(|model| model.session_activity(session).cloned())
    };
    let program = || -> Option<ProgramStatus> {
        match activity()?.doing {
            Doing::Program(program) => Some(program),
            Doing::Agent(_) => None,
        }
    };
    let state = || program().map(|program| program.state);
    let text_of = |node: NodeId| {
        let document = opened.app.document();
        let document = document.borrow();
        document.get_node(node).unwrap().text_content()
    };
    let tab = |index: usize| text_of(opened.all("tab-label")[index]);
    assert!(!tab(0).starts_with('◐'), "{}", tab(0));

    // A program that says nothing of itself has no mark; one that reports
    // has the mark of what it reports before its title.
    opened.run(r"printf '\033]7501;state=working:app=claude-code\033\\'");
    opened.until("working", || state() == Some(ProgramState::Working));
    assert!(tab(0).starts_with("◐ "), "{}", tab(0));

    opened.run(
        r"printf '\033]7501;state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZSBCYXNoOiBscw==\033\\'",
    );
    opened.until("blocked", || state() == Some(ProgramState::Blocked));
    assert!(tab(0).starts_with("? "), "{}", tab(0));
    assert_eq!(
        activity().and_then(|activity| activity.detail).as_deref(),
        Some("approve Bash: ls")
    );

    // Done while another tab is the one looked at: news, on the tab. (The
    // program says it once the other tab is looked at, when told to.)
    let go = go_file("done");
    opened.run(&format!(
        r"until [ -e {go} ]; do sleep 0.02; done; printf '\033]7501;state=done:app=claude-code\033\\'",
        go = go.display()
    ));
    opened.chord();
    opened.character("t");
    assert_eq!(opened.model(Workspace::active_tab_index), 1);
    std::fs::write(&go, "").unwrap();
    opened.until("done", || state() == Some(ProgramState::Done));
    let _ = std::fs::remove_file(&go);
    assert!(activity().is_some_and(|activity| !activity.seen));
    assert!(tab(0).starts_with("● "), "{}", tab(0));
    assert!(!tab(1).starts_with('●'), "{}", tab(1));

    // Looked at, it is news no longer, and marked as a program at rest.
    opened.shell.reveal_session(session);
    opened.app.run_effects();
    opened.until("seen", || activity().is_some_and(|activity| activity.seen));
    assert!(tab(0).starts_with("✳ "), "{}", tab(0));

    // Taken back, as a program does on its way out: no mark.
    opened.prompt(first);
    opened.run(r"printf '\033]7501;state=clear\033\\'");
    opened.until("cleared", || program().is_none());
    assert!(!tab(0).starts_with('✳'), "{}", tab(0));
}

/// The page painted, as the color at a point of it.
fn painted_at(opened: &Opened) -> impl Fn(f32, f32) -> [u8; 3] {
    use anyrender::ImageRenderer as _;

    let (width, height) = (1200usize, 800usize);
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width as u32, height as u32);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(
                scene,
                &mut document.borrow_mut(),
                1.0,
                width as u32,
                height as u32,
                0,
                0,
            )
        },
        &mut pixels,
    );
    move |x: f32, y: f32| {
        let at = (y as usize * width + x as usize) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    }
}

#[test]
fn the_modes_cursor_tints_the_pane_it_is_on_as_it_moves() {
    let opened = open();
    opened.chord();
    opened.character("s");
    let elements = opened.all("pane");
    assert_eq!(elements.len(), 2);
    let (left, right) = (opened.middle(elements[0]), opened.middle(elements[1]));

    // In the mode, the cursor on the pane with the focus, the right one: it
    // is tinted, the other veiled alone.
    opened.chord();
    let at = painted_at(&opened);
    let (veiled, tinted) = (at(left.0, left.1), at(right.0, right.1));
    assert_ne!(veiled, tinted);
    // Moved, the tint goes with it.
    opened.character("h");
    let at = painted_at(&opened);
    assert_eq!(at(left.0, left.1), tinted);
    assert_eq!(at(right.0, right.1), veiled);
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
fn the_panes_of_a_split_in_a_tab_have_one_line_between_them_the_focused_ones() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};

    for scale in [1.0f32, 1.5] {
        let opened = open();
        let (width, height) = (1200u32, 800u32);
        (opened.app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        opened.app.resolve();
        // Split: the new pane, on the right, has the focus.
        opened.chord();
        opened.character("s");
        opened.app.resolve();
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
        let at = |x: i64, y: i64| {
            let pixel = (y as usize * width as usize + x as usize) * 4;
            [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
        };
        let (x, y, wide, tall) = opened.bounds(opened.all("divider")[0]);
        let row = ((y + tall / 2.) * scale) as i64;
        let (from, to) = ((x * scale) as i64 - 2, ((x + wide) * scale) as i64 + 2);
        let background = at(from, row);
        let line: Vec<_> = (from..to)
            .map(|column| at(column, row))
            .filter(|pixel| *pixel != background)
            .collect();
        // The focused pane's right side, over the card's.
        let (x, _, wide, _) = opened.bounds(opened.all("shown-tab")[0]);
        let right = ((x + wide) * scale).round() as i64;
        let accent = (right - 3..right + 1)
            .map(|column| at(column, row))
            .find(|pixel| *pixel != background)
            .expect("the card's right side");
        assert_eq!(line, vec![accent], "the boundary at {scale}");
    }
}

#[test]
fn a_focused_panes_line_lies_on_the_line_beside_it_wherever_the_boundaries_are() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
    use horizon_workspace::commands::CommandId;

    // Arrangements whose edges land on a half display pixel, where two
    // edges meant to meet were once rounded opposite ways: three panes down
    // with the shares moved, at 150 %, and a split in a split at 125 %.
    let arrangements: [(f32, u32, u32, &[CommandId], &str); 2] = [
        (
            1.5,
            1200,
            800,
            &[CommandId::SplitDown, CommandId::SplitDown],
            "--",
        ),
        (
            1.25,
            1201,
            801,
            &[CommandId::SplitRight, CommandId::SplitDown],
            "",
        ),
    ];
    let settle = |opened: &Opened| {
        for _ in 0..3 {
            opened.app.run_effects();
            opened.app.resolve();
        }
    };
    for (scale, width, height, splits, shares) in arrangements {
        let opened = open();
        (opened.app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        opened.app.resolve();
        for split in splits {
            opened.shell.execute(*split);
            settle(&opened);
        }
        if !shares.is_empty() {
            opened.chord();
            for key in shares.chars() {
                opened.character(&key.to_string());
            }
            opened.named(NamedKey::Escape);
            settle(&opened);
        }
        for (index, pane) in opened.panes().into_iter().enumerate() {
            opened
                .shell
                .model
                .update(|model| _ = model.activate_pane(pane));
            settle(&opened);
            let element = opened.all("pane")[index];
            assert!(opened.all("focused").contains(&element));
            let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
            let mut pixels = Vec::new();
            let document = opened.app.document();
            renderer.render_to_vec(
                |scene| {
                    let mut document = document.borrow_mut();
                    blitz_paint::paint_scene(
                        scene,
                        &mut document,
                        scale as f64,
                        width,
                        height,
                        0,
                        0,
                    )
                },
                &mut pixels,
            );
            let at = |x: i64, y: i64| {
                let pixel = (y as usize * width as usize + x as usize) * 4;
                [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
            };
            // Across each side, a third of the way along it (clear of a
            // boundary meeting it from the other side): the pane's ground,
            // its line one display pixel wide, and what is beyond -- with no
            // other line beside its own.
            let (x, y, wide, tall) = opened.bounds(element);
            let (left, top) = ((x * scale) as i64, (y * scale) as i64);
            let (right, bottom) = (((x + wide) * scale) as i64, ((y + tall) * scale) as i64);
            let (along_x, along_y) = (left + (right - left) / 3, top + (bottom - top) / 3);
            let sides = [
                (
                    "left",
                    (left - 4..left + 4).map(|c| at(c, along_y)).collect(),
                ),
                (
                    "right",
                    (right - 4..right + 4).map(|c| at(c, along_y)).collect(),
                ),
                ("top", (top - 4..top + 4).map(|r| at(along_x, r)).collect()),
                (
                    "bottom",
                    (bottom - 4..bottom + 4)
                        .map(|r| at(along_x, r))
                        .collect::<Vec<_>>(),
                ),
            ];
            for (side, across) in sides {
                let mut runs: Vec<([u8; 3], usize)> = Vec::new();
                for pixel in across {
                    match runs.last_mut() {
                        Some((color, length)) if *color == pixel => *length += 1,
                        _ => runs.push((pixel, 1)),
                    }
                }
                assert!(
                    runs.len() == 3 && runs[1].1 == 1,
                    "pane {index}'s {side} side at {scale}: {runs:?}"
                );
            }
        }
    }
}

#[test]
fn a_tabs_outline_is_one_display_pixel_on_every_side_at_a_fractional_scale() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};

    for scale in [1.25f32, 1.5, 1.75] {
        let opened = open();
        let (width, height) = (1200u32, 800u32);
        (opened.app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        opened.app.resolve();
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
        let at = |x: i64, y: i64| {
            let pixel = (y as usize * width as usize + x as usize) * 4;
            [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
        };
        let label = opened.all("tab-label")[0];
        let (x, y, wide, tall) = opened.bounds(label);
        let (left, top) = ((x * scale).round() as i64, (y * scale).round() as i64);
        let (right, bottom) = (
            ((x + wide) * scale).round() as i64,
            ((y + tall) * scale).round() as i64,
        );
        let (middle_x, middle_y) = ((left + right) / 2, (top + bottom) / 2);
        let background = at(left - 4, middle_y);
        // Each side, from a few pixels outside the box to a few inside it:
        // what is not the page's ground there is the side's line.
        let sides = [
            (
                "top",
                (top - 3..top + 3)
                    .map(|y| at(middle_x, y))
                    .collect::<Vec<_>>(),
            ),
            (
                "bottom",
                (bottom - 3..bottom + 3).map(|y| at(middle_x, y)).collect(),
            ),
            (
                "left",
                (left - 3..left + 3).map(|x| at(x, middle_y)).collect(),
            ),
            (
                "right",
                (right - 3..right + 3).map(|x| at(x, middle_y)).collect(),
            ),
        ];
        let lines: Vec<(&str, Vec<[u8; 3]>)> = (sides.into_iter())
            .map(|(side, run)| {
                (
                    side,
                    run.into_iter()
                        .filter(|pixel| *pixel != background)
                        .collect(),
                )
            })
            .collect();
        for (side, line) in &lines {
            assert_eq!(line.len(), 1, "the {side} side at {scale}: {line:?}");
        }
        assert!(
            lines.iter().all(|(_, line)| line == &lines[0].1),
            "the sides at {scale} are not drawn alike: {lines:?}"
        );
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
fn the_shown_tab_is_marked_in_the_group_with_the_focus() {
    let opened = open();
    let marked = || -> Vec<NodeId> {
        let active = opened.all("active");
        (opened.all("focused").into_iter())
            .filter(|node| active.contains(node) && opened.all("tab-label").contains(node))
            .collect()
    };
    assert_eq!(marked().len(), 1);
    // A split from the strip goes beside the group: the focus is on a pane
    // in no group, and no tab is marked.
    opened.chord();
    opened.character("k");
    assert!(opened.model(Workspace::cursor_strip).is_some());
    opened.character("s");
    assert!(!opened.mode());
    assert!(marked().is_empty());
    // Back in the group, it is again.
    opened.chord();
    opened.character("h");
    opened.named(NamedKey::Enter);
    assert_eq!(marked().len(), 1);
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
    // The extensions are read off the page's thread from when the shell is
    // drawn, and their coming in is something to draw: waited for here,
    // or it may come while nothing is to happen.
    opened.until("the extensions read", || opened.shell.extensions_read());
    let Opened { app, shell, .. } = opened;
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
    // The echo is in the view only once a turn said there was something to
    // draw and the draw took it in. It may come in with the draws the key
    // itself asked for: the PTY echoes in well under a millisecond, as fast
    // as those are drawn, so a wait for a draw after them would wait for
    // nothing.
    let terminal = shell.terminal(pane).expect("a terminal");
    let started = Instant::now();
    while !terminal.text().trim_end().ends_with("$ a") {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no echo drawn: {}",
            terminal.text()
        );
        turn(&mut live);
        std::thread::sleep(Duration::from_millis(5));
    }
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

/// A terminal on a tab that is not shown keeps no terminal of its
/// own, and has one again, with what was written meanwhile, when its tab
/// is back.
#[test]
fn a_terminal_off_the_page_has_no_terminal_of_its_own_until_it_is_back() {
    let opened = open();
    let first = opened.panes()[0];
    opened.prompt(first);
    let terminal = opened.shell.terminal(first).expect("a terminal");
    opened.until("the session's own terminal", || terminal.has_a_terminal());
    opened.run("sleep 1; echo written-meanwhile");

    opened.chord();
    opened.character("t");
    assert_eq!(opened.model(Workspace::active_tab_index), 1);
    assert!(!terminal.has_a_terminal());
    let second = opened.panes()[0];
    opened.prompt(second);
    // What is written to the first is not taken in by anything here.
    std::thread::sleep(Duration::from_millis(1300));
    opened.app.resolve();
    assert!(!terminal.has_a_terminal());

    opened.chord();
    opened.named(NamedKey::Tab);
    opened.named(NamedKey::Escape);
    assert_eq!(opened.model(Workspace::active_tab_index), 0);
    opened.until("the session's own terminal again", || {
        terminal.has_a_terminal()
    });
    opened.wait(first, "what was written meanwhile", |text| {
        text.lines().any(|line| line == "written-meanwhile")
    });
    // And the keys go to it as before.
    opened.run("echo back");
    opened.wait(first, "the output", |text| {
        text.lines().any(|line| line == "back")
    });
}

/// A terminal put on the left of the tab group, with the cursor on the
/// group's strip: what keeps a pane on screen whatever tab is shown
/// (`docs/workspace-layout-design.md`). Returns the new pane.
fn left_of_the_group(opened: &Opened) -> PaneId {
    opened.chord();
    opened.character("k");
    let group = opened.model(|model| model.tab_groups()[0].id);
    assert_eq!(
        opened.model(|model| model.workspace_cursor()),
        Some(Cursor::Strip(group))
    );
    // (The strip is marked while the cursor is on it.)
    let strip = opened.all("tabs")[0];
    assert!(opened.all("cursor").contains(&strip));
    opened.shell.model.update(|model| {
        model.split_cursor_with_new_session(SessionKind::Terminal, Side::Left, true);
        model.exit_workspace_mode();
    });
    opened.app.run_effects();
    opened.app.resolve();
    opened.model(|model| model.cursor_pane_id().expect("the new pane has focus"))
}

#[test]
fn a_pane_beside_the_tab_group_runs_the_full_height_and_stays_across_tabs() {
    let opened = open();
    let first = opened.panes()[0];
    let left = left_of_the_group(&opened);
    assert_ne!(left, first);

    // One strip, above the group only: the pane on its left starts at the
    // top of the window and the strip right of it.
    let strips = opened.all("tabs");
    assert_eq!(strips.len(), 1);
    let panes = opened.all("pane");
    assert_eq!(panes.len(), 2);
    let (strip_x, strip_y, _, _) = opened.bounds(strips[0]);
    let (left_x, left_y, left_wide, _) = opened.bounds(panes[0]);
    assert!(left_y <= strip_y + 1., "{left_y} {strip_y}");
    assert!(
        strip_x >= left_x + left_wide - 1.,
        "{strip_x} {left_x} {left_wide}"
    );

    // A tab opened from the group's pane goes in the group; the pane on the
    // left stays, whichever tab is shown.
    opened
        .shell
        .model
        .update(|model| _ = model.activate_pane(first));
    opened.app.run_effects();
    opened.app.resolve();
    opened.chord();
    opened.character("t");
    assert_eq!(opened.model(Workspace::tab_count), 2);
    assert_eq!(opened.all("tab-label").len(), 2);
    assert!(opened.panes().contains(&left), "{:?}", opened.panes());
    opened.chord();
    opened.named(NamedKey::Tab);
    opened.named(NamedKey::Escape);
    assert!(opened.panes().contains(&left));
    assert!(opened.panes().contains(&first));
}

/// Paints the workspace with a terminal on the left of its tab group and,
/// with `HORIZON_LAYOUT_TWO` set, a second group made around that
/// terminal, in this machine's theme, to the file `HORIZON_LAYOUT_OUT`
/// names (a PPM). A picture for a person to look at; run by hand.
#[test]
#[ignore = "writes a picture for a person: set HORIZON_LAYOUT_OUT"]
fn paint_the_layout() {
    use anyrender::ImageRenderer as _;

    let out = std::env::var("HORIZON_LAYOUT_OUT").expect("HORIZON_LAYOUT_OUT");
    let opened = open_with(Opening {
        config: horizon_config::load().clone(),
        ..Opening::default()
    });
    let first = opened.panes()[0];
    opened.prompt(first);
    let left = left_of_the_group(&opened);
    opened.prompt(left);
    opened
        .shell
        .model
        .update(|model| _ = model.activate_pane(first));
    opened.app.run_effects();
    opened.chord();
    opened.character("t");
    if std::env::var_os("HORIZON_LAYOUT_TWO").is_some() {
        // A tab opened from the pane under no group makes its place a group.
        opened
            .shell
            .model
            .update(|model| _ = model.activate_pane(left));
        opened.app.run_effects();
        opened.chord();
        opened.character("t");
    }
    for pane in opened.panes() {
        opened.prompt(pane);
    }
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

#[test]
fn split_left_from_a_strip_puts_a_terminal_beside_the_whole_group() {
    let opened = open();
    let first = opened.panes()[0];
    opened.prompt(first);

    // From the strip, the palette's Split Left: with nothing but
    // terminals to choose from, a terminal at once.
    opened.chord();
    opened.character("k");
    opened.character(":");
    opened.commit("split left");
    opened.named(NamedKey::Enter);
    assert!(!opened.mode());
    let panes = opened.panes();
    assert_eq!(panes.len(), 2);
    let left = panes[0];
    assert_ne!(left, first);
    assert_eq!(opened.model(|model| model.cursor_pane_id()), Some(left));
    // It is under no group: the one strip is right of it.
    assert_eq!(opened.model(|model| model.tab_groups().len()), 1);
    let (strip_x, ..) = opened.bounds(opened.all("tabs")[0]);
    let (left_x, _, left_wide, _) = opened.bounds(opened.all("pane")[0]);
    assert!(strip_x >= left_x + left_wide - 1.);
    opened.prompt(left);

    // Tab in the mode, from the group's pane, goes round the group's tabs
    // only: the terminal on the left stays.
    opened
        .shell
        .model
        .update(|model| _ = model.activate_pane(first));
    opened.app.run_effects();
    opened.app.resolve();
    opened.chord();
    opened.character("t");
    opened.chord();
    opened.named(NamedKey::Tab);
    assert_eq!(opened.model(Workspace::active_tab_index), 0);
    assert!(opened.panes().contains(&left));
    opened.named(NamedKey::Tab);
    assert_eq!(opened.model(Workspace::active_tab_index), 1);
    assert!(opened.panes().contains(&left));
    opened.named(NamedKey::Escape);
}

/// Paints a view being moved over another, in this machine's theme, to
/// the file `HORIZON_MOVING_OUT` names (a PPM): a picture for a person to
/// look at. Run by hand.
#[test]
#[ignore = "writes a picture for a person: set HORIZON_MOVING_OUT"]
fn paint_a_view_moving() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::testing::{Pointer, pointer};

    let out = std::env::var("HORIZON_MOVING_OUT").expect("HORIZON_MOVING_OUT");
    let mine = horizon_config::load();
    let config = horizon_config::RawConfig {
        theme: mine.theme.clone(),
        ui: mine.ui.clone(),
        terminal: mine.terminal.clone(),
        ..horizon_config::RawConfig::default()
    };
    let opened = open_with(Opening {
        config,
        ..Opening::default()
    });
    opened.chord();
    opened.character("s");
    let elements = opened.all("pane");
    let handle = opened.middle(opened.all("handle")[0]);
    let to = opened.middle(elements[1]);
    if std::env::var_os("HORIZON_MOVING_HOVER").is_some() {
        // The pointer near the top edge of the right pane.
        let (x, y, width, _) = opened.bounds(elements[1]);
        pointer(&opened.app, x + width - 60., y + 6., Pointer::Move);
    } else {
        pointer(&opened.app, handle.0, handle.1, Pointer::Move);
        opened.app.resolve();
        pointer(&opened.app, handle.0, handle.1, Pointer::Down);
        pointer(&opened.app, to.0, to.1, Pointer::Drag);
    }
    for _ in 0..3 {
        opened.app.run_effects();
        opened.app.resolve();
    }
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

#[test]
fn a_focused_panes_line_stays_one_display_pixel_while_the_pointer_is_over_a_pane() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
    use horizon_workspace::commands::CommandId;

    // The pointer over a pane shows its tag. blitz-dom follows the pointer
    // with a hit test at the end of its resolve, and keeps where it found a
    // box over others (the focused pane is one) from the layout before it
    // is rounded to the display's pixels: that focused pane's line was then
    // painted half a pixel off, blurred across two.
    let settle = |opened: &Opened| {
        for _ in 0..3 {
            opened.app.run_effects();
            opened.app.resolve();
        }
    };
    for scale in [1.25f32, 1.5] {
        let (width, height) = (1200u32, 800u32);
        let opened = open();
        (opened.app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        opened.app.resolve();
        opened.shell.execute(CommandId::SplitRight);
        settle(&opened);
        let panes = opened.all("pane");
        let focused = *(panes.iter())
            .find(|pane| opened.all("focused").contains(pane))
            .expect("a focused pane");
        let over = opened.middle(panes[0]);
        pointer(&opened.app, over.0, over.1, Pointer::Move);
        settle(&opened);

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
        let at = |x: i64, y: i64| {
            let pixel = (y as usize * width as usize + x as usize) * 4;
            [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
        };
        let (x, y, wide, tall) = opened.bounds(focused);
        let (left, top) = ((x * scale) as i64, (y * scale) as i64);
        let (right, bottom) = (((x + wide) * scale) as i64, ((y + tall) * scale) as i64);
        let (along_x, along_y) = (left + (right - left) / 3, top + (bottom - top) / 3);
        let sides: [(&str, Vec<[u8; 3]>); 4] = [
            ("top", (top - 4..top + 4).map(|r| at(along_x, r)).collect()),
            (
                "bottom",
                (bottom - 4..bottom + 4).map(|r| at(along_x, r)).collect(),
            ),
            (
                "left",
                (left - 4..left + 4).map(|c| at(c, along_y)).collect(),
            ),
            (
                "right",
                (right - 4..right + 4).map(|c| at(c, along_y)).collect(),
            ),
        ];
        let mut lines = Vec::new();
        for (side, across) in sides {
            let mut runs: Vec<([u8; 3], usize)> = Vec::new();
            for pixel in across {
                match runs.last_mut() {
                    Some((color, length)) if *color == pixel => *length += 1,
                    _ => runs.push((pixel, 1)),
                }
            }
            // The ground, the line one display pixel wide, the ground.
            assert!(
                runs.len() == 3 && runs[0].0 == runs[2].0 && runs[1].1 == 1,
                "the {side} side at {scale}: {runs:?}"
            );
            lines.push(runs[1].0);
        }
        assert!(
            lines.iter().all(|line| *line == lines[0]),
            "the sides at {scale}: {lines:?}"
        );
    }
}
