//! Every fixed label the pane draws. The conversation's own text is data and
//! is never looked up here.

use super::display::Task;

/// The running session's state chip.
pub(crate) const RUNNING: &str = "Running";
/// The state chip while an approval is pending.
pub(crate) const WAITING_FOR_APPROVAL: &str = "Waiting for approval";
pub(crate) const STOPPED: &str = "Stopped";
pub(crate) const FAILED: &str = "Failed";
pub(crate) const STOP: &str = "Stop";
pub(crate) const APPROVE: &str = "Approve";
pub(crate) const DENY: &str = "Deny";
/// The chip on the pending approval's row.
pub(crate) const APPROVAL_NEEDED: &str = "Approval needed";
pub(crate) const THINKING: &str = "Thinking";
pub(crate) const PLACEHOLDER: &str = "Write a message";
pub(crate) const PLACEHOLDER_ENDED: &str = "This session has ended";
pub(crate) const PLACEHOLDER_UNREACHABLE: &str = "Runtime unreachable";
/// What follows the `Enter` key chip in the composer.
pub(crate) const SEND_HINT: &str = "to send";
pub(crate) const SESSION_ENDED: &str = "Session ended";
pub(crate) const RUNTIME_UNREACHABLE: &str = "Runtime unreachable";

/// A receipt line's count of failed calls.
pub(crate) fn failed_calls(count: usize) -> String {
    format!("{count} failed")
}

/// A running receipt's line, naming the call in flight: `Running cargo
/// test`, `Reading src/a.rs`.
pub(crate) fn in_flight(verb: &str, target: &str) -> String {
    let doing = match verb {
        "Read" => "Reading",
        "Edit" => "Editing",
        "Write" => "Writing",
        "Run" => "Running",
        "Search" => "Searching",
        "Call" => "Calling",
        _ => return format!("{RUNNING} {verb} {target}"),
    };
    format!("{doing} {target}")
}

/// The line under an output whose tail was cut.
pub(crate) fn more_lines(count: usize) -> String {
    format!("… {count} more lines")
}

/// One background task's line.
pub(crate) fn task_line(task: &Task) -> String {
    format!(
        "Background · {} · {} · {}",
        task.description, task.elapsed, task.activity
    )
}

/// Every label the pane can draw, the counted ones at `count` and the task
/// line over `task`.
#[cfg(test)]
pub(crate) fn all(count: usize, task: &Task) -> Vec<String> {
    let mut all: Vec<String> = [
        RUNNING,
        WAITING_FOR_APPROVAL,
        STOPPED,
        FAILED,
        STOP,
        APPROVE,
        DENY,
        APPROVAL_NEEDED,
        THINKING,
        PLACEHOLDER,
        PLACEHOLDER_ENDED,
        PLACEHOLDER_UNREACHABLE,
        SEND_HINT,
        SESSION_ENDED,
        RUNTIME_UNREACHABLE,
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    all.extend([failed_calls(count), more_lines(count), task_line(task)]);
    all
}
