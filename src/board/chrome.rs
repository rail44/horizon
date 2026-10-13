//! Every fixed label the two board views draw, and the few functions that
//! format a count or an age into one. Task titles, post bodies and status
//! values are data and never pass through here.

// ---------------------------------------------------------------------------
// Thread header actions
// ---------------------------------------------------------------------------

pub(crate) const REPLY: &str = "Reply";
pub(crate) const CLOSE: &str = "Close";
pub(crate) const REOPEN: &str = "Reopen";
pub(crate) const SESSION: &str = "Session";
/// What a closed task's status line reads, alone or after its status.
pub(crate) const CLOSED: &str = "closed";

// ---------------------------------------------------------------------------
// Chips, post links, fields
// ---------------------------------------------------------------------------

pub(crate) const UNREAD: &str = "Unread";
pub(crate) const COLLAPSE: &str = "Collapse";
pub(crate) const CHANGE_STATUS: &str = "Change status";
pub(crate) const WRITE_REPLY: &str = "Write a reply";
pub(crate) const ADD_TASK: &str = "Add a task";
/// Follows the `Enter` key chip under the composer.
pub(crate) const TO_SEND: &str = "to send";

// ---------------------------------------------------------------------------
// Authors
// ---------------------------------------------------------------------------

pub(crate) const OWNER: &str = "Owner";
pub(crate) const SYSTEM: &str = "System";
pub(crate) const AGENT: &str = "Agent";

// ---------------------------------------------------------------------------
// A bound session's activity
// ---------------------------------------------------------------------------

pub(crate) const LOADING: &str = "Loading";
pub(crate) const UNREACHABLE: &str = "Unreachable";
pub(crate) const STARTING: &str = "Starting";
pub(crate) const RUNNING: &str = "Running";
pub(crate) const TOOL_RUNNING: &str = "Running a tool";
pub(crate) const WAITING_FOR_INPUT: &str = "Waiting for input";
pub(crate) const WAITING_FOR_APPROVAL: &str = "Waiting for approval";
pub(crate) const CANCELLED: &str = "Cancelled";
pub(crate) const COMPLETED: &str = "Completed";
pub(crate) const FAILED: &str = "Failed";
pub(crate) const PAUSED: &str = "Paused";
pub(crate) const TERMINATED: &str = "Terminated";

// ---------------------------------------------------------------------------
// Notices
// ---------------------------------------------------------------------------

pub(crate) const READ_ONLY_REFUSAL: &str = "This board is read-only; the write was not recorded.";
pub(crate) const TASK_NOT_FOUND: &str = "Task not found";
/// What the guest build paints in place of attaching a task's session.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub(crate) const SESSION_IS_THE_SHELLS: &str = "Opening a session is the shell's job.";

// ---------------------------------------------------------------------------
// Formatted labels
// ---------------------------------------------------------------------------

/// What the list reports when a task is opened, in a build with no shell
/// above it to open anything. The guest build paints it; the native one
/// emits the request as an event instead.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub(crate) fn open_notice(title: &str) -> String {
    format!("Open thread: {title}")
}

/// The list view's one header line: how much there is, and how much of it
/// is unread.
pub(crate) fn list_header_text(tasks: usize, unread: usize) -> String {
    let tasks = count(tasks, "task", "tasks");
    if unread == 0 {
        tasks
    } else {
        format!("{tasks} · {}", unread_count(unread))
    }
}

/// The unread chip on a task, a folded subtree, or the finished band.
pub(crate) fn unread_count(unread: usize) -> String {
    format!("{unread} unread")
}

/// When a task's thread last moved, in the thread's header band.
pub(crate) fn updated(age: &str) -> String {
    format!("Updated {age}")
}

/// The link under a folded post.
pub(crate) fn more_lines(hidden: usize) -> String {
    count(hidden, "more line", "more lines")
}

/// The finished band's header.
pub(crate) fn done_band(finished: usize) -> String {
    format!("Done ({finished})")
}

/// A row's second line: when the thread last moved, and how long it is.
pub(crate) fn row_meta(age: Option<&str>, posts: usize) -> String {
    let posts = count(posts, "post", "posts");
    match age {
        Some(age) => format!("{age} · {posts}"),
        None => posts,
    }
}

/// `at` as an age. Both arguments are unix milliseconds.
pub(crate) fn relative_time(at_ms: u64, now_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(at_ms) / 1_000;
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        86_400..=604_799 => format!("{}d ago", seconds / 86_400),
        _ => format!("{}w ago", seconds / 604_800),
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::{done_band, list_header_text, more_lines, open_notice, relative_time, row_meta};

    #[test]
    fn the_list_header_names_the_unread_only_when_there_is_some() {
        assert_eq!(list_header_text(25, 3), "25 tasks · 3 unread");
        assert_eq!(list_header_text(1, 0), "1 task");
        assert_eq!(list_header_text(0, 0), "0 tasks");
    }

    #[test]
    fn ages_read_in_the_largest_unit_that_fits() {
        let now = 10_000_000_000u64;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now - 120_000, now), "2m ago");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3h ago");
        assert_eq!(relative_time(now - 2 * 86_400_000, now), "2d ago");
        assert_eq!(relative_time(now - 21 * 86_400_000, now), "3w ago");
        // A clock behind the message reads as now rather than underflowing.
        assert_eq!(relative_time(now + 1_000, now), "just now");
    }

    #[test]
    fn counts_agree_with_their_number() {
        assert_eq!(row_meta(Some("3h ago"), 4), "3h ago · 4 posts");
        assert_eq!(row_meta(None, 1), "1 post");
        assert_eq!(row_meta(None, 0), "0 posts");
        assert_eq!(more_lines(1), "1 more line");
        assert_eq!(more_lines(12), "12 more lines");
        assert_eq!(done_band(14), "Done (14)");
    }

    #[test]
    fn opening_a_task_is_reported_with_its_title() {
        assert_eq!(
            open_notice("貼り付け時に末尾の改行が落ちる"),
            "Open thread: 貼り付け時に末尾の改行が落ちる"
        );
    }

    /// No label or format string above the tests holds a CJK character, so
    /// a translated label cannot slip back into the chrome.
    #[test]
    fn the_chrome_holds_no_cjk_text() {
        let source = include_str!("chrome.rs");
        let table = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module before its tests");
        let cjk: Vec<char> = table
            .chars()
            .filter(|ch| {
                matches!(
                    *ch as u32,
                    0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF
                )
            })
            .collect();
        assert!(cjk.is_empty(), "CJK text in the board chrome: {cjk:?}");
    }
}
