//! The two views' pure half: the tree order the list draws, unread counts,
//! post folding, the post cursor, relative times, the header's narrow
//! decision, where a dragged row lands, and the two key maps.
//!
//! Nothing here builds an element, so the decisions both views are built on
//! are unit-testable on their own.

use std::collections::{HashMap, HashSet};

use gpui::{
    div, px, Bounds, Context, IntoElement, ParentElement as _, Pixels, Point, Render, Styled as _,
    Window,
};
use horizon_board::{tree_order, Item, Position};

use super::spec::{
    ACTION_PAD, BODY, CELL_EM, GAP_TIGHT, GAP_UNIT, HEADER_TITLE_MIN_CELLS, PAD_X, T1,
};
use crate::theme;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Everything the thread view can be asked to do. Keys, buttons, and clicks
/// all resolve to one of these, so a chord is attached to behaviour in
/// exactly one place ([`command_for_key`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ThreadCommand {
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
pub(crate) enum ListCommand {
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
pub(crate) fn command_for_key(key: &str) -> Option<ThreadCommand> {
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
pub(crate) fn list_command_for_key(key: &str) -> Option<ListCommand> {
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
pub(crate) struct Row {
    pub(crate) item: Item,
    /// How deep under a top-level task the row sits.
    pub(crate) depth: usize,
    /// Messages in this task nobody has read.
    pub(crate) unread: usize,
    /// The same, counted over this task and everything under it.
    pub(crate) subtree_unread: usize,
    /// Whether the task has children to disclose.
    pub(crate) has_children: bool,
    /// Whether those children are hidden right now.
    pub(crate) collapsed: bool,
    /// Whether a collapsed task above it hides this row.
    pub(crate) hidden: bool,
    /// Closed, or carrying the status the board uses for finished work.
    pub(crate) finished: bool,
    /// Whether the row belongs to the band at the bottom: a finished
    /// top-level task, and everything under it.
    pub(crate) in_finished_band: bool,
}

/// How many of `item`'s messages the reader has already seen: everything up
/// to and including the recorded position. A task with no position
/// recorded, or one whose position names a message the task no longer has,
/// has read nothing.
pub(crate) fn read_through(item: &Item, positions: &HashMap<u64, String>) -> usize {
    positions
        .get(&item.id)
        .and_then(|id| item.comments.iter().position(|comment| &comment.id == id))
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// How many of `item`'s messages come after the reader's recorded position.
pub(crate) fn unread_count(item: &Item, positions: &HashMap<u64, String>) -> usize {
    item.comments
        .len()
        .saturating_sub(read_through(item, positions))
}

/// Finished work: explicitly closed, or carrying the status the board uses
/// for a finished task.
pub(crate) fn is_finished(item: &Item) -> bool {
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
pub(crate) fn tree_rows(
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
pub(crate) fn visible_rows(rows: &[Row], finished_expanded: bool) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| !row.hidden && (finished_expanded || !row.in_finished_band))
        .map(|(index, _)| index)
        .collect()
}

/// How many top-level tasks the finished band holds, and how many unread
/// messages are folded away with them.
pub(crate) fn finished_summary(rows: &[Row]) -> (usize, usize) {
    rows.iter()
        .filter(|row| row.in_finished_band && row.depth == 0)
        .fold((0, 0), |(count, unread), row| {
            (count + 1, unread + row.subtree_unread)
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

/// The post the cursor lands on among `posts` posts. A folded post is still
/// a post: folding takes nothing out of the order, and `e` on a folded post
/// is what opens it.
pub(crate) fn step_post(posts: usize, cursor: Option<usize>, forward: bool) -> Option<usize> {
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
pub(crate) fn sibling_move(items: &[Item], id: u64, up: bool) -> Option<Position> {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropHalf {
    Above,
    Below,
}

/// The drop-half decision for a row during a drag, gated on the cursor
/// actually being over the row. Returns `None` when the cursor is outside
/// `row_bounds`, so the per-row `on_drag_move` caller skips writing the
/// shared drop indicator for rows the cursor isn't over.
///
/// `on_drag_move` is dispatched in the capture phase with no hit-test, so a
/// handler registered on every row fires for every row on each mouse move.
/// Without this containment guard every row would overwrite the single
/// indicator slot and the last row to handle would win, drawing the
/// indicator on the wrong row.
pub(crate) fn drop_half_for_row(
    cursor: &Point<Pixels>,
    row_bounds: &Bounds<Pixels>,
) -> Option<DropHalf> {
    if !row_bounds.contains(cursor) {
        return None;
    }
    let mid_y = row_bounds.origin.y + row_bounds.size.height / 2.0;
    if cursor.y < mid_y {
        Some(DropHalf::Above)
    } else {
        Some(DropHalf::Below)
    }
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
pub(crate) fn drop_position_from_half(
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

/// The drag payload for row reordering: carried by GPUI's native
/// `on_drag`/`on_drop` system. Also implements `Render` to produce the
/// ghost view that follows the cursor during the drag.
#[derive(Clone)]
pub(crate) struct BoardDragValue {
    pub(crate) item_id: u64,
    pub(crate) title: String,
}

impl Render for BoardDragValue {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(theme::surface_selected())
            .text_color(theme::readable_on(
                theme::text_primary(),
                theme::surface_selected(),
            ))
            .text_size(px(13.0))
            .child(self.title.clone())
    }
}

// ---------------------------------------------------------------------------
// The sessions a board binds
// ---------------------------------------------------------------------------

/// Every session id the board's tasks name, task bindings and reviewer
/// bindings alike, each reported once. An id that is not a uuid names no
/// session the shell can resolve, so it is dropped here.
pub(crate) fn bound_sessions(items: &[Item]) -> Vec<horizon_workspace::SessionId> {
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
pub(crate) fn task_session_id(item: &Item) -> Option<horizon_workspace::SessionId> {
    uuid::Uuid::parse_str(item.session_id.as_deref()?)
        .ok()
        .map(horizon_workspace::SessionId::from_uuid)
}

// ---------------------------------------------------------------------------
// What the reader has actually seen
// ---------------------------------------------------------------------------

/// Whether a post's marker is inside the scroll region that is on screen.
/// A post the viewport never covered was never displayed, so it never
/// advances the read position.
pub(crate) fn message_visible(marker: &Bounds<Pixels>, viewport: &Bounds<Pixels>) -> bool {
    marker.intersects(viewport)
}

// ---------------------------------------------------------------------------
// The task header band
// ---------------------------------------------------------------------------

/// How wide the header band has to be to keep its title and its actions on
/// one row: the band's own padding, the title's floor, the gap to the
/// actions, and the actions themselves at their label widths.
pub(crate) fn header_one_row_width(actions: &[&str]) -> Pixels {
    let cell = BODY.size * CELL_EM;
    let buttons = actions.iter().fold(px(0.0), |total, label| {
        total + cell * display_width(label) as f32 + ACTION_PAD
    });
    let gaps = GAP_TIGHT * actions.len().saturating_sub(1) as f32;
    PAD_X * 2.0 + measure_title_floor() + GAP_UNIT + buttons + gaps
}

fn measure_title_floor() -> Pixels {
    T1.size * (CELL_EM * HEADER_TITLE_MIN_CELLS)
}

/// Whether the band stacks into two rows — the title on its own, the chips
/// and the actions under it — instead of squeezing the title to nothing.
pub(crate) fn header_stacks(available: Pixels, actions: &[&str]) -> bool {
    available < header_one_row_width(actions)
}

// ---------------------------------------------------------------------------
// Folding by rendered length
// ---------------------------------------------------------------------------

/// A post taller than this many rendered lines is folded to a preview.
pub(crate) const FOLD_RENDERED_LINES: usize = 40;

/// How many of the post's own lines the preview keeps, on top of its first
/// heading.
pub(crate) const FOLD_PREVIEW_LINES: usize = 3;

/// How many cells `ch` occupies: two for the full-width ranges Japanese is
/// written in, one otherwise. The ranges are the East Asian Wide and
/// Fullwidth blocks, which is what the measure's "one glyph is two cells"
/// rests on.
pub(crate) fn cell_width(ch: char) -> usize {
    let code = ch as u32;
    let wide = matches!(code,
        0x1100..=0x115F      // Hangul Jamo
        | 0x2E80..=0x303E    // CJK radicals, kangxi, CJK punctuation
        | 0x3041..=0x33FF    // kana, bopomofo, compatibility
        | 0x3400..=0x4DBF    // CJK extension A
        | 0x4E00..=0x9FFF    // CJK unified
        | 0xA000..=0xA4CF    // Yi
        | 0xAC00..=0xD7A3    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFE10..=0xFE19
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60    // fullwidth forms
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F  // emoji
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD  // CJK extensions B and beyond
    );
    1 + usize::from(wide)
}

/// How many cells `line` occupies.
pub(crate) fn display_width(line: &str) -> usize {
    line.chars().map(cell_width).sum()
}

/// How many rows `text` takes in a column `cells` wide: each of its own
/// lines wraps as often as its width needs, and an empty line still takes a
/// row. A word-wrap boundary can move a character to the next row, so this
/// is the length the fold decision is made on rather than a promise about
/// the laid-out text.
pub(crate) fn rendered_lines(text: &str, cells: usize) -> usize {
    let cells = cells.max(1);
    text.lines()
        .map(|line| display_width(line).div_ceil(cells).max(1))
        .sum()
}

/// The head of a folded post, and how many rendered lines it hides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FoldPreview {
    pub(crate) head: String,
    pub(crate) hidden_lines: usize,
}

/// The preview for `text` in a column `cells` wide, or `None` when the post
/// is short enough to show whole. The preview is the post's first heading,
/// if it has one, plus its first [`FOLD_PREVIEW_LINES`] non-empty lines.
pub(crate) fn fold_preview(text: &str, cells: usize) -> Option<FoldPreview> {
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
        0..=59 => "たった今".to_string(),
        60..=3_599 => format!("{}分前", seconds / 60),
        3_600..=86_399 => format!("{}時間前", seconds / 3_600),
        86_400..=604_799 => format!("{}日前", seconds / 86_400),
        _ => format!("{}週間前", seconds / 604_800),
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

/// The list view's one header line: how much there is, and how much of it
/// is unread.
pub(crate) fn list_header_text(tasks: usize, unread: usize) -> String {
    if unread == 0 {
        format!("タスク {tasks}件")
    } else {
        format!("タスク {tasks}件 · 未読 {unread}件")
    }
}

/// What the list reports when a task is opened, in a build with no shell
/// above it to open anything. The guest build paints it; the native one
/// emits the request as an event instead.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub(crate) fn open_notice(title: &str) -> String {
    format!("スレッドを開く: {title}")
}

/// The title the add-task input takes, or `None` when it holds nothing but
/// whitespace.
pub(crate) fn parse_new_task(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bound_sessions, cell_width, command_for_key, display_width, drop_half_for_row,
        drop_position_from_half, finished_summary, fold_preview, header_one_row_width,
        header_stacks, is_finished, list_command_for_key, list_header_text, message_visible,
        open_notice, parse_new_task, relative_time, rendered_lines, sibling_move, status_text,
        step_post, step_selection, task_session_id, tree_rows, unread_count, visible_rows,
        DropHalf, ListCommand, ThreadCommand, Voice, FOLD_RENDERED_LINES,
    };
    use gpui::px;
    use horizon_board::{Comment, Item, Position};
    use std::collections::{HashMap, HashSet};

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

    fn child(id: u64, parent: u64, rank: &str) -> Item {
        Item {
            parent: Some(parent),
            ..task(id, rank)
        }
    }

    fn ids(rows: &[super::Row], expanded: bool) -> Vec<u64> {
        visible_rows(rows, expanded)
            .into_iter()
            .map(|index| rows[index].item.id)
            .collect()
    }

    #[test]
    fn the_thread_keys_move_a_post_cursor_and_the_list_keys_move_a_row() {
        assert_eq!(command_for_key("j"), Some(ThreadCommand::NextPost));
        assert_eq!(command_for_key("down"), Some(ThreadCommand::NextPost));
        assert_eq!(command_for_key("k"), Some(ThreadCommand::PreviousPost));
        assert_eq!(command_for_key("up"), Some(ThreadCommand::PreviousPost));
        assert_eq!(command_for_key("e"), Some(ThreadCommand::ToggleCurrentPost));
        assert_eq!(command_for_key("enter"), Some(ThreadCommand::FocusComposer));
        assert_eq!(
            command_for_key("escape"),
            Some(ThreadCommand::LeaveComposer)
        );
        // The list's own keys are not the thread's.
        assert_eq!(command_for_key("o"), None);
        assert_eq!(command_for_key("l"), None);
        assert_eq!(command_for_key("x"), None);

        assert_eq!(list_command_for_key("j"), Some(ListCommand::SelectNext));
        assert_eq!(list_command_for_key("down"), Some(ListCommand::SelectNext));
        assert_eq!(list_command_for_key("k"), Some(ListCommand::SelectPrevious));
        assert_eq!(
            list_command_for_key("up"),
            Some(ListCommand::SelectPrevious)
        );
        assert_eq!(list_command_for_key("l"), Some(ListCommand::Expand));
        assert_eq!(list_command_for_key("right"), Some(ListCommand::Expand));
        assert_eq!(list_command_for_key("h"), Some(ListCommand::Collapse));
        assert_eq!(list_command_for_key("left"), Some(ListCommand::Collapse));
        assert_eq!(list_command_for_key("o"), Some(ListCommand::ToggleFinished));
        assert_eq!(list_command_for_key("enter"), Some(ListCommand::OpenThread));
        assert_eq!(list_command_for_key("e"), None);
    }

    #[test]
    fn the_post_cursor_stops_at_both_ends_and_ignores_folding() {
        assert_eq!(step_post(0, None, true), None);
        assert_eq!(step_post(3, None, true), Some(0));
        assert_eq!(step_post(3, None, false), Some(0));
        assert_eq!(step_post(3, Some(0), true), Some(1));
        assert_eq!(step_post(3, Some(2), true), Some(2));
        assert_eq!(step_post(3, Some(1), false), Some(0));
        assert_eq!(step_post(3, Some(0), false), Some(0));
        // A cursor left behind by a thread that shrank lands inside it.
        assert_eq!(step_post(2, Some(9), true), Some(1));

        // Folding is not part of the order: a thread of three posts steps
        // the same however many of them are folded away.
        let long = "本文。\n".repeat(200);
        assert!(fold_preview(&long, 72).is_some());
        let folded_thread = [long.as_str(), "短い", long.as_str()];
        let mut cursor = None;
        let mut visited = Vec::new();
        for _ in 0..folded_thread.len() {
            cursor = step_post(folded_thread.len(), cursor, true);
            visited.push(cursor);
        }
        assert_eq!(visited, vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn the_header_stacks_once_the_title_would_lose_its_floor() {
        let actions = ["返信", "閉じる", "セッション"];
        let needed = header_one_row_width(&actions);
        assert!(!header_stacks(needed, &actions));
        assert!(!header_stacks(needed + px(200.0), &actions));
        assert!(header_stacks(needed - px(1.0), &actions));
        // Fewer actions need less room, so the same pane stacks later.
        let fewer = ["返信", "閉じる"];
        assert!(header_one_row_width(&fewer) < needed);
        assert!(!header_stacks(needed, &fewer));
        // A pane the width of a narrow split stacks; a wide one does not.
        assert!(header_stacks(px(360.0), &actions));
        assert!(!header_stacks(px(900.0), &actions));
    }

    #[test]
    fn the_list_header_names_the_unread_only_when_there_is_some() {
        assert_eq!(list_header_text(25, 3), "タスク 25件 · 未読 3件");
        assert_eq!(list_header_text(0, 0), "タスク 0件");
    }

    #[test]
    fn opening_a_task_is_reported_with_its_title() {
        assert_eq!(
            open_notice("貼り付け時に末尾の改行が落ちる"),
            "スレッドを開く: 貼り付け時に末尾の改行が落ちる"
        );
    }

    #[test]
    fn a_blank_add_task_input_adds_nothing() {
        assert_eq!(
            parse_new_task("   新しいタスク "),
            Some("新しいタスク".into())
        );
        assert_eq!(parse_new_task("   "), None);
        assert_eq!(parse_new_task(""), None);
    }

    #[test]
    fn a_japanese_glyph_is_two_cells_and_latin_is_one() {
        assert_eq!(cell_width('あ'), 2);
        assert_eq!(cell_width('録'), 2);
        assert_eq!(cell_width('、'), 2);
        assert_eq!(cell_width('a'), 1);
        assert_eq!(cell_width(' '), 1);
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("あい"), 4);
        assert_eq!(display_width("あa"), 3);
    }

    #[test]
    fn a_line_wraps_as_often_as_the_column_needs() {
        assert_eq!(rendered_lines("", 72), 0, "no lines at all");
        assert_eq!(rendered_lines("short", 72), 1);
        // 72 cells is 36 full-width glyphs, so 37 of them take two rows.
        assert_eq!(rendered_lines(&"あ".repeat(36), 72), 1);
        assert_eq!(rendered_lines(&"あ".repeat(37), 72), 2);
        // An empty line between paragraphs still takes a row.
        assert_eq!(rendered_lines("a\n\nb", 72), 3);
    }

    #[test]
    fn a_folded_post_keeps_its_heading_and_first_lines() {
        let short = "一行だけ";
        assert_eq!(fold_preview(short, 72), None);

        let body: String = (0..60)
            .map(|index| format!("{index}行目の本文。\n\n"))
            .collect();
        let text = format!("## 見出し\n\n{body}");
        assert!(rendered_lines(&text, 72) > FOLD_RENDERED_LINES);
        let preview = fold_preview(&text, 72).expect("a long post folds");
        assert!(preview.head.starts_with("## 見出し"));
        assert!(preview.head.contains("0行目"));
        assert!(preview.head.contains("2行目"));
        assert!(
            !preview.head.contains("3行目"),
            "the preview keeps three lines under the heading: {:?}",
            preview.head
        );
        assert!(preview.hidden_lines > 0);
        assert_eq!(
            preview.hidden_lines,
            rendered_lines(&text, 72) - rendered_lines(&preview.head, 72)
        );

        // A post with no heading keeps its first lines only.
        let plain = fold_preview(&body, 72).expect("a long post folds");
        assert!(plain.head.starts_with("0行目"));
        assert!(!plain.head.contains("3行目"));
    }

    #[test]
    fn a_narrow_column_folds_a_post_a_wide_one_shows_whole() {
        let text: String = (0..12)
            .map(|index| format!("{index}行目、幅で折り返しの回数が変わる長さの段落。\n"))
            .collect();
        assert!(fold_preview(&text, 200).is_none());
        assert!(fold_preview(&text, 8).is_some());
    }

    #[test]
    fn ages_read_in_the_largest_unit_that_fits() {
        let now = 10_000_000_000u64;
        assert_eq!(relative_time(now, now), "たった今");
        assert_eq!(relative_time(now - 120_000, now), "2分前");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3時間前");
        assert_eq!(relative_time(now - 2 * 86_400_000, now), "2日前");
        assert_eq!(relative_time(now - 21 * 86_400_000, now), "3週間前");
        // A clock behind the message reads as now rather than underflowing.
        assert_eq!(relative_time(now + 1_000, now), "たった今");
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

    /// The owner's own order is the list's order: rank, with children under
    /// their parent and finished top-level work at the end.
    #[test]
    fn rows_follow_rank_as_a_tree_with_finished_top_level_work_last() {
        let items = vec![
            task(1, "a"),
            child(11, 1, "b"),
            child(10, 1, "a"),
            task(2, "b"),
            {
                let mut done = task(3, "c");
                done.status = "done".into();
                done
            },
            child(30, 3, "a"),
        ];
        let rows = tree_rows(&items, &HashMap::new(), &HashSet::new());
        assert_eq!(ids(&rows, false), vec![1, 10, 11, 2]);
        assert_eq!(ids(&rows, true), vec![1, 10, 11, 2, 3, 30]);
        // A finished child of an open parent stays under it rather than
        // moving into the band.
        assert_eq!(finished_summary(&rows), (1, 0));
        let depths: Vec<usize> = rows.iter().map(|row| row.depth).collect();
        assert_eq!(depths, vec![0, 1, 1, 0, 0, 1]);
        assert!(rows[0].has_children);
        assert!(!rows[1].has_children);
    }

    #[test]
    fn a_finished_child_stays_under_its_open_parent() {
        let mut done_child = child(10, 1, "a");
        done_child.is_closed = true;
        let items = vec![task(1, "a"), done_child, task(2, "b")];
        let rows = tree_rows(&items, &HashMap::new(), &HashSet::new());
        assert_eq!(ids(&rows, false), vec![1, 10, 2]);
        assert_eq!(finished_summary(&rows), (0, 0));
        assert!(rows[1].finished);
        assert!(!rows[1].in_finished_band);
    }

    #[test]
    fn a_collapsed_parent_hides_its_whole_subtree() {
        let items = vec![
            task(1, "a"),
            child(10, 1, "a"),
            child(100, 10, "a"),
            task(2, "b"),
        ];
        let collapsed = HashSet::from([1]);
        let rows = tree_rows(&items, &HashMap::new(), &collapsed);
        assert_eq!(ids(&rows, false), vec![1, 2]);
        assert!(rows[0].collapsed);
        assert!(rows[1].hidden && rows[2].hidden);
        // Collapsing the inner one hides only what is under it.
        let rows = tree_rows(&items, &HashMap::new(), &HashSet::from([10]));
        assert_eq!(ids(&rows, false), vec![1, 10, 2]);
        // A task with no children never reads as collapsed, so no
        // disclosure affordance appears on it.
        let rows = tree_rows(&items, &HashMap::new(), &HashSet::from([2]));
        assert!(!rows[3].collapsed);
    }

    #[test]
    fn a_parent_carries_the_unread_of_everything_under_it() {
        let mut parent = task(1, "a");
        parent.comments = vec![message("p1")];
        let mut kid = child(10, 1, "a");
        kid.comments = vec![message("c1"), message("c2")];
        let mut grandkid = child(100, 10, "a");
        grandkid.comments = vec![message("g1")];
        let items = vec![parent, kid, grandkid];
        let rows = tree_rows(&items, &HashMap::new(), &HashSet::new());
        assert_eq!(rows[0].unread, 1);
        assert_eq!(rows[0].subtree_unread, 4);
        assert_eq!(rows[1].subtree_unread, 3);
        assert_eq!(rows[2].subtree_unread, 1);

        // Reading the parent's own message leaves the subtree count on it.
        let positions = HashMap::from([(1, "p1".to_string())]);
        let rows = tree_rows(&items, &positions, &HashSet::new());
        assert_eq!(rows[0].unread, 0);
        assert_eq!(rows[0].subtree_unread, 3);
    }

    /// A finished top-level task nobody has read still reports its unread
    /// from the band's own row, so nothing disappears by being folded.
    #[test]
    fn the_finished_band_reports_the_unread_it_folds_away() {
        let mut done = task(3, "c");
        done.status = "done".into();
        done.comments = vec![message("m1")];
        let rows = tree_rows(&[task(1, "a"), done], &HashMap::new(), &HashSet::new());
        assert_eq!(finished_summary(&rows), (1, 1));
        assert_eq!(ids(&rows, false), vec![1]);
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
    fn reorder_stays_within_siblings_even_with_interleaved_descendants() {
        let tasks = vec![task(1, "a"), child(2, 1, "a"), task(3, "b")];
        assert_eq!(sibling_move(&tasks, 3, true), Some(Position::Before(1)));
        assert_eq!(sibling_move(&tasks, 1, true), None);
        assert_eq!(sibling_move(&tasks, 2, false), None);
        assert_eq!(drop_position_from_half(2, &tasks, 3, DropHalf::Above), None);
    }

    #[test]
    fn drag_uses_real_sibling_order_including_hidden_tasks() {
        let a = task(1, "a");
        let mut hidden = task(2, "b");
        hidden.is_closed = true;
        let b = task(3, "c");
        let kid = child(4, 1, "a");
        let items = vec![b, kid, hidden, a];
        assert_eq!(
            drop_position_from_half(1, &items, 3, DropHalf::Above),
            Some(Position::Before(3))
        );
        assert_eq!(drop_position_from_half(3, &items, 2, DropHalf::Below), None);
    }

    #[test]
    fn the_drop_half_is_the_row_the_cursor_is_inside() {
        use gpui::{bounds, point, size};
        let row = bounds(point(px(0.), px(100.)), size(px(300.), px(48.)));
        assert_eq!(
            drop_half_for_row(&point(px(10.), px(110.)), &row),
            Some(DropHalf::Above)
        );
        assert_eq!(
            drop_half_for_row(&point(px(10.), px(140.)), &row),
            Some(DropHalf::Below)
        );
        // A row the cursor is not over writes no indicator at all.
        assert_eq!(drop_half_for_row(&point(px(10.), px(60.)), &row), None);
    }

    #[test]
    fn long_consultation_viewport_excludes_unseen_messages() {
        use gpui::{bounds, point, size};
        let viewport = bounds(point(px(0.), px(210.)), size(px(300.), px(60.)));
        let markers =
            [20., 120., 220.].map(|y| bounds(point(px(0.), px(y)), size(px(300.), px(80.))));
        assert!(!message_visible(&markers[0], &viewport));
        assert!(!message_visible(&markers[1], &viewport));
        assert!(message_visible(&markers[2], &viewport));
    }

    #[test]
    fn refreshed_bindings_include_task_and_reviewer_once_each() {
        let task_session = uuid::Uuid::new_v4();
        let reviewer = uuid::Uuid::new_v4();
        let mut a = task(1, "a");
        a.session_id = Some(task_session.to_string());
        a.review_session_id = Some(reviewer.to_string());
        let mut b = task(2, "b");
        b.session_id = Some(task_session.to_string());
        b.review_session_id = Some("invalid".into());
        assert_eq!(
            bound_sessions(&[a.clone(), b]),
            vec![
                horizon_workspace::SessionId::from_uuid(task_session),
                horizon_workspace::SessionId::from_uuid(reviewer)
            ]
        );
        // Only the task's own binding is what the session action opens.
        assert_eq!(
            task_session_id(&a),
            Some(horizon_workspace::SessionId::from_uuid(task_session))
        );
        assert_eq!(task_session_id(&task(9, "z")), None);
    }

    #[test]
    fn authorship_splits_into_three_voices() {
        assert_eq!(super::voice("owner"), Voice::Owner);
        assert_eq!(super::voice("system"), Voice::System);
        assert_eq!(super::voice("agent"), Voice::Agent);
        assert_eq!(super::voice("reviewer"), Voice::Agent);
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
