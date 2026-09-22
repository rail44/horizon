//! The prototype's pure half: the steering order, unread counts, message
//! folding, relative times, and the key map.
//!
//! Nothing here touches GPUI, so the decisions the view is built on are
//! unit-testable on their own.

use std::collections::HashMap;

use horizon_board::Item;

use crate::board_pane::activity::BoardSessionActivity;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Everything the prototype can be asked to do. Keys, buttons, and row
/// clicks all resolve to one of these, so a chord is attached to behaviour
/// in exactly one place ([`command_for_key`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    SelectNext,
    SelectPrevious,
    SelectTask(u64),
    FocusComposer,
    FocusList,
    ToggleFinished,
    /// Expands every folded message in the open thread, or folds them all
    /// back once none is folded.
    ToggleLongMessages,
    ToggleMessage(String),
    ToggleBody,
    SetStatus,
    ToggleClosed,
    OpenTaskSession,
    PostMessage,
}

/// The key map. `key` is a GPUI keystroke key name; a chord carrying any
/// modifier other than shift never reaches this.
pub(crate) fn command_for_key(key: &str) -> Option<Command> {
    Some(match key {
        "j" | "down" => Command::SelectNext,
        "k" | "up" => Command::SelectPrevious,
        "enter" => Command::FocusComposer,
        "escape" => Command::FocusList,
        "o" => Command::ToggleFinished,
        "e" => Command::ToggleLongMessages,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// The steering order
// ---------------------------------------------------------------------------

/// The bands the list is ordered in. Declaration order is display order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Group {
    /// Messages nobody has read yet — the reason to open the board.
    Unread,
    /// A bound session that is running or waiting on something.
    Active,
    Open,
    /// `done` or closed, folded behind one row.
    Finished,
}

/// One list row: the task plus everything the row draws.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) item: Item,
    pub(crate) group: Group,
    pub(crate) unread: usize,
    pub(crate) activity: Option<BoardSessionActivity>,
}

/// How many of `item`'s messages come after the reader's recorded position.
/// A task with no position recorded has read nothing.
pub(crate) fn unread_count(item: &Item, positions: &HashMap<u64, String>) -> usize {
    let read = positions
        .get(&item.id)
        .and_then(|id| item.comments.iter().position(|comment| &comment.id == id))
        .map(|index| index + 1)
        .unwrap_or(0);
    item.comments.len().saturating_sub(read)
}

/// Finished work: explicitly closed, or carrying the status the board uses
/// for a finished task.
pub(crate) fn is_finished(item: &Item) -> bool {
    item.is_closed || item.status.eq_ignore_ascii_case("done")
}

/// A session that still has somewhere to get to.
pub(crate) fn is_active(activity: Option<BoardSessionActivity>) -> bool {
    matches!(
        activity,
        Some(
            BoardSessionActivity::Starting
                | BoardSessionActivity::Running
                | BoardSessionActivity::ToolRunning
                | BoardSessionActivity::WaitingForApproval
                | BoardSessionActivity::WaitingForInput
        )
    )
}

/// Unread wins over finished: an agent that reports and closes its task
/// would otherwise post into a collapsed group.
fn group_of(item: &Item, unread: usize, activity: Option<BoardSessionActivity>) -> Group {
    if unread > 0 {
        Group::Unread
    } else if is_finished(item) {
        Group::Finished
    } else if is_active(activity) {
        Group::Active
    } else {
        Group::Open
    }
}

/// The list, ordered for steering: unread first, then live sessions, then
/// the rest by rank, with finished work last.
pub(crate) fn rows(
    items: &[Item],
    positions: &HashMap<u64, String>,
    states: &HashMap<horizon_workspace::SessionId, BoardSessionActivity>,
) -> Vec<Row> {
    let mut rows: Vec<Row> = items
        .iter()
        .map(|item| {
            let unread = unread_count(item, positions);
            let activity = crate::board_pane::activity::task_session_state(item, states);
            Row {
                group: group_of(item, unread, activity),
                unread,
                activity,
                item: item.clone(),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.item.rank.cmp(&b.item.rank))
            .then_with(|| a.item.id.cmp(&b.item.id))
    });
    rows
}

/// Indices into `rows` that are on screen: the finished band only when it
/// is expanded.
pub(crate) fn visible_rows(rows: &[Row], finished_expanded: bool) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| finished_expanded || row.group != Group::Finished)
        .map(|(index, _)| index)
        .collect()
}

/// How many rows the finished band holds, and how many unread messages are
/// hidden with them.
pub(crate) fn finished_summary(rows: &[Row]) -> (usize, usize) {
    rows.iter()
        .filter(|row| row.group == Group::Finished)
        .fold((0, 0), |(count, unread), row| {
            (count + 1, unread + row.unread)
        })
}

/// The task a selection move lands on. `selected` is the currently selected
/// task id; `None` selects the first row.
pub(crate) fn step_selection(visible: &[u64], selected: Option<u64>, forward: bool) -> Option<u64> {
    if visible.is_empty() {
        return None;
    }
    let current = selected.and_then(|id| visible.iter().position(|row| *row == id));
    let next = match (current, forward) {
        (None, _) => 0,
        (Some(index), true) => (index + 1).min(visible.len() - 1),
        (Some(index), false) => index.saturating_sub(1),
    };
    visible.get(next).copied()
}

// ---------------------------------------------------------------------------
// Message folding
// ---------------------------------------------------------------------------

/// A message longer than this many lines is folded to its first lines.
pub(crate) const FOLD_LINES: usize = 12;

/// A message longer than this many characters is folded even when it has
/// few line breaks — one 5,000-character paragraph is as long to read as
/// fifty short lines.
pub(crate) const FOLD_CHARS: usize = 700;

/// The head of a folded message, and how much of it stays hidden.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fold {
    pub(crate) head: String,
    pub(crate) hidden_lines: usize,
}

/// The fold for `text`, or `None` when it is short enough to show whole.
pub(crate) fn fold(text: &str) -> Option<Fold> {
    let total_lines = text.lines().count();
    let mut head = String::new();
    let mut kept = 0usize;
    let mut cut_mid_line = false;
    for line in text.lines() {
        if kept == FOLD_LINES || head.chars().count() >= FOLD_CHARS {
            break;
        }
        if kept > 0 {
            head.push('\n');
        }
        let budget = FOLD_CHARS.saturating_sub(head.chars().count());
        if line.chars().count() > budget {
            head.extend(line.chars().take(budget));
            head.push('…');
            cut_mid_line = true;
            kept += 1;
            break;
        }
        head.push_str(line);
        kept += 1;
    }
    if kept == total_lines && !cut_mid_line {
        return None;
    }
    let hidden = total_lines
        .saturating_sub(kept)
        .max(usize::from(cut_mid_line));
    Some(Fold {
        head,
        hidden_lines: hidden,
    })
}

// ---------------------------------------------------------------------------
// Presentation helpers
// ---------------------------------------------------------------------------

/// Who wrote a message, as far as the thread's typography cares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Voice {
    Owner,
    Agent,
    System,
}

pub(crate) fn voice(author: &str) -> Voice {
    match author {
        "owner" => Voice::Owner,
        "system" => Voice::System,
        _ => Voice::Agent,
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

/// Wall-clock now in unix milliseconds, `0` when the clock is unreadable.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// Escapes plain text for verbatim rendering through `TextView::markdown`:
/// backslash-escaping every ASCII punctuation character keeps a reply's
/// `*` or `1.` from turning into a GFM construct. CommonMark resolves each
/// escape back to the literal character, so the painted text is unchanged.
pub(crate) fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_punctuation() {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// The status line a row and the thread header show.
pub(crate) fn status_text(item: &Item) -> String {
    match (item.is_closed, item.status.trim().is_empty()) {
        (true, true) => "closed".to_string(),
        (true, false) => format!("{} · closed", item.status.trim()),
        (false, true) => String::new(),
        (false, false) => item.status.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        command_for_key, finished_summary, fold, is_active, is_finished, relative_time, rows,
        status_text, step_selection, unread_count, visible_rows, Command, Group, Voice, FOLD_CHARS,
        FOLD_LINES,
    };
    use crate::board_pane::activity::BoardSessionActivity;
    use horizon_board::{Comment, Item};
    use horizon_workspace::SessionId;
    use std::collections::HashMap;

    fn message(id: &str) -> Comment {
        Comment {
            id: id.into(),
            author: "agent".into(),
            text: "…".into(),
            at: None,
            source: None,
        }
    }

    fn task(id: u64, rank: &str) -> Item {
        Item {
            id,
            rank: rank.into(),
            title: format!("task {id}"),
            ..Item::default()
        }
    }

    #[test]
    fn keys_map_to_one_command_each() {
        assert_eq!(command_for_key("j"), Some(Command::SelectNext));
        assert_eq!(command_for_key("down"), Some(Command::SelectNext));
        assert_eq!(command_for_key("k"), Some(Command::SelectPrevious));
        assert_eq!(command_for_key("up"), Some(Command::SelectPrevious));
        assert_eq!(command_for_key("enter"), Some(Command::FocusComposer));
        assert_eq!(command_for_key("escape"), Some(Command::FocusList));
        assert_eq!(command_for_key("o"), Some(Command::ToggleFinished));
        assert_eq!(command_for_key("e"), Some(Command::ToggleLongMessages));
        assert_eq!(command_for_key("x"), None);
    }

    #[test]
    fn unread_counts_the_messages_after_the_recorded_position() {
        let mut item = task(1, "a");
        item.comments = vec![message("m1"), message("m2"), message("m3")];
        let mut positions = HashMap::new();
        assert_eq!(unread_count(&item, &positions), 3);
        positions.insert(1, "m1".to_string());
        assert_eq!(unread_count(&item, &positions), 2);
        positions.insert(1, "m3".to_string());
        assert_eq!(unread_count(&item, &positions), 0);
        // A position naming a message the task no longer has reads as
        // nothing read, not as everything read.
        positions.insert(1, "gone".to_string());
        assert_eq!(unread_count(&item, &positions), 3);
    }

    #[test]
    fn done_without_closing_still_counts_as_finished() {
        let mut item = task(1, "a");
        assert!(!is_finished(&item));
        item.status = "done".into();
        assert!(is_finished(&item));
        item.status = "進行中".into();
        item.is_closed = true;
        assert!(is_finished(&item));
    }

    #[test]
    fn only_running_and_waiting_sessions_are_active() {
        assert!(is_active(Some(BoardSessionActivity::Running)));
        assert!(is_active(Some(BoardSessionActivity::ToolRunning)));
        assert!(is_active(Some(BoardSessionActivity::WaitingForApproval)));
        assert!(is_active(Some(BoardSessionActivity::WaitingForInput)));
        assert!(!is_active(Some(BoardSessionActivity::Completed)));
        assert!(!is_active(Some(BoardSessionActivity::Terminated)));
        assert!(!is_active(None));
    }

    #[test]
    fn unread_leads_then_live_sessions_then_rank_with_finished_last() {
        let session = SessionId::new();
        let mut unread_done = task(1, "z");
        unread_done.status = "done".into();
        unread_done.comments = vec![message("m1")];
        let mut running = task(2, "y");
        running.session_id = Some(session.as_uuid().to_string());
        let plain_late = task(3, "c");
        let plain_early = task(4, "b");
        let mut finished = task(5, "a");
        finished.is_closed = true;

        let states = HashMap::from([(session, BoardSessionActivity::ToolRunning)]);
        let ordered = rows(
            &[
                unread_done.clone(),
                running,
                plain_late,
                plain_early,
                finished,
            ],
            &HashMap::new(),
            &states,
        );
        assert_eq!(
            ordered.iter().map(|row| row.item.id).collect::<Vec<_>>(),
            vec![1, 2, 4, 3, 5]
        );
        assert_eq!(ordered[0].group, Group::Unread);
        assert_eq!(ordered[1].group, Group::Active);
        assert_eq!(ordered[4].group, Group::Finished);
        // A finished task nobody has read still leads the list.
        assert_eq!(ordered[0].unread, 1);

        let visible = visible_rows(&ordered, false);
        assert_eq!(visible, vec![0, 1, 2, 3]);
        assert_eq!(visible_rows(&ordered, true).len(), 5);
        assert_eq!(finished_summary(&ordered), (1, 0));
    }

    #[test]
    fn selection_steps_within_the_visible_rows_and_stops_at_the_ends() {
        let visible = vec![7u64, 8, 9];
        assert_eq!(step_selection(&visible, None, true), Some(7));
        assert_eq!(step_selection(&visible, Some(7), true), Some(8));
        assert_eq!(step_selection(&visible, Some(9), true), Some(9));
        assert_eq!(step_selection(&visible, Some(9), false), Some(8));
        assert_eq!(step_selection(&visible, Some(7), false), Some(7));
        // A selection that scrolled out of the visible set restarts.
        assert_eq!(step_selection(&visible, Some(42), true), Some(7));
        assert_eq!(step_selection(&[], Some(7), true), None);
    }

    #[test]
    fn short_messages_are_not_folded_and_long_ones_keep_their_first_lines() {
        assert_eq!(fold("one line"), None);
        let short = (0..FOLD_LINES).map(|i| i.to_string()).collect::<Vec<_>>();
        assert_eq!(fold(&short.join("\n")), None);

        let long = (0..FOLD_LINES + 5)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let folded = fold(&long).expect("a 17-line message folds");
        assert_eq!(folded.hidden_lines, 5);
        assert_eq!(folded.head.lines().count(), FOLD_LINES);
        assert!(folded.head.starts_with("line 0"));
        assert!(!folded.head.contains("line 12"));
    }

    #[test]
    fn one_very_long_paragraph_folds_on_characters() {
        let paragraph = "あ".repeat(FOLD_CHARS * 3);
        let folded = fold(&paragraph).expect("a long paragraph folds");
        assert_eq!(
            folded.head.chars().count(),
            FOLD_CHARS + 1,
            "head plus the ellipsis"
        );
        assert_eq!(folded.hidden_lines, 1);
    }

    #[test]
    fn authorship_splits_into_three_voices() {
        assert_eq!(super::voice("owner"), Voice::Owner);
        assert_eq!(super::voice("system"), Voice::System);
        assert_eq!(super::voice("agent"), Voice::Agent);
        assert_eq!(super::voice("reviewer"), Voice::Agent);
    }

    #[test]
    fn ages_read_in_the_largest_unit_that_fits() {
        let now = 10_000_000_000u64;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now - 120_000, now), "2m ago");
        assert_eq!(relative_time(now - 7_200_000, now), "2h ago");
        assert_eq!(relative_time(now - 3 * 86_400_000, now), "3d ago");
        assert_eq!(relative_time(now - 21 * 86_400_000, now), "3w ago");
        // A clock behind the message reads as now rather than underflowing.
        assert_eq!(relative_time(now + 1_000, now), "just now");
    }

    #[test]
    fn escaping_keeps_a_plain_reply_plain() {
        assert_eq!(
            super::escape_markdown("# not a heading"),
            "\\# not a heading"
        );
        assert_eq!(
            super::escape_markdown("続行してください"),
            "続行してください"
        );
    }

    #[test]
    fn status_text_joins_closure_to_the_project_status() {
        let mut item = task(1, "a");
        assert_eq!(status_text(&item), "");
        item.status = "review".into();
        assert_eq!(status_text(&item), "review");
        item.is_closed = true;
        assert_eq!(status_text(&item), "review · closed");
        item.status = String::new();
        assert_eq!(status_text(&item), "closed");
    }
}
