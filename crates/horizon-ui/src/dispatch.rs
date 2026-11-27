//! An event's way through the handlers: inwards to its target first, then
//! outwards from it.

use std::cell::Cell;
use std::rc::Rc;

use blitz_dom::{Document, EventHandler, NodeId};
use blitz_traits::events::{DomEvent, DomEventData, EventState};

use crate::El;
use crate::shared::{Handler, Handlers, Shared};

thread_local! {
    static PREVENTED: Cell<bool> = const { Cell::new(false) };
    static STOPPED: Cell<bool> = const { Cell::new(false) };
    static CAPTURING: Cell<Option<NodeId>> = const { Cell::new(None) };
}

/// From a handler: the document does not do what it would have done with
/// the event itself (a text field does not take the key).
pub fn prevent_default() {
    PREVENTED.set(true);
}

/// From a handler: the event goes to no element further out.
pub fn stop_propagation() {
    STOPPED.set(true);
}

/// From a pointer's handler (its press): the pointer's moves and its
/// release go to `element` until its button comes up, wherever the
/// pointer is, and nothing else under it is hovered meanwhile.
pub fn capture_pointer(element: El) {
    CAPTURING.set(Some(element.0));
}

/// The capture a handler asked for, given to the pointer of `event`.
fn captured(event: &DomEvent, document: &mut dyn Document) {
    let Some(node) = CAPTURING.take() else {
        return;
    };
    let (DomEventData::PointerDown(pointer) | DomEventData::PointerMove(pointer)) = &event.data
    else {
        return;
    };
    document.inner_mut().set_pointer_capture(node, pointer.id);
}

pub(crate) struct Dispatch(pub(crate) Rc<Shared>);

impl EventHandler for Dispatch {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        document: &mut dyn Document,
        state: &mut EventState,
    ) {
        // Cloned out: a handler may add handlers (a row made in it).
        let named = |handlers: &Handlers, node: &NodeId, name: &str| {
            (handlers.borrow().get(node).into_iter().flatten())
                .filter(|(event, _)| *event == name)
                .map(|(_, handler)| handler.clone())
                .collect::<Vec<Handler>>()
        };
        // In from the outermost element to the target, whose own capturing
        // handlers run as the DOM's do.
        for node in chain.iter().rev() {
            for handler in named(&self.0.captures, node, event.name()) {
                handler(event);
            }
            captured(event, document);
            if PREVENTED.replace(false) {
                state.prevent_default();
                self.0.prevented.set(true);
            }
            if STOPPED.replace(false) {
                return;
            }
        }
        for node in chain {
            let handlers = named(&self.0.handlers, node, event.name());
            for handler in handlers {
                handler(event);
            }
            captured(event, document);
            if PREVENTED.replace(false) {
                state.prevent_default();
                self.0.prevented.set(true);
            }
            if STOPPED.replace(false) || !event.bubbles {
                break;
            }
        }
    }
}
