//! The two board views' pure half: the tree order the list draws, unread
//! counts, post folding, the post cursor, where a dragged row lands, and
//! the two key maps.
//!
//! Nothing here builds an element, so the decisions both views are built on
//! are the same in either shell.

use std::collections::{HashMap, HashSet};

use horizon_board::{tree_order, Item, Position};

use crate::chrome;
pub use crate::measure::{display_width, rendered_lines};

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Everything the thread view can be asked to do. Keys, buttons, and clicks
/// all resolve to one of these, so a chord is attached to behaviour in
/// exactly one place ([`command_for_key`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadCommand {
    NextPost,
    PreviousPost,
    /// Folds the post under the cursor away, or opens it back up.
    ToggleCurrentPost,
    /// The same for a named post, for the fold affordances inside it.
    TogglePost(String),
    /// Moves the cursor onto a post that was clicked.
    SelectPost(usize),
    FocusComposer,
    LeaveComposer,
    ToggleClosed,
    /// Writes the status the header's input holds.
    SaveStatus,
    OpenTaskSession,
    PostMessage,
}

/// Everything the list view can be asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListCommand {
    SelectNext,
    SelectPrevious,
    SelectTask(u64),
    /// Shows the selected parent's children.
    Expand,
    /// Hides them again.
    Collapse,
    /// The same for a named task, for the disclosure affordance on a row.
    ToggleExpansion(u64),
    ToggleFinished,
    /// Opens the selected task's thread. A pane of its own holds that
    /// thread, so the list reports the request rather than rendering it.
    OpenThread,
    /// Moves the selected task among its siblings.
    MoveUp,
    MoveDown,
    /// Adds a top-level task from the pinned input.
    AddTask,
    /// Applies the move the drop indicator is showing.
    Reorder,
}

/// The thread view's key map. `key` is a GPUI keystroke key name; a chord
/// carrying any modifier other than shift never reaches this.
pub fn command_for_key(key: &str) -> Option<ThreadCommand> {
    Some(match key {
        "j" | "down" => ThreadCommand::NextPost,
        "k" | "up" => ThreadCommand::PreviousPost,
        "e" => ThreadCommand::ToggleCurrentPost,
        "enter" => ThreadCommand::FocusComposer,
        "escape" => ThreadCommand::LeaveComposer,
        _ => return None,
    })
}

/// The list view's key map.
pub fn list_command_for_key(key: &str) -> Option<ListCommand> {
    Some(match key {
        "j" | "down" => ListCommand::SelectNext,
        "k" | "up" => ListCommand::SelectPrevious,
        "l" | "right" => ListCommand::Expand,
        "h" | "left" => ListCommand::Collapse,
        "o" => ListCommand::ToggleFinished,
        "enter" => ListCommand::OpenThread,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// The tree order
// ---------------------------------------------------------------------------

/// One list row: the task plus everything the row draws.
#[derive(Clone, Debug)]
pub struct Row {
    pub item: Item,
    /// How deep under a top-level task the row sits.
    pub depth: usize,
    /// Messages in this task nobody has read.
    pub unread: usize,
    /// The same, counted over this task and everything under it.
    pub subtree_unread: usize,
    /// Whether the task has children to disclose.
    pub has_children: bool,
    /// Whether those children are hidden right now.
    pub collapsed: bool,
    /// Whether a collapsed task above it hides this row.
    pub hidden: bool,
    /// Closed, or carrying the status the board uses for finished work.
    pub finished: bool,
    /// Whether the row belongs to the band at the bottom: a finished
    /// top-level task, and everything under it.
    pub in_finished_band: bool,
}

/// How many of `item`'s messages the reader has already seen: everything up
/// to and including the recorded position. A task with no position
/// recorded, or one whose position names a message the task no longer has,
/// has read nothing.
pub fn read_through(item: &Item, positions: &HashMap<u64, String>) -> usize {
    positions
        .get(&item.id)
        .and_then(|id| item.comments.iter().position(|comment| &comment.id == id))
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// How many of `item`'s messages come after the reader's recorded position.
pub fn unread_count(item: &Item, positions: &HashMap<u64, String>) -> usize {
    item.comments
        .len()
        .saturating_sub(read_through(item, positions))
}

/// Finished work: explicitly closed, or carrying the status the board uses
/// for a finished task.
pub fn is_finished(item: &Item) -> bool {
    item.is_closed || item.status.eq_ignore_ascii_case("done")
}

/// The unread messages of `id` and of every task under it. A parent shows
/// this rather than its own count, so unread work never hides inside a
/// collapsed subtree.
fn subtree_unread(
    id: u64,
    children: &HashMap<u64, Vec<u64>>,
    own: &HashMap<u64, usize>,
    seen: &mut HashSet<u64>,
) -> usize {
    if !seen.insert(id) {
        return 0;
    }
    let mut total = own.get(&id).copied().unwrap_or(0);
    for child in children.get(&id).into_iter().flatten() {
        total += subtree_unread(*child, children, own, seen);
    }
    total
}

/// Every row the list can show, in the owner's own order: top-level tasks
/// by rank with their children indented under them, and the finished
/// top-level subtrees moved to the end as the band's contents. A row under
/// a collapsed task is still in the list, marked `hidden`; the finished
/// band's rows are marked too, so one pass builds both foldings.
pub fn tree_rows(
    items: &[Item],
    positions: &HashMap<u64, String>,
    collapsed: &HashSet<u64>,
) -> Vec<Row> {
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut own: HashMap<u64, usize> = HashMap::new();
    for item in items {
        own.insert(item.id, unread_count(item, positions));
        if let Some(parent) = item.parent {
            children.entry(parent).or_default().push(item.id);
        }
    }

    // `tree_order` emits each top-level task followed by its own subtree, so
    // a depth-0 entry is where one subtree ends and the next begins.
    let ordered = tree_order(items, false);
    let mut groups: Vec<(bool, Vec<(&Item, usize)>)> = Vec::new();
    for (item, depth) in ordered {
        if depth == 0 || groups.is_empty() {
            groups.push((is_finished(item), Vec::new()));
        }
        groups
            .last_mut()
            .expect("a group was just pushed")
            .1
            .push((item, depth));
    }

    let mut rows = Vec::new();
    for finished_band in [false, true] {
        for (band, group) in groups.iter().filter(|(band, _)| *band == finished_band) {
            let mut collapse_depth: Option<usize> = None;
            for (item, depth) in group {
                if collapse_depth.is_some_and(|open_at| *depth <= open_at) {
                    collapse_depth = None;
                }
                let hidden = collapse_depth.is_some();
                let has_children = children.contains_key(&item.id);
                let is_collapsed = has_children && collapsed.contains(&item.id);
                if !hidden && is_collapsed {
                    collapse_depth = Some(*depth);
                }
                rows.push(Row {
                    depth: *depth,
                    unread: own.get(&item.id).copied().unwrap_or(0),
                    subtree_unread: subtree_unread(item.id, &children, &own, &mut HashSet::new()),
                    has_children,
                    collapsed: is_collapsed,
                    hidden,
                    finished: is_finished(item),
                    in_finished_band: *band,
                    item: (*item).clone(),
                });
            }
        }
    }
    rows
}

/// Indices into `rows` that are on screen: nothing under a collapsed task,
/// and the finished band only when it is expanded.
pub fn visible_rows(rows: &[Row], finished_expanded: bool) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| !row.hidden && (finished_expanded || !row.in_finished_band))
        .map(|(index, _)| index)
        .collect()
}

/// How many top-level tasks the finished band holds, and how many unread
/// messages are folded away with them.
pub fn finished_summary(rows: &[Row]) -> (usize, usize) {
    rows.iter()
        .filter(|row| row.in_finished_band && row.depth == 0)
        .fold((0, 0), |(count, unread), row| {
            (count + 1, unread + row.subtree_unread)
        })
}

/// The task a selection move lands on. `selected` is the currently selected
/// task id; `None` selects the first row.
pub fn step_selection(visible: &[u64], selected: Option<u64>, forward: bool) -> Option<u64> {
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

/// The post the cursor lands on among `posts` posts. A folded post is still
/// a post: folding takes nothing out of the order, and `e` on a folded post
/// is what opens it.
pub fn step_post(posts: usize, cursor: Option<usize>, forward: bool) -> Option<usize> {
    if posts == 0 {
        return None;
    }
    let next = match (cursor, forward) {
        (None, _) => 0,
        (Some(index), true) => (index + 1).min(posts - 1),
        (Some(index), false) => index.saturating_sub(1),
    };
    Some(next.min(posts - 1))
}

// ---------------------------------------------------------------------------
// Moving a task among its siblings
// ---------------------------------------------------------------------------

/// Where moving `id` one place up (or down) among its siblings puts it, or
/// `None` when it is already at that end. Siblings are the tasks sharing its
/// parent, whatever the display currently hides.
pub fn sibling_move(items: &[Item], id: u64, up: bool) -> Option<Position> {
    let item = items.iter().find(|item| item.id == id)?;
    let mut siblings: Vec<_> = items
        .iter()
        .filter(|other| other.parent == item.parent)
        .collect();
    siblings.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.id.cmp(&b.id)));
    let index = siblings.iter().position(|item| item.id == id)?;
    if up {
        Some(Position::Before(siblings.get(index.checked_sub(1)?)?.id))
    } else {
        Some(Position::After(siblings.get(index + 1)?.id))
    }
}

/// Which half of a row the cursor is in during a drag, used to decide
/// whether the drop indicator line shows above or below the row and
/// whether the move is `Before` or `After`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DropHalf {
    Above,
    Below,
}

/// The `Position` (if any) for dropping `dragged_id` onto the given `half`
/// of `target_id`'s row, or `None` when the drop is a no-op -- it would
/// leave the item where it already is.
///
/// `Above` maps to `Before(target_id)` and `Below` to `After(target_id)`,
/// but both are suppressed when the resulting insertion point equals the
/// dragged item's current position. A drop is a no-op when the dragged
/// and target items are the same, or when dropping on the near half of an
/// adjacent row (the half that faces the dragged item). This is the
/// invariant behind the indicator: a position that shows an indicator is
/// always one where a drop will execute a move.
pub fn drop_position_from_half(
    dragged_id: u64,
    items: &[Item],
    target_id: u64,
    half: DropHalf,
) -> Option<Position> {
    let dragged = items.iter().find(|item| item.id == dragged_id)?;
    let target = items.iter().find(|item| item.id == target_id)?;
    if dragged.parent != target.parent {
        return None;
    }
    let mut siblings: Vec<_> = items
        .iter()
        .filter(|item| item.parent == dragged.parent)
        .collect();
    siblings.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.id.cmp(&b.id)));
    let di = siblings.iter().position(|i| i.id == dragged_id)?;
    let ti = siblings.iter().position(|i| i.id == target_id)?;
    match half {
        DropHalf::Above => {
            // `Before(target)`: a no-op when the dragged item is the target
            // itself, or already sits immediately before it.
            if di == ti || di + 1 == ti {
                None
            } else {
                Some(Position::Before(target_id))
            }
        }
        DropHalf::Below => {
            // `After(target)`: a no-op when the dragged item is the target
            // itself, or already sits immediately after it.
            if di == ti || di == ti + 1 {
                None
            } else {
                Some(Position::After(target_id))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The sessions a board binds
// ---------------------------------------------------------------------------

/// Every session id the board's tasks name, task bindings and reviewer
/// bindings alike, each reported once. An id that is not a uuid names no
/// session the shell can resolve, so it is dropped here.
pub fn bound_sessions(items: &[Item]) -> Vec<horizon_workspace::SessionId> {
    let mut seen = HashSet::new();
    items
        .iter()
        .flat_map(|item| {
            [
                item.session_id.as_deref(),
                item.review_session_id.as_deref(),
            ]
        })
        .flatten()
        .filter_map(|id| uuid::Uuid::parse_str(id).ok())
        .map(horizon_workspace::SessionId::from_uuid)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// The session id a task's own binding names, if it has one.
pub fn task_session_id(item: &Item) -> Option<horizon_workspace::SessionId> {
    uuid::Uuid::parse_str(item.session_id.as_deref()?)
        .ok()
        .map(horizon_workspace::SessionId::from_uuid)
}

// ---------------------------------------------------------------------------
// What the reader has actually seen
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The task header band
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Folding by rendered length
// ---------------------------------------------------------------------------

/// A post taller than this many rendered lines is folded to a preview.
pub const FOLD_RENDERED_LINES: usize = 40;

/// How many of the post's own lines the preview keeps, on top of its first
/// heading.
pub const FOLD_PREVIEW_LINES: usize = 3;

/// The head of a folded post, and how many rendered lines it hides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoldPreview {
    pub head: String,
    pub hidden_lines: usize,
}

/// The preview for `text` in a column `cells` wide, or `None` when the post
/// is short enough to show whole. The preview is the post's first heading,
/// if it has one, plus its first [`FOLD_PREVIEW_LINES`] non-empty lines.
pub fn fold_preview(text: &str, cells: usize) -> Option<FoldPreview> {
    let total = rendered_lines(text, cells);
    if total <= FOLD_RENDERED_LINES {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let heading = lines
        .iter()
        .position(|line| line.trim_start().starts_with('#'));
    let mut kept: Vec<usize> = heading.into_iter().collect();
    for (index, line) in lines.iter().enumerate() {
        if kept.len() >= FOLD_PREVIEW_LINES + usize::from(heading.is_some()) {
            break;
        }
        if line.trim().is_empty() || kept.contains(&index) {
            continue;
        }
        kept.push(index);
    }
    kept.sort_unstable();
    let head = kept
        .iter()
        .filter_map(|index| lines.get(*index).copied())
        .collect::<Vec<_>>()
        .join("\n\n");
    let hidden = total.saturating_sub(rendered_lines(&head, cells)).max(1);
    Some(FoldPreview {
        head,
        hidden_lines: hidden,
    })
}

// ---------------------------------------------------------------------------
// Presentation helpers
// ---------------------------------------------------------------------------

/// Who wrote a message, as far as the thread's typography cares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Voice {
    Owner,
    Agent,
    System,
}

pub fn voice(author: &str) -> Voice {
    match author {
        "owner" => Voice::Owner,
        "system" => Voice::System,
        _ => Voice::Agent,
    }
}

/// Wall-clock now in unix milliseconds, `0` when the clock is unreadable.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// The status line a row and the thread header show.
pub fn status_text(item: &Item) -> String {
    match (item.is_closed, item.status.trim().is_empty()) {
        (true, true) => chrome::CLOSED.to_string(),
        (true, false) => format!("{} · {}", item.status.trim(), chrome::CLOSED),
        (false, true) => String::new(),
        (false, false) => item.status.trim().to_string(),
    }
}

/// The title the add-task input takes, or `None` when it holds nothing but
/// whitespace.
pub fn parse_new_task(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
