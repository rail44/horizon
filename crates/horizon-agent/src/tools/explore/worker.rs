//! A watcher owns its child, including stop-before-attachment and unwinding.
use super::ExplorationHost;
use crate::contract::SessionId;
use crate::tools::background::Registration;
use crossbeam_channel::Receiver;
use std::sync::Arc;
use std::time::Duration;

pub(crate) struct ChildWork {
    work: Registration,
    host: Arc<dyn ExplorationHost>,
    child: SessionId,
    cancelled: Receiver<()>,
}
impl ChildWork {
    pub(crate) fn attach(
        work: Registration,
        host: Arc<dyn ExplorationHost>,
        child: SessionId,
    ) -> Self {
        let (cancel, cancelled) = crossbeam_channel::bounded(1);
        let stop_host = host.clone();
        work.on_stop(move || {
            let _ = cancel.try_send(());
            stop_host.terminate(child);
        });
        Self {
            work,
            host,
            child,
            cancelled,
        }
    }
    pub(crate) fn cancelled(&self) -> &Receiver<()> {
        &self.cancelled
    }
    pub(crate) fn is_cancelled(&self) -> bool {
        self.work.is_cancelled()
    }
    pub(crate) fn finish(&self) -> bool {
        self.work.finish()
    }
}
impl Drop for ChildWork {
    fn drop(&mut self) {
        // finish also stops the child on an early return or watcher panic.
        self.work.finish();
        if self.work.is_cancelled() {
            super::children::remove_child(self.child);
        }
        while !self
            .host
            .wait_stopped(self.child, Duration::from_millis(100))
        {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::background::{self, Lifetime, WorkKind};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct DelayedHost {
        stopped: AtomicBool,
        stops: AtomicUsize,
    }
    impl ExplorationHost for DelayedHost {
        fn start(
            &self,
            _: super::super::ExplorationRequest,
        ) -> Result<super::super::StartedExploration, String> {
            unreachable!()
        }
        fn terminate(&self, _: SessionId) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
        fn wait_stopped(&self, _: SessionId, timeout: Duration) -> bool {
            if self.stopped.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(timeout);
            self.stopped.load(Ordering::SeqCst)
        }
    }
    #[test]
    fn parent_drain_waits_for_child_exit_not_just_the_termination_request() {
        let session = SessionId::new();
        let child = SessionId::new();
        let host = Arc::new(DelayedHost::default());
        let work = Registration::new(session, Lifetime::Session, WorkKind::Child);
        let child_work = ChildWork::attach(work, host.clone(), child);
        background::close_session(session);
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);
        let waiter = std::thread::spawn(move || drop(child_work));
        assert!(!background::drain_session_work(
            session,
            Duration::from_millis(10)
        ));
        host.stopped.store(true, Ordering::SeqCst);
        assert!(background::drain_session_work(
            session,
            Duration::from_secs(2)
        ));
        waiter.join().unwrap();
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn a_child_acquired_after_parent_teardown_is_stopped_and_forgotten() {
        let session = SessionId::new();
        let child = SessionId::new();
        let host = Arc::new(DelayedHost::default());
        host.stopped.store(true, Ordering::SeqCst);
        let work = Registration::new(session, Lifetime::Session, WorkKind::Child);
        background::close_session(session);
        super::super::children::register(session, child, "late".into());
        let child_work = ChildWork::attach(work, host.clone(), child);
        assert!(!child_work.finish());
        drop(child_work);
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);
        assert!(matches!(
            super::super::children::lookup(session, child),
            super::super::children::Lookup::Unknown
        ));
        assert!(background::drain_session_work(session, Duration::ZERO));
    }
    #[test]
    fn a_panicking_watcher_stops_its_child_exactly_once() {
        let session = SessionId::new();
        let host = Arc::new(DelayedHost::default());
        host.stopped.store(true, Ordering::SeqCst);
        let work = Registration::new(session, Lifetime::Session, WorkKind::Child);
        let child_work = ChildWork::attach(work, host.clone(), SessionId::new());
        let waiter = std::thread::spawn(move || {
            let _child = child_work;
            panic!("watcher failed");
        });
        assert!(waiter.join().is_err());
        assert!(background::drain_session_work(
            session,
            Duration::from_secs(2)
        ));
        assert_eq!(host.stops.load(Ordering::SeqCst), 1);
    }
}
