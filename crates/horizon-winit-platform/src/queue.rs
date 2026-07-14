//! Multi-priority MPMC queue for `dispatcher.rs`, vendored from gpui's
//! `src/queue.rs` (zed-industries/zed @ 5f8a741). gpui only compiles and
//! re-exports `PriorityQueueSender`/`PriorityQueueReceiver` on
//! Windows/Linux/wasm — its macOS platform dispatches through Grand
//! Central Dispatch and never needs them — so a macOS build of this crate
//! cannot import them; this copy keeps the dispatcher identical on every
//! OS. The weighted-random pop (a loaded die over `Priority::weight()`,
//! see <https://www.keithschwarz.com/darts-dice-coins/>) is preserved
//! verbatim so scheduling fairness matches gpui's own dispatchers; the
//! `spin_*`/`try_iter` variants this crate never calls are dropped.

use std::{
    collections::VecDeque,
    fmt,
    iter::FusedIterator,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use gpui::Priority;
use rand::{rngs::SmallRng, Rng, SeedableRng};

struct PriorityQueues<T> {
    high_priority: VecDeque<T>,
    medium_priority: VecDeque<T>,
    low_priority: VecDeque<T>,
}

impl<T> PriorityQueues<T> {
    fn is_empty(&self) -> bool {
        self.high_priority.is_empty()
            && self.medium_priority.is_empty()
            && self.low_priority.is_empty()
    }
}

struct PriorityQueueState<T> {
    queues: parking_lot::Mutex<PriorityQueues<T>>,
    condvar: parking_lot::Condvar,
    receiver_count: AtomicUsize,
    sender_count: AtomicUsize,
}

impl<T> PriorityQueueState<T> {
    fn send(&self, priority: Priority, item: T) -> Result<(), SendError<T>> {
        if self.receiver_count.load(Ordering::Relaxed) == 0 {
            return Err(SendError(item));
        }

        let mut queues = self.queues.lock();
        match priority {
            Priority::RealtimeAudio => unreachable!(
                "Realtime audio priority runs on a dedicated thread and is never queued"
            ),
            Priority::High => queues.high_priority.push_back(item),
            Priority::Medium => queues.medium_priority.push_back(item),
            Priority::Low => queues.low_priority.push_back(item),
        }
        self.condvar.notify_one();
        Ok(())
    }

    fn recv(&self) -> Result<parking_lot::MutexGuard<'_, PriorityQueues<T>>, RecvError> {
        let mut queues = self.queues.lock();

        if queues.is_empty() && self.sender_count.load(Ordering::Relaxed) == 0 {
            return Err(RecvError);
        }

        while queues.is_empty() {
            self.condvar.wait(&mut queues);
        }

        Ok(queues)
    }

    fn try_recv(
        &self,
    ) -> Result<Option<parking_lot::MutexGuard<'_, PriorityQueues<T>>>, RecvError> {
        let queues = self.queues.lock();

        if queues.is_empty() && self.sender_count.load(Ordering::Relaxed) == 0 {
            return Err(RecvError);
        }

        if queues.is_empty() {
            Ok(None)
        } else {
            Ok(Some(queues))
        }
    }
}

pub(crate) struct PriorityQueueSender<T> {
    state: Arc<PriorityQueueState<T>>,
}

impl<T> PriorityQueueSender<T> {
    pub(crate) fn send(&self, priority: Priority, item: T) -> Result<(), SendError<T>> {
        self.state.send(priority, item)
    }
}

impl<T> Drop for PriorityQueueSender<T> {
    fn drop(&mut self) {
        self.state.sender_count.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct PriorityQueueReceiver<T> {
    state: Arc<PriorityQueueState<T>>,
    rand: SmallRng,
}

impl<T> Clone for PriorityQueueReceiver<T> {
    fn clone(&self) -> Self {
        self.state.receiver_count.fetch_add(1, Ordering::AcqRel);
        Self {
            state: Arc::clone(&self.state),
            rand: SmallRng::seed_from_u64(0),
        }
    }
}

pub(crate) struct SendError<T>(pub(crate) T);

impl<T: fmt::Debug> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SendError").field(&self.0).finish()
    }
}

#[derive(Debug)]
pub(crate) struct RecvError;

impl<T> PriorityQueueReceiver<T> {
    pub(crate) fn new() -> (PriorityQueueSender<T>, Self) {
        let state = Arc::new(PriorityQueueState {
            queues: parking_lot::Mutex::new(PriorityQueues {
                high_priority: VecDeque::new(),
                medium_priority: VecDeque::new(),
                low_priority: VecDeque::new(),
            }),
            condvar: parking_lot::Condvar::new(),
            receiver_count: AtomicUsize::new(1),
            sender_count: AtomicUsize::new(1),
        });

        let sender = PriorityQueueSender {
            state: Arc::clone(&state),
        };
        let receiver = PriorityQueueReceiver {
            state,
            rand: SmallRng::seed_from_u64(0),
        };

        (sender, receiver)
    }

    /// Tries to pop one element without blocking; `Ok(None)` when the
    /// queue is currently empty.
    ///
    /// # Errors
    ///
    /// If every sender was dropped and the queue is empty.
    pub(crate) fn try_pop(&mut self) -> Result<Option<T>, RecvError> {
        self.pop_inner(false)
    }

    /// Pops an element, blocking until one is available.
    ///
    /// # Errors
    ///
    /// If every sender was dropped and the queue is empty.
    fn pop(&mut self) -> Result<T, RecvError> {
        // With non-empty queues the loaded die always lands on some
        // non-empty lane (the last candidate's flip is `w/w`), so
        // `pop_inner(true)` never returns `Ok(None)`.
        self.pop_inner(true).map(|e| e.unwrap())
    }

    /// Returns a blocking iterator over the queue's elements; ends when
    /// every sender has been dropped and the queue is drained.
    pub(crate) fn iter(self) -> Iter<T> {
        Iter(self)
    }

    #[inline(always)]
    // algorithm is the loaded die from biased coin from
    // https://www.keithschwarz.com/darts-dice-coins/
    fn pop_inner(&mut self, block: bool) -> Result<Option<T>, RecvError> {
        use Priority as P;

        let mut queues = if !block {
            let Some(queues) = self.state.try_recv()? else {
                return Ok(None);
            };
            queues
        } else {
            self.state.recv()?
        };

        let high = P::High.weight() * !queues.high_priority.is_empty() as u32;
        let medium = P::Medium.weight() * !queues.medium_priority.is_empty() as u32;
        let low = P::Low.weight() * !queues.low_priority.is_empty() as u32;
        let mut mass = high + medium + low;

        if !queues.high_priority.is_empty() {
            let flip = self.rand.random_ratio(P::High.weight(), mass);
            if flip {
                return Ok(queues.high_priority.pop_front());
            }
            mass -= P::High.weight();
        }

        if !queues.medium_priority.is_empty() {
            let flip = self.rand.random_ratio(P::Medium.weight(), mass);
            if flip {
                return Ok(queues.medium_priority.pop_front());
            }
            mass -= P::Medium.weight();
        }

        if !queues.low_priority.is_empty() {
            let flip = self.rand.random_ratio(P::Low.weight(), mass);
            if flip {
                return Ok(queues.low_priority.pop_front());
            }
        }

        Ok(None)
    }
}

impl<T> Drop for PriorityQueueReceiver<T> {
    fn drop(&mut self) {
        self.state.receiver_count.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct Iter<T>(PriorityQueueReceiver<T>);

impl<T> Iterator for Iter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.pop().ok()
    }
}

impl<T> FusedIterator for Iter<T> {}
