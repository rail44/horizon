//! The mouse, for tests: steps that go through the document's own hit
//! testing and dispatch.

use blitz_traits::events::UiEvent;

use crate::app::App;

/// One step of the mouse at a point of the page: what a test drives the
/// document's hit testing and dispatch with.
#[derive(Clone, Copy, PartialEq)]
pub enum Pointer {
    /// A move with no button held.
    Move,
    /// The main button goes down.
    Down,
    /// A move with the main button held.
    Drag,
    /// The main button comes up.
    Up,
}

pub fn pointer(app: &App, x: f32, y: f32, step: Pointer) {
    pointer_with(app, x, y, step, false);
}

/// [`pointer`], with the secondary button in place of the main one where
/// `secondary`.
pub fn pointer_with(app: &App, x: f32, y: f32, step: Pointer, secondary: bool) {
    use blitz_traits::events::{
        BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, PointerCoords,
    };
    let (button, held) = match secondary {
        true => (MouseEventButton::Secondary, MouseEventButtons::Secondary),
        false => (MouseEventButton::Main, MouseEventButtons::Primary),
    };
    let event = |buttons| BlitzPointerEvent {
        id: BlitzPointerId::MOUSE,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button,
        buttons,
        mods: Default::default(),
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    };
    app.dispatch(match step {
        Pointer::Move => UiEvent::PointerMove(event(MouseEventButtons::None)),
        Pointer::Down => UiEvent::PointerDown(event(held)),
        Pointer::Drag => UiEvent::PointerMove(event(held)),
        Pointer::Up => UiEvent::PointerUp(event(MouseEventButtons::None)),
    });
}

/// A press and a release of the main button at a point of the page.
pub fn click_at(app: &App, x: f32, y: f32) {
    for step in [Pointer::Move, Pointer::Down, Pointer::Up] {
        pointer(app, x, y, step);
    }
}

/// A press and a release of the secondary button at a point of the page:
/// a right-click.
pub fn right_click_at(app: &App, x: f32, y: f32) {
    for step in [Pointer::Move, Pointer::Down, Pointer::Up] {
        pointer_with(app, x, y, step, true);
    }
}

/// The pointer moved to a point of the page: what the wheel then scrolls
/// is what is under it.
pub fn move_to(app: &App, x: f32, y: f32) {
    pointer(app, x, y, Pointer::Move);
}

/// Selects text on the page as a drag over it would: from a byte of one
/// element's own lines of text to a byte of another's (or the same's).
pub fn select(app: &App, from: (crate::El, usize), to: (crate::El, usize)) {
    let document = app.document();
    (document.borrow_mut()).set_text_selection(from.0.0, from.1, to.0.0, to.1);
}

/// Whether `classes`, an element's `class` attribute, has the class a
/// sheet wrote as `class`: by that name, or by the name a CSS module gave
/// it (`class--` and the module's own mark).
pub fn has_class(classes: &str, class: &str) -> bool {
    classes.split_whitespace().any(|name| {
        name == class || (name.rsplit_once("--")).is_some_and(|(written, _)| written == class)
    })
}

/// The elements at or under `within` that have the class a sheet wrote as
/// `class` (as [`has_class`] matches it), in document order.
///
/// A test looks under the element of the view it checks -- the one the
/// view returned, a pane's, a sheet's -- and not over the whole page: a
/// class's name is its sheet's own, and any other view on the page may
/// write the same name.
pub fn find(app: &App, within: blitz_dom::NodeId, class: &str) -> Vec<blitz_dom::NodeId> {
    let document = app.document();
    let document = document.borrow();
    let mut found = Vec::new();
    let mut pending = vec![within];
    while let Some(id) = pending.pop() {
        let node = document.get_node(id).expect("a node of the page");
        let classes = node.attr(blitz_dom::local_name!("class"));
        if classes.is_some_and(|classes| has_class(classes, class)) {
            found.push(id);
        }
        pending.extend(node.children.iter().rev().copied());
    }
    found
}

/// The one element at or under `within` with the class `class`, as
/// [`find`] looks for it.
pub fn find_one(app: &App, within: blitz_dom::NodeId, class: &str) -> blitz_dom::NodeId {
    match find(app, within, class)[..] {
        [one] => one,
        ref found => panic!("{} elements of the class {class}, not one", found.len()),
    }
}

/// The words on the page: the text of everything in the body.
pub fn text(app: &App) -> String {
    let document = app.document();
    let document = document.borrow();
    let top = document.root_element();
    // A view drawn straight into the page (not in a body of its own):
    // everything outside the head, whose title is not words to show.
    let (in_body, elsewhere): (Vec<&blitz_dom::Node>, Vec<&blitz_dom::Node>) = (top
        .children
        .iter())
    .filter_map(|child| document.get_node(*child))
    .partition(|node| (node.element_data()).is_some_and(|element| &*element.name.local == "body"));
    let words = |nodes: &[&blitz_dom::Node]| -> String {
        nodes
            .iter()
            .map(|node| node.text_content())
            .collect::<Vec<_>>()
            .concat()
    };
    if in_body.is_empty() {
        words(&elsewhere)
    } else {
        words(&in_body)
    }
}
