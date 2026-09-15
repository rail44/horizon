//! Actual asynchronous work outlives a cancelled transcript call briefly.
//! Environment changes wait for these guards, not merely a cancelled receipt.
use crate::contract::SessionId;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

fn work() -> &'static Mutex<HashMap<SessionId, usize>> {
    static WORK: OnceLock<Mutex<HashMap<SessionId, usize>>> = OnceLock::new();
    WORK.get_or_init(Mutex::default)
}

pub(crate) struct WorkGuard(SessionId);

pub(crate) fn begin(session: SessionId) -> WorkGuard {
    *work()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(session)
        .or_default() += 1;
    WorkGuard(session)
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        let mut work = work()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = work.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                work.remove(&self.0);
            }
        }
    }
}

pub fn session_tool_work_settled(session: SessionId) -> bool {
    !work()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(&session)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn work_boundary_waits_for_every_actual_job_even_after_a_call_is_retired() {
        let session = SessionId::new();
        let first = begin(session);
        let second = begin(session);
        drop(first);
        assert!(!session_tool_work_settled(session));
        drop(second);
        assert!(session_tool_work_settled(session));
    }
}
