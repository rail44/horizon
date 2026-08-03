//! Push delivery of finished `task` children: the notification text a
//! requester's next provider round is given, and the wake channel that
//! starts a turn when there is no round to ride on.
//!
//! **Wire shape (`docs/agent-async-task-design.md` decision 2).** The
//! notification reaches the provider as a **user-role message carrying
//! plain text** and nothing else -- never a synthetic tool call or tool
//! result. That is the one shape every production chat template can render:
//! MiniMax-M3's, the strictest observed, iterates a tool call's `arguments`
//! as a mapping with no string branch at all (it is what killed session
//! `12fd8d14`, see `providers::rig::completion::
//! replay_safe_tool_arguments`), so anything tool-call-shaped is a
//! standing risk that plain user text simply does not carry. The
//! *persisted* shape is distinct even though the provider-facing one is
//! not: `contract::MessageRole::TaskNotification` keeps the event log from
//! recording a system notification as words the human typed.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::contract::SessionId;

use super::children::Completion;

/// How much of a child's report is inlined into the notification before it
/// is cut and the requester is pointed at `task_output` for the rest.
///
/// Chosen to be large enough that most delegated reports arrive whole (the
/// measured reports in the 2026-07-27 campaign ran ~27k characters at the
/// blocking-delegation extreme, but those were one-per-session monoliths;
/// the whole point of async delegation is smaller, several-in-flight
/// questions) and small enough that three coalesced completions cannot
/// dominate a turn's input on their own.
pub(crate) const INLINE_REPORT_CAP_CHARS: usize = 4_000;

/// Builds the single notification block for everything that finished since
/// the last drain. Multiple completions coalesce here rather than at the
/// delivery site: one block per provider round is what decision 2 asks
/// for, and it keeps the requester from paying a preamble per child.
pub(super) fn notification_text(completions: &[Completion]) -> String {
    let mut text = String::new();
    text.push_str(if completions.len() == 1 {
        "A background task you launched has finished. This is a system notification, not a \
         message from the user."
    } else {
        "Background tasks you launched have finished. This is a system notification, not a \
         message from the user."
    });
    for (index, completion) in completions.iter().enumerate() {
        text.push_str("\n\n");
        text.push_str(&entry(index + 1, completion));
    }
    text
}

fn entry(position: usize, completion: &Completion) -> String {
    let session_id = completion.session_id.as_uuid().to_string();
    let output = &completion.output;
    let failed = output.get("is_error").and_then(serde_json::Value::as_bool) == Some(true);
    let capped = output.get("capped").and_then(serde_json::Value::as_bool) == Some(true);
    let message = output
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let report = output
        .get("report")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    // A child that died or was capped mid-thought can leave a "partial
    // report" with no body -- the observed extreme being one whose entire
    // text was `</mm:think>`. Shipping that as content reads like an answer,
    // so the notification says what happened instead and points at the one
    // move that can still work (`super::report_body_is_empty`).
    let usable = !super::report_body_is_empty(report);
    let status = if usable {
        match (failed, capped) {
            (true, _) => format!("finished with an error ({message}); partial report follows"),
            (false, true) => {
                "completed, but hit its turn limit — the report is partial".to_string()
            }
            (false, false) => "completed".to_string(),
        }
    } else {
        let cause = match (failed, capped) {
            (true, _) if !message.is_empty() => format!("failed: {message}"),
            (true, _) => "failed".to_string(),
            (false, true) => "hit its turn limit".to_string(),
            (false, false) => "ended its turn with nothing to say".to_string(),
        };
        format!(
            "{cause} — it produced no usable report. If you still need this, relaunch a narrower \
             task rather than re-reading this one."
        )
    };
    let mut entry = format!(
        "[{position}] task \"{description}\" (session_id: {session_id}) — {status}",
        description = completion.description,
    );
    if usable {
        let (head, truncated) = truncate_report(report);
        entry.push('\n');
        entry.push_str(&head);
        if truncated {
            entry.push_str(&format!(
                "\n… [report truncated at {INLINE_REPORT_CAP_CHARS} characters; call task_output \
                 with session_id \"{session_id}\" for the full report]"
            ));
        }
    }
    entry
}

/// Cuts `report` to [`INLINE_REPORT_CAP_CHARS`] *characters* (never bytes --
/// a report can hold any UTF-8), reporting whether anything was dropped.
fn truncate_report(report: &str) -> (String, bool) {
    crate::transcript::truncate_chars(report, INLINE_REPORT_CAP_CHARS)
}

/// The wake half of the subscription seam: a session loop registers here at
/// startup and is signalled whenever one of its children finishes, so a
/// completion landing after the requester's turn already ended can start a
/// new one (`docs/agent-async-task-design.md` decision 2, "the completion
/// starts a new turn automatically"). Unbounded and lossless-by-count is
/// not required -- the queue in `super::children` is the real carrier, and
/// a signal only ever means "look at it".
fn wakers() -> &'static Mutex<HashMap<SessionId, UnboundedSender<()>>> {
    static WAKERS: OnceLock<Mutex<HashMap<SessionId, UnboundedSender<()>>>> = OnceLock::new();
    WAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Installs `session_id`'s wake channel and hands back the receiving half.
/// Called once by a session loop as it starts (`providers::rig::session::
/// run_session_loop`); a second call replaces the first, so a respawned
/// loop for the same id never leaves a dead sender behind.
pub(crate) fn register_wake(session_id: SessionId) -> UnboundedReceiver<()> {
    let (tx, rx) = unbounded_channel();
    wakers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id, tx);
    rx
}

pub(crate) fn unregister_wake(session_id: SessionId) {
    wakers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&session_id);
}

/// Signals `session_id`'s loop that it has something queued. A no-op for a
/// session with no registered loop (a non-rig provider, or one that already
/// exited) -- the completion stays queued either way.
pub(super) fn wake(session_id: SessionId) {
    let wakers = wakers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(tx) = wakers.get(&session_id) {
        let _ = tx.send(());
    }
}
