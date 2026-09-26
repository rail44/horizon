//! What the two views report upwards.
//!
//! Neither view reaches the shell: opening a pane, resolving a session, and
//! adopting one are the workspace's business, so a view states the request
//! as an event and whatever holds it decides. A preview holds nothing, so
//! an unhandled event there is simply a no-op (the views also write a
//! notice line in the guest build, which is what a windowless check reads).

// Nothing in this build reads an event's payload: the views emit them, and
// the subscriber that acts on them is the shell's, which is not wired to
// these two views yet.
#![allow(dead_code)]

use gpui::EventEmitter;
use horizon_workspace::SessionId;

use super::{BoardListView, BoardThreadView};

/// Open task `0`'s thread. The list emits it; a thread lives in a pane of
/// its own, so the list never renders one itself.
pub(crate) struct OpenTaskThread(pub(crate) u64);

/// Attach the agent session bound to the open task. The thread view emits
/// it; resolving the id against the running runtime is the shell's.
pub(crate) struct OpenTaskSession(pub(crate) SessionId);

/// The session ids the board's tasks name, reported after a load found
/// bindings the emitter has not reported yet. Never a request to open a
/// pane — the shell answers it by resolving those ids in its inventory.
pub(crate) struct BoardSessionsRefreshed(pub(crate) Vec<SessionId>);

impl EventEmitter<OpenTaskThread> for BoardListView {}
impl EventEmitter<BoardSessionsRefreshed> for BoardListView {}
impl EventEmitter<OpenTaskSession> for BoardThreadView {}
impl EventEmitter<BoardSessionsRefreshed> for BoardThreadView {}
