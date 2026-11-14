//! The agent pane, driven the way a session and an owner drive it.

use std::cell::RefCell;
use std::rc::Rc;

use horizon::agent::{Agent, Asked, view};
use horizon_theme_css::{Theme, themed};
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, BlitzWheelEvent, KeyState, PointerCoords,
    UiEvent,
};
use horizon_ui::reactive_graph::traits::GetUntracked;
use horizon_ui::testing::{click_at, move_to};

mod sample;

use sample::Session;

struct Opened {
    app: App,
    agent: Agent,
    /// What the view is given to show.
    shown: sample::Given,
    asked: Rc<RefCell<Vec<Asked>>>,
}

fn open(session: &Session) -> Opened {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = asked.clone();
    let mut agent = None;
    // (Made on the page: a signal is its page's.)
    let mut given = None;
    // The theme with no config file, at its 13 px: the measurements below
    // are the type scale's at that size.
    let app = App::new(900, 700, horizon_theme_css::CSS, || {
        let shown = sample::Given::new_local(None);
        given = Some(shown);
        let (root, state) = view(shown.into(), move |asked| log.borrow_mut().push(asked));
        state.field.focus();
        agent = Some(state);
        themed(Theme::built_in, root)
    });
    let opened = Opened {
        app,
        agent: agent.unwrap(),
        shown: given.unwrap(),
        asked,
    };
    opened.show(session);
    opened
}

fn has_class(document: &BaseDocument, id: NodeId, class: &str) -> bool {
    (document.get_node(id))
        .and_then(|node| node.attr(local_name!("class")))
        .is_some_and(|classes| horizon_ui::testing::has_class(classes, class))
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
        session.show(self.shown);
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
    // Of the 60 units, the ones near the end are elements.
    let units = opened.all("unit");
    let (start, end) = opened.agent.built.get_untracked();
    assert_eq!(end, 60);
    assert_eq!(units.len(), end - start);
    assert!((4..30).contains(&units.len()), "{}", units.len());
    // The column ends where the composer starts, less its own padding.
    let column_bottom = opened.span(opened.all("composer")[0]).0 - 30.;
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
    // The ended turns still built kept their elements.
    let grown = opened.all("unit");
    assert!(units.iter().rev().take(3).all(|unit| grown.contains(unit)));
    for chunk in 0..40 {
        session.stream(&format!("{chunk} 行目の文章が流れてきます。\n\n"));
        opened.show(&session);
    }
    let after = opened.all("unit");
    assert_eq!(opened.agent.built.get_untracked().1, 62);
    // The turn in progress has grown past what is kept built above the
    // view: the units before it are no longer elements.
    assert!(!units.contains(after.last().unwrap()));
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
    // Where the last unit ends on the page: at the column's end while the
    // column is followed. (The scroll offset itself is not compared: it
    // also moves when units above the view are built at their real
    // heights in place of guessed ones.)
    let last_bottom = || opened.span(*opened.all("unit").last().unwrap()).1;
    let column_bottom = opened.span(opened.all("composer")[0]).0 - 30.;
    let at_end = |bottom: f64| (bottom - column_bottom).abs() <= 1.;
    assert!(at_end(last_bottom()), "{} {column_bottom}", last_bottom());

    opened.wheel(-300.);
    let away = last_bottom();
    assert!((away - column_bottom - 300.).abs() <= 1., "{away}");
    // What is in view stays where it is while the turn below it grows.
    let in_view = opened.all("unit")[0];
    let place = opened.span(in_view);
    session.stream("流れてくる文章。\n\n流れてくる文章。\n\n流れてくる文章。\n\n");
    opened.show(&session);
    assert_eq!(opened.all("unit")[0], in_view);
    assert_eq!(opened.span(in_view), place);
    assert!(last_bottom() > away + 50.);

    opened.wheel(100_000.);
    assert!(at_end(last_bottom()), "{}", last_bottom());
    session.stream("さらに流れてくる文章。\n\nさらに流れてくる文章。\n\n");
    opened.show(&session);
    assert!(at_end(last_bottom()), "{}", last_bottom());
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
    // `Stop` took the focus when pressed; the keys answer from the field.
    opened.click(opened.agent.field.0);
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
    assert_eq!(one_row, 24.);

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
    assert_eq!(opened.field_height(), 48.);
    assert!(opened.asked.borrow().is_empty());

    opened.enter();
    assert_eq!(
        *opened.asked.borrow(),
        [Asked::Send("日本語の依頼\n二行目".to_string())]
    );
    assert_eq!(opened.agent.typed.get_untracked(), "");
    assert_eq!(opened.field_height(), 24.);
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
    let dot = opened.all("d")[0];
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

#[test]
fn a_long_history_is_scrolled_through_with_few_units_built() {
    let session = Session::long(200);
    let opened = open(&session);
    let built = || opened.agent.built.get_untracked();
    assert_eq!(built().1, 400);
    assert!(built().1 - built().0 < 40, "{:?}", built());

    // Up to the top, a wheel step at a time. Units come into being above
    // the view at heights that were guessed; what is in view moves by the
    // step all the same, and no further once the step is over.
    let mut steps = 0;
    while opened.scrolled() > 0. {
        let before = opened.all("unit");
        let places: Vec<_> = before.iter().map(|unit| opened.span(*unit)).collect();
        let was = opened.scrolled();
        opened.wheel(-400.);
        let after = opened.all("unit");
        assert!(after.len() < 40, "{}", after.len());
        // The lowest unit still built is one that was in or near the view.
        let (unit, place) = (before.iter().zip(&places))
            .rfind(|(unit, _)| after.contains(unit))
            .expect("a unit built before and after");
        let moved = opened.span(*unit).0 - place.0;
        // The wheel is taken whole unless the top is nearer than that.
        let step = was.min(400.);
        if was > 2000. {
            assert!((moved - step).abs() < 1., "{moved} at {was}");
        }
        opened.app.resolve();
        assert_eq!(opened.span(*unit).0 - place.0, moved);
        steps += 1;
        assert!(steps < 400, "still at {}", opened.scrolled());
    }
    assert_eq!(built().0, 0);
    assert!(opened.text().contains("0 番目の依頼"));
    let first = opened.all("unit")[0];
    assert_eq!(opened.span(first).0, 22.);

    // A receipt opened here is open again when the view comes back to it,
    // its elements having been dropped and built in between.
    opened.click(opened.all("receipt-line")[0]);
    assert!(opened.text().contains("src/board/spec.rs"));
    opened.wheel(1_000_000.);
    assert_eq!(built().1, 400);
    assert!(built().0 > 300, "{:?}", built());
    assert!(!opened.text().contains("src/board/spec.rs"));
    let mut steps = 0;
    while opened.scrolled() > 0. {
        opened.wheel(-2000.);
        steps += 1;
        assert!(steps < 400, "still at {}", opened.scrolled());
    }
    assert!(opened.text().contains("src/board/spec.rs"));
}

/// What the document put on the clipboard.
#[derive(Default)]
struct Clipboard(std::sync::Mutex<Vec<String>>);

impl horizon_ui::blitz_traits::shell::ShellProvider for Clipboard {
    fn set_clipboard_text(
        &self,
        text: String,
    ) -> Result<(), horizon_ui::blitz_traits::shell::ClipboardError> {
        self.0.lock().unwrap().push(text);
        Ok(())
    }
}

impl Opened {
    fn focused(&self) -> Option<NodeId> {
        self.app.document().borrow().get_focussed_node_id()
    }

    fn ctrl_c(&self) {
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key: keyboard_types::Key::Character("c".into()),
            code: keyboard_types::Code::KeyC,
            modifiers: keyboard_types::Modifiers::CONTROL,
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }));
        self.app.resolve();
    }

    /// Drags across the middle of a node's box, from its left edge to its
    /// right, and lets go.
    fn drag_across(&self, node: NodeId) {
        use horizon_ui::testing::{Pointer, pointer};
        let (x, y, width, height) = {
            let document = self.app.document();
            let document = document.borrow();
            let bounds = document.get_client_bounding_rect(node).expect("a box");
            (bounds.x, bounds.y, bounds.width, bounds.height)
        };
        let y = (y + height / 2.) as f32;
        let (from, to) = ((x + 4.) as f32, (x + width - 4.) as f32);
        pointer(&self.app, from, y, Pointer::Move);
        pointer(&self.app, from, y, Pointer::Down);
        pointer(&self.app, to, y, Pointer::Drag);
        pointer(&self.app, to, y, Pointer::Up);
        self.app.resolve();
    }
}

#[test]
fn text_selected_in_the_conversation_is_copied_while_the_field_had_the_focus() {
    let session = Session::long(1);
    let opened = open(&session);
    let clipboard = std::sync::Arc::new(Clipboard::default());
    (opened.app.document().borrow_mut()).set_shell_provider(clipboard.clone());
    assert_eq!(opened.focused(), Some(opened.agent.field.0));

    let request = opened.all("user")[0];
    opened.drag_across(request);
    // The press took the focus out of the field, into the view.
    assert_eq!(opened.focused(), Some(opened.all("agent")[0]));
    let selected = (opened.app.document().borrow().get_selected_text()).expect("a selection");
    assert!(selected.contains("番目の依頼"), "{selected}");
    opened.ctrl_c();
    assert_eq!(*clipboard.0.lock().unwrap(), [selected]);
}

#[test]
fn enter_goes_into_the_field_and_escape_comes_out() {
    let mut session = Session::long(1);
    let opened = open(&session);
    let view = opened.all("agent")[0];
    opened.click(opened.all("user")[0]);
    assert_eq!(opened.focused(), Some(view));

    opened.enter();
    assert_eq!(opened.focused(), Some(opened.agent.field.0));
    // What is typed then is the field's, and Enter in it sends.
    opened.commit("続けて");
    opened.enter();
    assert_eq!(*opened.asked.borrow(), [Asked::Send("続けて".into())]);
    assert_eq!(opened.focused(), Some(opened.agent.field.0));

    opened.press(
        keyboard_types::NamedKey::Escape,
        keyboard_types::Code::Escape,
        false,
    );
    assert_eq!(opened.focused(), Some(view));

    // Waiting on an approval with nothing typed, Escape in the field
    // denies it and stays.
    session.begin_turn("設定を書き換えて");
    session.call(
        "Write",
        "~/.config/horizon/config.toml",
        Some("外のファイル"),
    );
    opened.show(&session);
    opened.enter();
    opened.asked.borrow_mut().clear();
    opened.press(
        keyboard_types::NamedKey::Escape,
        keyboard_types::Code::Escape,
        false,
    );
    assert!(
        matches!(opened.asked.borrow().as_slice(), [Asked::Deny(_)]),
        "{:?}",
        opened.asked.borrow()
    );
    assert_eq!(opened.focused(), Some(opened.agent.field.0));
}
