//! An event's way through the handlers: inwards to its target first, then
//! outwards from it.

use std::cell::Cell;
use std::rc::Rc;

use blitz_dom::{Document, EventHandler, NodeId};
use blitz_traits::events::{DomEvent, EventState};

use crate::shared::{Handler, Handlers, Shared};

thread_local! {
    static PREVENTED: Cell<bool> = const { Cell::new(false) };
    static STOPPED: Cell<bool> = const { Cell::new(false) };
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

pub(crate) struct Dispatch(pub(crate) Rc<Shared>);

impl EventHandler for Dispatch {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        _document: &mut dyn Document,
        state: &mut EventState,
    ) {
        // Cloned out: a handler may add handlers (a row made in it).
        let named = |handlers: &Handlers, node: &NodeId, name: &str| {
            (handlers.borrow().get(node).into_iter().flatten())
                .filter(|(event, _)| *event == name)
                .map(|(_, handler)| handler.clone())
                .collect::<Vec<Handler>>()
        };
        // In from the outermost element to the target's parent.
        for node in chain.iter().skip(1).rev() {
            for handler in named(&self.0.captures, node, event.name()) {
                handler(event);
            }
            if PREVENTED.replace(false) {
                state.prevent_default();
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
            if PREVENTED.replace(false) {
                state.prevent_default();
            }
            if STOPPED.replace(false) || !event.bubbles {
                break;
            }
        }
    }
}
