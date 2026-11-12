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
    use blitz_traits::events::{
        BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, PointerCoords,
    };
    let event = |buttons| BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons,
        mods: Default::default(),
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    };
    app.dispatch(match step {
        Pointer::Move => UiEvent::PointerMove(event(MouseEventButtons::None)),
        Pointer::Down => UiEvent::PointerDown(event(MouseEventButtons::Primary)),
        Pointer::Drag => UiEvent::PointerMove(event(MouseEventButtons::Primary)),
        Pointer::Up => UiEvent::PointerUp(event(MouseEventButtons::None)),
    });
}

/// A press and a release of the main button at a point of the page.
pub fn click_at(app: &App, x: f32, y: f32) {
    for step in [Pointer::Move, Pointer::Down, Pointer::Up] {
        pointer(app, x, y, step);
    }
}

/// The pointer moved to a point of the page: what the wheel then scrolls
/// is what is under it.
pub fn move_to(app: &App, x: f32, y: f32) {
    pointer(app, x, y, Pointer::Move);
}
