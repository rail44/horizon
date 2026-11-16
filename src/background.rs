//! Work done off the UI thread, and what is done on the UI thread with
//! what it came back with: how a view reads a store or asks a daemon
//! without the page waiting.

use std::sync::{Arc, Mutex};

use crate::wake::Wake;

/// What is done on the UI thread.
type Arrived = Box<dyn FnOnce() + Send>;

#[derive(Clone, Default)]
pub struct Background {
    arrived: Arc<Mutex<Vec<Arrived>>>,
    wake: Wake,
}

impl Background {
    /// `wake` makes the page poll, so that what arrived is taken in.
    pub fn new(wake: Wake) -> Self {
        Self {
            arrived: Arc::default(),
            wake,
        }
    }

    /// Runs `work` on a thread of its own, and `then` on the UI thread
    /// with what it came back with.
    pub fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(T) + Send + 'static,
    ) {
        let this = self.clone();
        std::thread::spawn(move || {
            let came = work();
            this.tell(move || then(came));
        });
    }

    /// From any thread: `then` runs on the UI thread at its next taking
    /// in.
    pub fn tell(&self, then: impl FnOnce() + Send + 'static) {
        self.arrived.lock().unwrap().push(Box::new(then));
        self.wake.wake();
    }

    /// On the UI thread: runs what has arrived. Whether anything had.
    pub fn take_in(&self) -> bool {
        let arrived = std::mem::take(&mut *self.arrived.lock().unwrap());
        let any = !arrived.is_empty();
        for arrived in arrived {
            arrived();
        }
        any
    }
}
