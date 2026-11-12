//! One blocking request across a runtime's operation queue.
//!
//! Each call owns its reply channel. A timeout stops waiting; it does not
//! cancel an operation already queued in the daemon client. Callers retain
//! their operation-specific diagnostics and the daemon's result unchanged.

use std::time::Duration;

use crossbeam_channel::{bounded, RecvTimeoutError, Sender};
use tokio::sync::mpsc::UnboundedSender;

use super::SYNC_REPLY_TIMEOUT;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RequestError {
    NotSent,
    NoReply(RecvTimeoutError),
}

impl RequestError {
    pub(super) fn describe(self, not_sent: &str, timeout: &str, disconnected: &str) -> String {
        match self {
            Self::NotSent => not_sent,
            Self::NoReply(RecvTimeoutError::Timeout) => timeout,
            Self::NoReply(RecvTimeoutError::Disconnected) => disconnected,
        }
        .to_string()
    }
}

/// Call from a background thread: the queue may still be waiting for the
/// daemon connection. The reply payload can itself be a daemon-side error.
pub(super) fn request<Op, Reply>(
    ops: &UnboundedSender<Op>,
    make_op: impl FnOnce(Sender<Reply>) -> Op,
) -> Result<Reply, RequestError> {
    request_with_timeout(ops, make_op, SYNC_REPLY_TIMEOUT)
}

fn request_with_timeout<Op, Reply>(
    ops: &UnboundedSender<Op>,
    make_op: impl FnOnce(Sender<Reply>) -> Op,
    timeout: Duration,
) -> Result<Reply, RequestError> {
    let (reply, receive) = bounded(1);
    ops.send(make_op(reply))
        .map_err(|_| RequestError::NotSent)?;
    receive.recv_timeout(timeout).map_err(RequestError::NoReply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_queue_fails_before_waiting_for_a_reply() {
        let (ops, receiver) = tokio::sync::mpsc::unbounded_channel::<Sender<()>>();
        drop(receiver);
        assert_eq!(request(&ops, |reply| reply), Err(RequestError::NotSent));
    }

    #[test]
    fn dropping_an_accepted_requests_reply_reports_disconnection() {
        let (ops, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Sender<()>>();
        let worker = std::thread::spawn(move || drop(receiver.blocking_recv().unwrap()));
        assert_eq!(
            request(&ops, |reply| reply),
            Err(RequestError::NoReply(RecvTimeoutError::Disconnected))
        );
        worker.join().unwrap();
    }

    #[test]
    fn timing_out_does_not_remove_the_queued_operation() {
        let (ops, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Sender<()>>();
        assert_eq!(
            request_with_timeout(&ops, |reply| reply, Duration::ZERO),
            Err(RequestError::NoReply(RecvTimeoutError::Timeout))
        );
        let reply = receiver.try_recv().expect("the request remains queued");
        assert!(reply.send(()).is_err(), "the caller has stopped waiting");
    }

    #[test]
    fn daemon_errors_are_reply_payloads_and_do_not_close_the_queue() {
        let (ops, mut receiver) =
            tokio::sync::mpsc::unbounded_channel::<Sender<Result<u32, String>>>();
        let worker = std::thread::spawn(move || {
            receiver
                .blocking_recv()
                .unwrap()
                .send(Err("unknown session".into()))
                .unwrap();
            receiver.blocking_recv().unwrap().send(Ok(42)).unwrap();
        });
        assert_eq!(
            request(&ops, |reply| reply),
            Ok(Err("unknown session".into()))
        );
        assert_eq!(request(&ops, |reply| reply), Ok(Ok(42)));
        worker.join().unwrap();
    }
}
