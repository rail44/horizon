//! Process-wide registry of live subscribe-stream subscribers, and the
//! fan-out path from `ingest` appends to their connections.
//!
//! Built once in `main` and shared (via `Arc`) across every accepted
//! connection — both the remoc `ingest` path (which calls [`notify`] after
//! each append) and the raw NDJSON `subscribe` path (which calls
//! [`register`] on connect). The registry keys subscribers by the log path
//! they're watching (`None` = all paths), so an ingest appending to `/foo`
//! reaches both `/foo`-specific subscribers and all-paths subscribers.
//!
//! Pokes are lossy (`docs/logd-design.md` decision 3): a subscriber whose
//! channel is full loses the poke (the channel is small and pokes are tiny);
//! a subscriber whose channel is closed is dropped from the registry on the
//! next `notify`. `notify` never blocks — the ingest path must not wait on
//! a stuck subscriber.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use horizon_board::wire::SubscribePoke;
use tokio::sync::mpsc::{self, error::TrySendError};

/// How many pokes a subscriber's channel buffers before the oldest is
/// dropped. Pokes are tiny (a log type string + a u64 seq); 64 is enough
/// headroom for a consumer that's briefly slow (e.g. blocked on stdout)
/// without growing unbounded memory for a permanently-stuck one.
const POKE_CHANNEL_CAPACITY: usize = 64;

pub struct SubscriberRegistry {
    subscribers: Mutex<HashMap<Option<PathBuf>, Vec<mpsc::Sender<SubscribePoke>>>>,
}

impl SubscriberRegistry {
    pub fn new() -> Self {
        Self {
            subscribers: Mutex::new(HashMap::new()),
        }
    }

    /// Registers a new subscriber for `key` (`Some(path)` = watch that path
    /// only; `None` = watch all paths) and returns the receiver it reads
    /// pokes from. When the subscriber disconnects, the sender's `try_send`
    /// fails with `Closed` and the next [`notify`] cleans it up — no
    /// explicit deregister is needed.
    pub fn register(&self, key: Option<PathBuf>) -> mpsc::Receiver<SubscribePoke> {
        let (tx, rx) = mpsc::channel(POKE_CHANNEL_CAPACITY);
        let mut subs = self.subscribers.lock().unwrap();
        subs.entry(key).or_default().push(tx);
        rx
    }

    /// Fans `poke` out to every subscriber watching `path` plus every
    /// all-paths subscriber. Lossy and non-blocking: a full channel drops
    /// the poke; a closed channel is removed from the registry.
    pub fn notify(&self, path: &Path, poke: &SubscribePoke) {
        let mut subs = self.subscribers.lock().unwrap();
        for key in [Some(path.to_path_buf()), None] {
            if let Some(senders) = subs.get_mut(&key) {
                let mut i = 0;
                while i < senders.len() {
                    match senders[i].try_send(poke.clone()) {
                        Ok(()) => i += 1,
                        // Alive but behind — drop this poke, keep the subscriber.
                        Err(TrySendError::Full(_)) => i += 1,
                        // Gone — remove from the registry.
                        Err(TrySendError::Closed(_)) => {
                            senders.swap_remove(i);
                        }
                    }
                }
            }
        }
    }
}

impl Default for SubscriberRegistry {
    fn default() -> Self {
        Self::new()
    }
}
