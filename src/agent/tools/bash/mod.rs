//! The `bash` tool (`docs/agent-tools-design.md`, "Bash Semantics"): a fresh
//! `bash -c` process per call, with the working directory tracked by the
//! harness across calls (not a persistent shell). Unlike `fs.write`/
//! `fs.edit` — the other `RequireApproval` tools Horizon executes app-side —
//! a bash command can run for up to its timeout (120s default, 600s hard
//! cap), so it can never run synchronously on the UI thread the way those
//! do. See `agent::tools::approval::ApprovalOutcome::Started` for the split
//! this forces: approval folds a "running" frame immediately and kicks off
//! this module's `spawn`, whose eventual result is delivered back to the UI
//! thread over a channel (`app/runtime/agent.rs` wires it up) rather than
//! being returned synchronously.

mod exec;
mod output;
mod registry;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use serde_json::Value;

use crate::agent::contract::{ToolCallId, ToolCallResult};
use crate::agent::frame::AgentFrame;

/// A bash call's outcome, delivered from the background thread that ran it
/// back to the UI thread. `app/runtime/agent.rs` bridges this over a
/// `crossbeam_channel` with `floem::ext_event::create_signal_from_channel` —
/// the same cross-thread-to-UI seam it already uses for provider events
/// (`agent::contract::SessionHandle::events`) — and folds it into the
/// session's `LiveState`/`Frames` from a `create_effect`.
#[derive(Clone, Debug)]
pub(crate) struct BashCompletion {
    pub(crate) result: ToolCallResult,
}

/// Kicks off a bash call on a dedicated background thread and returns
/// immediately; the UI thread must not block waiting on this. `cwd` is the
/// session's shared, tracked bash working directory (`ToolSessionState::
/// bash_cwd_handle`) — read at the start of the call and updated in place
/// if the command `cd`s, so the next call in this session picks it up.
/// `result_tx` delivers the finished `BashCompletion` back to the UI
/// thread; see the module doc for the full round trip.
pub(crate) fn spawn(
    call_id: ToolCallId,
    input: Value,
    cwd: Arc<Mutex<PathBuf>>,
    result_tx: Sender<BashCompletion>,
) {
    std::thread::spawn(move || {
        let output = exec::run(&call_id, &input, &cwd);
        let _ = result_tx.send(BashCompletion {
            result: ToolCallResult { call_id, output },
        });
    });
}

/// Kills the running child for `call_id`, if this session has one in
/// flight, and removes it from the registry. A no-op if `call_id` isn't a
/// currently-running bash call — safe to call unconditionally for every
/// provider-originated `ToolCallFinished` (see `agent::tools::processing`),
/// since a cancelled turn's synthetic `ToolCallFinished` is exactly the
/// signal that a still-running bash child needs to be killed.
pub(crate) fn kill_if_running(call_id: &ToolCallId) {
    registry::kill(call_id);
}

/// Whether a finished bash call's result should still be folded into the
/// session's frame — `false` if `call_id` already has a `ToolCallFinished`
/// there. A cancellation racing this completion (see `kill_if_running` and
/// `agent::tools::processing`) can beat it to the frame, in which case the
/// late, genuine result is accepted and discarded — the same idempotence
/// pattern `agent::tools::approval`'s `ApprovalOutcome::AlreadyResolved`
/// uses for a duplicate approve/deny. Called from
/// `app/runtime/agent.rs::fold_bash_completion`, on the UI thread, right
/// before folding.
pub(crate) fn should_fold_completion(frame: &AgentFrame, call_id: &ToolCallId) -> bool {
    !frame.has_tool_call_finished(call_id)
}

#[cfg(test)]
mod tests;
