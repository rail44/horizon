//! Which one of many a signal names, asked by each of the many.
//!
//! A list whose rows each read the signal that says which row is selected
//! runs every row's effects when the selection moves. Asked through
//! [`OneOf`], a row is told only when it becomes the one or stops being
//! it.
//!
//! (`reactive_graph`'s `Selector` does the same with an effect that runs
//! on the executor's other threads, after the effects of this thread's
//! frame have run.)

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;

use reactive_graph::effect::Effect;
use reactive_graph::signal::ArcRwSignal;
use reactive_graph::traits::{Get, Set};

struct State<K> {
    now: Option<K>,
    /// A signal for each key asked about so far: whether it is the one.
    asked: HashMap<K, ArcRwSignal<bool>>,
}

pub struct OneOf<K>(Rc<RefCell<State<K>>>);

impl<K> Clone for OneOf<K> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<K: Eq + Hash + Clone + 'static> OneOf<K> {
    /// Follows `source`, for as long as what it is made under lasts.
    pub fn new(source: impl Fn() -> K + 'static) -> Self {
        let state = Rc::new(RefCell::new(State {
            now: None,
            asked: HashMap::new(),
        }));
        let followed = state.clone();
        Effect::new(move |_| {
            let next = source();
            let mut state = followed.borrow_mut();
            let before = state.now.replace(next.clone());
            if before.as_ref() == Some(&next) {
                return;
            }
            let left = before.and_then(|before| state.asked.get(&before).cloned());
            let reached = state.asked.get(&next).cloned();
            drop(state);
            if let Some(left) = left {
                left.set(false);
            }
            if let Some(reached) = reached {
                reached.set(true);
            }
        });
        Self(state)
    }

    /// Whether `key` is the one `source` names. Read in an effect, the
    /// effect runs again when that changes, and not when the one moves
    /// between other keys.
    pub fn is(&self, key: &K) -> bool {
        let signal = {
            let mut state = self.0.borrow_mut();
            let now = state.now.as_ref() == Some(key);
            (state.asked.entry(key.clone()))
                .or_insert_with(|| ArcRwSignal::new(now))
                .clone()
        };
        signal.get()
    }
}
