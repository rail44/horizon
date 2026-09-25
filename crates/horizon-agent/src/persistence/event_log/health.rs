//! Sticky writer failure, broadcast to every session sharing the log.
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Default)]
pub(super) struct WriterHealth(Arc<Mutex<Health>>);

#[derive(Default)]
struct Health {
    failure: Option<String>,
    subscribers: Vec<Weak<Sender<String>>>,
}

impl WriterHealth {
    pub(super) fn failure(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .clone()
    }

    pub(super) fn fail(&self, message: String) {
        let mut health = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if health.failure.is_some() {
            return;
        }
        health.failure = Some(message.clone());
        for subscriber in health.subscribers.drain(..) {
            if let Some(subscriber) = subscriber.upgrade() {
                let _ = subscriber.send(message.clone());
            }
        }
    }

    pub(super) fn subscribe(&self) -> FailureSubscription {
        let (tx, rx) = unbounded();
        let tx = Arc::new(tx);
        let mut health = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(message) = &health.failure {
            let _ = tx.send(message.clone());
        } else {
            health
                .subscribers
                .retain(|subscriber| subscriber.strong_count() > 0);
            health.subscribers.push(Arc::downgrade(&tx));
        }
        FailureSubscription {
            receiver: rx,
            _sender: tx,
        }
    }
}

/// Keeps one broadcast subscription alive without retaining departed sessions.
pub struct FailureSubscription {
    receiver: Receiver<String>,
    _sender: Arc<Sender<String>>,
}

impl FailureSubscription {
    pub fn receiver(&self) -> &Receiver<String> {
        &self.receiver
    }
}

pub(super) struct WriterExit(pub(super) WriterHealth);
impl Drop for WriterExit {
    fn drop(&mut self) {
        self.0.fail("Event log writer stopped".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_reaches_every_session_and_late_subscribers() {
        let health = WriterHealth::default();
        let first = health.subscribe();
        let second = health.subscribe();
        health.fail("disk full".into());
        health.fail("later error".into());
        let late = health.subscribe();
        for listener in [&first, &second, &late] {
            assert_eq!(listener.receiver().try_recv().unwrap(), "disk full");
            assert!(listener.receiver().try_recv().is_err());
        }
    }
}
