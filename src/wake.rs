//! Who to wake so that the page is polled and drawn again: handed to
//! whatever changes outside the document, on another thread.

use std::sync::{Arc, Mutex};
use std::task::Waker;

#[derive(Clone, Default)]
pub struct Wake(Arc<Mutex<Option<Waker>>>);

impl Wake {
    /// The waker to use from now on.
    pub fn set(&self, waker: Waker) {
        *self.0.lock().unwrap() = Some(waker);
    }

    /// Wakes the page, if it has given its waker yet.
    pub fn wake(&self) {
        if let Some(waker) = self.0.lock().unwrap().as_ref() {
            waker.wake_by_ref();
        }
    }
}
