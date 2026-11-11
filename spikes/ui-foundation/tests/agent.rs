//! The agent pane on this stack, driven the way a session and an owner
//! drive Horizon's own (`src/agent/view/`).

use std::cell::RefCell;
use std::rc::Rc;

use reactive_graph::traits::GetUntracked;
use ui_foundation_spike::agent::sample::Session;
use ui_foundation_spike::agent::{Agent, Asked, CSS, view};
use ui_foundation_spike::blitz_dom::{BaseDocument, NodeId, local_name};
use ui_foundation_spike::blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, BlitzWheelEvent, KeyState, PointerCoords,
    UiEvent,
};
use ui_foundation_spike::{App, click_at, move_to};

struct Opened {
    app: App,
    agent: Agent,
    asked: Rc<RefCell<Vec<Asked>>>,
}

fn open(session: &Session) -> Opened {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = asked.clone();
    let mut agent = None;
    let app = App::new(900, 700, CSS, || {
        let (root, state) = view(move |asked| log.borrow_mut().push(asked));
        agent = Some(state);
        root
    });
    let opened = Opened {
        app,
        agent: agent.unwrap(),
        asked,
    };
    opened.show(session);
    opened
}

fn has_class(document: &BaseDocument, id: NodeId, class: &str) -> bool {
    (document.get_node(id))
        .and_then(|node| node.attr(local_name!("class")))
        .is_some_and(|classes| classes.split(' ').any(|name| name == class))
}

fn collect(document: &BaseDocument, id: NodeId, class: &str, found: &mut Vec<NodeId>) {
    if has_class(document, id, class) {
        found.push(id);
    }
    for child in &document.get_node(id).expect("a node").children {
        collect(document, *child, class, found);
    }
}

impl Opened {
    fn show(&self, session: &Session) {
        session.show(&self.agent);
        self.app.run_effects();
        self.app.resolve();
    }

    fn all(&self, class: &str) -> Vec<NodeId> {
        let document = self.app.document();
        let document = document.borrow();
        let mut found = Vec::new();
        collect(&document, document.root_element().id, class, &mut found);
        found
    }

    fn text(&self) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.root_element().text_content()
    }

    /// Top and bottom of a node's box on the page.
    fn span(&self, node: NodeId) -> (f64, f64) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (bounds.y, bounds.y + bounds.height)
    }

    fn click(&self, node: NodeId) {
        let (x, y) = {
            let document = self.app.document();
            let document = document.borrow();
            let bounds = document.get_client_bounding_rect(node).expect("a box");
            (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.)
        };
        click_at(&self.app, x as f32, y as f32);
        self.app.resolve();
    }

    fn scrolled(&self) -> f64 {
        let document = self.app.document();
        let document = document.borrow();
        document
            .get_node(self.agent.column.0)
            .unwrap()
            .scroll_offset()
            .y
    }

    fn wheel(&self, down: f64) {
        let coords = PointerCoords {
            page_x: 400.,
            page_y: 300.,
            screen_x: 400.,
            screen_y: 300.,
            client_x: 400.,
            client_y: 300.,
        };
        move_to(&self.app, 400., 300.);
        self.app.dispatch(UiEvent::Wheel(BlitzWheelEvent {
            delta: BlitzWheelDelta::Pixels(0., -down),
            coords,
            buttons: Default::default(),
            mods: Default::default(),
            element: Default::default(),
        }));
        self.app.resolve();
    }

    fn press(&self, key: keyboard_types::NamedKey, code: keyboard_types::Code, shift: bool) {
        let mut modifiers = keyboard_types::Modifiers::default();
        if shift {
            modifiers |= keyboard_types::Modifiers::SHIFT;
        }
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key: keyboard_types::Key::Named(key),
            code,
            modifiers,
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }));
        self.app.resolve();
    }

    /// Text as an input method commits it into the focused field.
    fn commit(&self, text: &str) {
        self.app
            .dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        self.app.resolve();
    }

    fn enter(&self) {
        self.press(
            keyboard_types::NamedKey::Enter,
            keyboard_types::Code::Enter,
            false,
        );
    }

    fn field_height(&self) -> f32 {
        let document = self.app.document();
        let document = document.borrow();
        let node = document.get_node(self.agent.field.0).unwrap();
        node.final_layout().size.height
    }
}

#[test]
fn a_history_opens_at_its_end_and_follows_what_streams_in() {
    let mut session = Session::long(30);
    let opened = open(&session);
    let units = opened.all("unit");
    assert_eq!(units.len(), 60);
    // The column ends where the composer starts, less its own padding.
    let column_bottom = opened.span(opened.all("composer")[0]).0 - 16.;
    let (_, last_bottom) = opened.span(*units.last().unwrap());
    assert!(
        last_bottom <= column_bottom + 1.,
        "{last_bottom} {column_bottom}"
    );
    assert!(
        last_bottom > column_bottom - 40.,
        "{last_bottom} {column_bottom}"
    );

    session.begin_turn("clippy の警告も直してください");
    opened.show(&session);
    assert!(opened.text().contains("Thinking"));
    for chunk in 0..40 {
        session.stream(&format!("{chunk} 行目の文章が流れてきます。\n\n"));
        opened.show(&session);
    }
    let after = opened.all("unit");
    assert_eq!(after.len(), 62);
    // The ended turns kept their elements; only the turn in progress is new.
    assert_eq!(after[..60], units[..]);
    assert!(opened.text().contains("39 行目の文章"));
    assert!(!opened.text().contains("Thinking"));
    let (_, last_bottom) = opened.span(*after.last().unwrap());
    assert!(
        last_bottom <= column_bottom + 1.,
        "{last_bottom} {column_bottom}"
    );
    assert!(
        last_bottom > column_bottom - 40.,
        "{last_bottom} {column_bottom}"
    );
}

#[test]
fn a_scroll_away_from_the_end_stops_the_following_and_back_resumes_it() {
    let mut session = Session::long(30);
    let opened = open(&session);
    session.begin_turn("続き");
    opened.show(&session);
    let end = opened.scrolled();
    assert!(end > 1000.);

    opened.wheel(-300.);
    let away = opened.scrolled();
    assert!(away < end - 200., "{away} {end}");
    session.stream("流れてくる文章。\n\n流れてくる文章。\n\n流れてくる文章。\n\n");
    opened.show(&session);
    assert_eq!(opened.scrolled(), away);

    opened.wheel(100_000.);
    let back = opened.scrolled();
    assert!(back > end);
    session.stream("さらに流れてくる文章。\n\nさらに流れてくる文章。\n\n");
    opened.show(&session);
    assert!(opened.scrolled() > back);
}

#[test]
fn a_click_opens_a_receipt_and_a_call_and_they_stay_open_across_updates() {
    let mut session = Session::long(3);
    let opened = open(&session);
    assert!(!opened.text().contains("src/board/spec.rs"));

    let line = opened.all("receipt-line")[2];
    opened.click(line);
    assert!(opened.text().contains("src/board/spec.rs"));
    assert_eq!(opened.all("call-row").len(), 4);
    assert!(!opened.text().contains("let size = font_size();"));

    // The edit's row opens its diff.
    opened.click(opened.all("call-row")[2]);
    assert!(opened.text().contains("let size = font_size();"));
    assert_eq!(opened.all("added").len(), 2);

    session.begin_turn("次");
    session.stream("文章");
    opened.show(&session);
    assert!(opened.text().contains("let size = font_size();"));

    opened.click(opened.all("receipt-line")[2]);
    assert!(!opened.text().contains("src/board/spec.rs"));
    assert_eq!(opened.agent.expanded.get_untracked().len(), 0);
    assert_eq!(opened.agent.open_bodies.get_untracked().len(), 1);
}

#[test]
fn a_running_turn_carries_the_state_and_stop_and_an_approval_is_answered_by_key_or_click() {
    let mut session = Session::long(1);
    let opened = open(&session);
    session.begin_turn("設定を書き換えて");
    opened.show(&session);
    assert!(opened.text().contains("Running"));
    opened.click(opened.all("button")[0]);
    assert_eq!(*opened.asked.borrow(), [Asked::Stop]);
    // `Stop` did not open or close anything on its way.
    assert!(opened.agent.expanded.get_untracked().is_empty());

    session.call("Run", "cargo nextest run", None);
    opened.show(&session);
    assert!(opened.text().contains("Running cargo nextest run"));
    session.finish_call();
    session.call(
        "Write",
        "~/.config/horizon/config.toml",
        Some("リポジトリの外のファイルを書き換えます"),
    );
    opened.show(&session);
    let text = opened.text();
    assert!(text.contains("Waiting for approval"), "{text}");
    assert!(text.contains("Approval needed"), "{text}");
    assert!(
        text.contains("Write ~/.config/horizon/config.toml"),
        "{text}"
    );

    opened.asked.borrow_mut().clear();
    opened.enter();
    opened.press(
        keyboard_types::NamedKey::Escape,
        keyboard_types::Code::Escape,
        false,
    );
    opened.click(opened.all("approve")[0]);
    opened.click(opened.all("deny")[0]);
    let id = "approval-2".to_string();
    assert_eq!(
        *opened.asked.borrow(),
        [
            Asked::Approve(id.clone()),
            Asked::Deny(id.clone()),
            Asked::Approve(id.clone()),
            Asked::Deny(id),
        ]
    );

    // Open, the approval sits under its call instead: still once.
    let lines = opened.all("receipt-line");
    opened.click(*lines.last().unwrap());
    assert_eq!(opened.all("approval").len(), 1);
    assert_eq!(opened.all("call-row").len(), 1);

    session.finish_call();
    session.end_turn();
    opened.show(&session);
    let text = opened.text();
    assert!(!text.contains("Approval needed"), "{text}");
    assert!(!text.contains("Stop"), "{text}");
    assert!(text.contains("12s"), "{text}");
}

#[test]
fn the_composer_sends_on_enter_and_grows_with_its_lines() {
    let session = Session::long(1);
    let opened = open(&session);
    let one_row = opened.field_height();
    assert_eq!(one_row, 22.);

    opened.commit("日本語の");
    opened.commit("依頼");
    assert_eq!(opened.agent.typed.get_untracked(), "日本語の依頼");
    // Shift+Enter is the field's own: a new line, and a taller field.
    opened.press(
        keyboard_types::NamedKey::Enter,
        keyboard_types::Code::Enter,
        true,
    );
    opened.commit("二行目");
    assert_eq!(opened.agent.typed.get_untracked(), "日本語の依頼\n二行目");
    assert_eq!(opened.field_height(), 44.);
    assert!(opened.asked.borrow().is_empty());

    opened.enter();
    assert_eq!(
        *opened.asked.borrow(),
        [Asked::Send("日本語の依頼\n二行目".to_string())]
    );
    assert_eq!(opened.agent.typed.get_untracked(), "");
    assert_eq!(opened.field_height(), 22.);
    // The field itself is empty, not only the view's copy of it.
    opened.commit("次");
    assert_eq!(opened.agent.typed.get_untracked(), "次");

    // Nothing typed and nothing pending: Enter asks for nothing.
    opened.asked.borrow_mut().clear();
    let empty = open(&session);
    empty.enter();
    assert!(empty.asked.borrow().is_empty());
}

#[test]
fn the_thinking_dots_move_with_the_clock() {
    let mut session = Session::long(1);
    let opened = open(&session);
    session.begin_turn("考えて");
    opened.show(&session);
    let dot = opened.all("d0")[0];
    let opacity = |seconds: f64| {
        opened.app.resolve_at(seconds);
        let document = opened.app.document();
        let document = document.borrow();
        let styles = document.get_node(dot).unwrap().primary_styles().unwrap();
        styles.get_effects().opacity
    };
    let (early, late) = (opacity(0.1), opacity(0.7));
    assert!(early > late, "{early} {late}");
    assert!((0.25..=1.0).contains(&late), "{late}");
}
