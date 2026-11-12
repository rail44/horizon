//! The shell around the views: four of them on one page, a boundary
//! dragged, the palette, workspace mode, tabs.

use reactive_graph::traits::GetUntracked;
use ui_foundation_spike::blitz_dom::{BaseDocument, NodeId, local_name};
use ui_foundation_spike::blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, BlitzWheelEvent, KeyState, PointerCoords,
    UiEvent,
};
use ui_foundation_spike::theme::{self, Theme};
use ui_foundation_spike::workspace::{Command, Workspace, css, view};
use ui_foundation_spike::{App, Pointer, click_at, move_to, pointer};

struct Opened {
    app: App,
    workspace: Workspace,
}

fn open() -> Opened {
    let mut workspace = None;
    let css = [theme::CSS, &css()].concat();
    let app = App::new(1200, 800, &css, || {
        let (root, state) = view(Theme::built_in().terminal());
        workspace = Some(state);
        theme::themed(Theme::built_in, root)
    });
    app.resolve();
    Opened {
        app,
        workspace: workspace.unwrap(),
    }
}

fn collect(document: &BaseDocument, id: NodeId, class: &str, found: &mut Vec<NodeId>) {
    let node = document.get_node(id).expect("a node");
    let named = (node.attr(local_name!("class")))
        .is_some_and(|classes| classes.split(' ').any(|name| name == class));
    if named
        || node
            .element_data()
            .is_some_and(|element| &*element.name.local == class)
    {
        found.push(id);
    }
    for child in &node.children {
        collect(document, *child, class, found);
    }
}

impl Opened {
    /// The elements of a class, or of a tag, in document order.
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

    fn click(&self, node: NodeId) {
        let (x, y) = self.middle(node);
        click_at(&self.app, x, y);
        self.app.resolve();
    }

    fn key(&self, key: keyboard_types::Key, ctrl: bool) {
        let mut modifiers = keyboard_types::Modifiers::default();
        if ctrl {
            modifiers |= keyboard_types::Modifiers::CONTROL;
        }
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers,
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }));
        self.app.resolve();
    }

    fn character(&self, character: &str) {
        self.key(keyboard_types::Key::Character(character.into()), false);
    }

    fn named(&self, key: keyboard_types::NamedKey) {
        self.key(keyboard_types::Key::Named(key), false);
    }

    /// Workspace mode, then the palette.
    fn open_palette(&self) {
        self.key(keyboard_types::Key::Character("'".into()), true);
        assert!(self.workspace.mode.get_untracked());
        self.character(":");
        assert!(self.workspace.palette.get_untracked());
        // The mode stays on under the palette, and ends with it.
        assert!(self.workspace.mode.get_untracked());
    }

    /// Text as an input method commits it into the focused field.
    fn commit(&self, text: &str) {
        self.app
            .dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        self.app.resolve();
    }

    fn field_text(&self, field: NodeId) -> String {
        let document = self.app.document();
        let document = document.borrow();
        let node = document.get_node(field).unwrap();
        let input = node.element_data().unwrap().text_input_data().unwrap();
        input.editor.raw_text().to_string()
    }

    fn scrolled(&self, node: NodeId) -> f64 {
        let document = self.app.document();
        let document = document.borrow();
        document.get_node(node).unwrap().scroll_offset().y
    }

    /// Which pane, by its place among all of them, is drawn at a point.
    fn pane_at(&self, x: f32, y: f32) -> Option<usize> {
        let panes = self.all("pane");
        let document = self.app.document();
        let document = document.borrow();
        let mut node = document.hit(x, y).map(|hit| hit.node_id);
        while let Some(id) = node {
            if let Some(pane) = panes.iter().position(|pane| *pane == id) {
                return Some(pane);
            }
            node = document.get_node(id).and_then(|node| node.parent);
        }
        None
    }

    /// Which of the board's rows is the selected one.
    fn selected_row(&self) -> Option<usize> {
        let selected = self.all("selected");
        (self.all("row").iter()).position(|row| selected.contains(row))
    }
}

const OPENING: &str = "across 0.50 [Agent, down 0.60 [Markdown, Board]]";

#[test]
fn four_views_share_a_page_each_under_its_own_rules() {
    let opened = open();
    assert_eq!(opened.workspace.describe(), OPENING);
    // An agent beside a document over a board; the terminal's tab is not
    // shown.
    let panes: Vec<_> = (opened.all("pane").into_iter())
        .map(|pane| opened.bounds(pane))
        .collect();
    assert_eq!(panes.len(), 4);
    let (agent, document, board) = (panes[0], panes[1], panes[2]);
    assert_eq!((agent.0, agent.2), (0., 597.5));
    assert_eq!(document.0, 602.5);
    assert_eq!(board.0, 602.5);
    assert_eq!(board.1, document.1 + document.3 + 5.);
    assert!(document.3 > board.3);
    assert_eq!(opened.pane_at(300., 400.), Some(0));
    assert_eq!(opened.pane_at(900., 200.), Some(1));
    assert_eq!(opened.pane_at(900., 700.), Some(2));
    // The views each have a `.chip`, `.title` or `.notice` of their own,
    // and each is laid out by its own view's rules: the board's rows and
    // the agent's field have the heights their views give them.
    assert_eq!(opened.bounds(opened.all("row")[0]).3, 45.);
    assert_eq!(opened.bounds(opened.all("textarea")[0]).3, 24.);
    assert!(opened.bounds(opened.all("markdown")[0]).3 > 300.);
}

#[test]
fn a_boundary_is_dragged_and_a_rearrangement_keeps_what_the_panes_hold() {
    let opened = open();
    let panes = opened.all("pane");
    let field = opened.all("textarea")[0];
    opened.commit("下書き");
    assert_eq!(opened.field_text(field), "下書き");

    // The boundary between the agent and the rest, to a third of the way.
    let (x, y) = opened.middle(opened.all("divider")[0]);
    pointer(&opened.app, x, y, Pointer::Move);
    pointer(&opened.app, x, y, Pointer::Down);
    pointer(&opened.app, 500., y + 30., Pointer::Drag);
    pointer(&opened.app, 400., y + 60., Pointer::Drag);
    pointer(&opened.app, 400., y + 60., Pointer::Up);
    opened.app.resolve();
    assert_eq!(
        opened.workspace.describe(),
        "across 0.33 [Agent, down 0.60 [Markdown, Board]]"
    );
    let agent = opened.bounds(panes[0]);
    assert!((agent.2 - 398.).abs() < 1., "{agent:?}");
    // Released: a further move does nothing.
    pointer(&opened.app, 700., y, Pointer::Move);
    opened.app.resolve();
    assert_eq!(opened.bounds(panes[0]), agent);

    // The document, scrolled; then the board's pane is split, which builds
    // the tab's boxes again around the same panes.
    let scroll = opened.all("scroll")[0];
    let (x, y) = opened.middle(scroll);
    move_to(&opened.app, x, y);
    opened.app.dispatch(UiEvent::Wheel(BlitzWheelEvent {
        delta: BlitzWheelDelta::Pixels(0., -240.),
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        buttons: Default::default(),
        mods: Default::default(),
        element: Default::default(),
    }));
    opened.app.resolve();
    assert_eq!(opened.scrolled(scroll), 240.);

    opened.click(opened.all("row")[1]);
    opened.workspace.execute(Command::SplitRight);
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(
        opened.workspace.describe(),
        "across 0.33 [Agent, down 0.60 [Markdown, across 0.50 [Board, Empty]]]"
    );
    assert_eq!(opened.all("pane")[..3], panes[..3]);
    assert_eq!(opened.scrolled(scroll), 240.);
    assert_eq!(opened.field_text(field), "下書き");
    assert_eq!(opened.selected_row(), Some(1));
}

#[test]
fn the_palette_runs_a_command_on_the_focused_pane() {
    let opened = open();
    opened.click(opened.all("row")[0]);
    opened.open_palette();
    assert_eq!(opened.all("item").len(), 9);

    // Typed into the palette's field, which has the focus.
    opened.commit("split d");
    assert_eq!(opened.all("item").len(), 1);
    opened.named(keyboard_types::NamedKey::Enter);
    assert!(!opened.workspace.palette.get_untracked());
    assert!(opened.all("item").is_empty());
    assert_eq!(
        opened.workspace.describe(),
        "across 0.50 [Agent, down 0.60 [Markdown, down 0.50 [Board, Empty]]]"
    );

    // The new pane has the focus: the next command is about it.
    opened.open_palette();
    opened.commit("term");
    opened.named(keyboard_types::NamedKey::Enter);
    assert_eq!(
        opened.workspace.describe(),
        "across 0.50 [Agent, down 0.60 [Markdown, down 0.50 [Board, Terminal]]]"
    );

    // The arrows choose; Escape leaves without running anything.
    opened.open_palette();
    opened.named(keyboard_types::NamedKey::ArrowDown);
    opened.named(keyboard_types::NamedKey::ArrowDown);
    opened.named(keyboard_types::NamedKey::ArrowDown);
    opened.named(keyboard_types::NamedKey::ArrowDown);
    assert_eq!(opened.all("chosen"), [opened.all("item")[4]]);
    opened.named(keyboard_types::NamedKey::Escape);
    assert!(!opened.workspace.palette.get_untracked());
    opened.open_palette();
    opened.commit("close");
    opened.named(keyboard_types::NamedKey::Enter);
    assert_eq!(opened.workspace.describe(), OPENING);
    assert_eq!(opened.all("pane").len(), 4);

    // A click on an item runs it too: on the agent's pane, which has the
    // focus now that the closed pane's tab gave it to its first pane.
    opened.open_palette();
    opened.click(opened.all("item")[2]);
    assert_eq!(
        opened.workspace.describe(),
        "across 0.50 [across 0.50 [Agent, Empty], down 0.60 [Markdown, Board]]"
    );
}

#[test]
fn in_workspace_mode_the_keys_are_the_shells_and_not_the_views() {
    let opened = open();
    opened.click(opened.all("row")[0]);
    assert_eq!(opened.selected_row(), Some(0));
    opened.character("j");
    assert_eq!(opened.selected_row(), Some(1));

    opened.key(keyboard_types::Key::Character("'".into()), true);
    assert!(opened.workspace.mode.get_untracked());
    assert_eq!(opened.all("mode").len(), 1);
    opened.character("j");
    assert_eq!(opened.selected_row(), Some(1));

    opened.named(keyboard_types::NamedKey::Escape);
    assert!(!opened.workspace.mode.get_untracked());
    assert!(opened.all("mode").is_empty());
    opened.character("j");
    assert_eq!(opened.selected_row(), Some(2));
}

#[test]
fn a_tab_shows_its_own_panes_and_a_new_one_opens_empty() {
    let opened = open();
    let labels = opened.all("tab-label");
    assert_eq!(labels.len(), 2);
    opened.click(labels[1]);
    assert_eq!(opened.workspace.describe(), "Terminal");
    // The terminal's pane is what is drawn, everywhere under the tabs.
    assert_eq!(opened.bounds(opened.all("pane")[3]).2, 1200.);
    assert_eq!(opened.pane_at(300., 400.), Some(3));
    assert_eq!(opened.pane_at(900., 700.), Some(3));

    opened.workspace.execute(Command::NewTab);
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(opened.all("tab-label").len(), 3);
    assert_eq!(opened.workspace.describe(), "Empty");
    opened.workspace.execute(Command::NextTab);
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(opened.workspace.describe(), OPENING);
}

#[test]
fn the_shells_chord_and_its_mode_are_kept_from_a_terminal_with_the_focus() {
    let opened = open();
    // The second tab's pane is a terminal on a real shell.
    opened.click(opened.all("tab-label")[1]);
    let wait = |what: &str, met: &dyn Fn(&str) -> bool| {
        let started = std::time::Instant::now();
        loop {
            opened.app.resolve();
            let text = opened.workspace.terminal_text().unwrap_or_default();
            if met(&text) {
                return text;
            }
            assert!(started.elapsed().as_secs() < 10, "no {what} in:\n{text}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    wait("a prompt", &|text| text.trim_end().ends_with('$'));

    for character in ["e", "c", "h", "o", " ", "a"] {
        opened.character(character);
    }
    // The chord is the shell's: the terminal, which has the focus and takes
    // every key it is given, does not get it, nor what is typed in the mode.
    opened.key(keyboard_types::Key::Character("'".into()), true);
    assert!(opened.workspace.mode.get_untracked());
    opened.character("x");
    opened.named(keyboard_types::NamedKey::Escape);
    assert!(!opened.workspace.mode.get_untracked());
    // Out of the mode, the keys are the terminal's again.
    opened.character("b");
    opened.named(keyboard_types::NamedKey::Enter);
    let text = wait("the command's output", &|text| {
        text.lines().any(|line| line == "ab")
    });
    assert!(text.contains("$ echo ab"), "{text}");
    assert!(!text.contains('x') && !text.contains('\''), "{text}");
}
