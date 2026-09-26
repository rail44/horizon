use super::*;

// The tree-independent half of the pane's model — where a dragged row
// lands, how a task moves among its siblings, which sessions the board
// binds, and whether a message was on screen — lives in
// [`crate::board_next::model`], next to the views that will replace this
// pane.
pub(super) use crate::board_next::model::{
    bound_sessions, drop_half_for_row, drop_position_from_half, message_visible, sibling_move,
    BoardDragValue, DropHalf,
};

/// Flattens `items` into parent→child tree order for display, returning the
/// display list and a parallel depth vector. When `top_level_only` is true,
/// only true roots (items whose parent is `None`) are
/// returned, all at depth 0. Delegates to [`horizon_board::tree_order`] for
/// the ordering logic; this wrapper converts references to owned `Item`s.
pub(super) fn flatten_with_depth(items: &[Item], top_level_only: bool) -> (Vec<Item>, Vec<usize>) {
    let ordered = tree_order(items, false);
    let ordered: Vec<_> = ordered
        .into_iter()
        .filter(|(item, _)| !top_level_only || item.parent.is_none())
        .collect();
    let filtered = ordered.iter().map(|(item, _)| (*item).clone()).collect();
    let depths = ordered.iter().map(|(_, depth)| *depth).collect();
    (filtered, depths)
}

/// Formats a unix-millisecond timestamp as `YYYY-MM-DD HH:MM` (UTC) via a
/// small civil-date conversion (Howard Hinnant's days-from-civil algorithm),
/// so the shell does not pull in a datetime crate just for this column.
pub(super) fn format_timestamp(unix_ms: u64) -> String {
    let secs = (unix_ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    // Civil date from days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

/// The pure fallback behind the pane's root resolution, kept free of
/// `WorkspaceShell`/`App` so it's unit-testable without a GPUI window: the
/// active session's `workspace_root` wins; when that is absent (no active
/// session, or a terminal/resumed session with no recorded root -- the common
/// terminal-only state) the shell process's own cwd stands in. Both are
/// *starting* directories -- `Store::from_dir` does the worktree -> main-root
/// collapse.
#[cfg(not(target_family = "wasm"))]
pub(super) fn board_root_dir(
    session_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    session_root.or(cwd)
}

/// The pure decision behind the list's `ListEvent::Confirm` handler: a
/// confirm on a row opens the detail view *iff* the row has an item and the
/// pane has a store to re-read it from. Extracted so the event→transition
/// mapping is unit-testable without a GPUI window.
pub(super) fn board_confirm_transition(item: Option<Item>, has_store: bool) -> Option<Item> {
    item.filter(|_| has_store)
}

/// The pure validation behind the pane's add-item composer: returns the
/// trimmed title when the input is non-blank, or `None` so the caller can
/// no-op on empty. Trimming stops a whitespace-only submit from creating
/// an untitled item.
pub(super) fn parse_new_item(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The first row is selectable exactly when the list isn't empty -- the
/// pure predicate behind [`select_first_row_on_open`].
pub(super) fn first_row_to_select(items_count: usize) -> Option<IndexPath> {
    (items_count > 0).then(IndexPath::default)
}

/// Selects the first row right after a searchable `List` is constructed, so
/// a bare Enter on open runs it without arrowing down first. A no-op when
/// the delegate starts empty.
pub(super) fn select_first_row_on_open<D: ListDelegate>(
    list: &mut ListState<D>,
    window: &mut Window,
    cx: &mut Context<ListState<D>>,
) {
    if let Some(ix) = first_row_to_select(list.delegate().items_count(0, cx)) {
        list.set_selected_index(Some(ix), window, cx);
    }
}

pub(super) fn task_state(item: &Item) -> String {
    match (item.is_closed, item.status.is_empty()) {
        (true, true) => "Closed".into(),
        (true, false) => format!("{} · Closed", item.status),
        (false, _) => item.status.clone(),
    }
}

pub(super) fn unread_tasks(
    items: &[Item],
    positions: &std::collections::HashMap<u64, String>,
) -> std::collections::HashSet<u64> {
    let mut unread = std::collections::HashSet::new();
    for item in items {
        if item
            .comments
            .last()
            .is_none_or(|comment| positions.get(&item.id) == Some(&comment.id))
        {
            continue;
        }
        let mut current = Some(item.id);
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = current {
            if !visited.insert(id) {
                break;
            }
            unread.insert(id);
            current = items
                .iter()
                .find(|item| item.id == id)
                .and_then(|item| item.parent);
        }
    }
    unread
}

pub(super) fn navigation_matches(
    request_epoch: u64,
    current_epoch: u64,
    task: u64,
    current_task: Option<u64>,
) -> bool {
    request_epoch == current_epoch && current_task == Some(task)
}

#[cfg(test)]
mod tests {
    use super::{flatten_with_depth, navigation_matches, unread_tasks};
    use horizon_board::Item;
    fn task(id: u64, parent: Option<u64>, rank: &str) -> Item {
        Item {
            id,
            parent,
            rank: rank.into(),
            ..Default::default()
        }
    }
    fn message(id: &str) -> horizon_board::Comment {
        horizon_board::Comment {
            id: id.into(),
            author: "owner".into(),
            text: "Message".into(),
            at: None,
            source: None,
        }
    }
    #[test]
    fn top_level_filter_does_not_promote_filtered_orphans() {
        let parent = task(1, None, "a");
        let child = task(2, Some(1), "a");
        assert_eq!(
            flatten_with_depth(&[parent, child.clone()], true).0.len(),
            1
        );
        assert!(flatten_with_depth(&[child], true).0.is_empty());
    }
    #[test]
    fn unread_descendants_remain_when_parent_is_read() {
        let mut parent = task(1, None, "a");
        parent.comments.push(message("parent"));
        let mut child = task(2, Some(1), "a");
        child.comments.push(message("child"));
        let mut positions = std::collections::HashMap::from([(1, "parent".into())]);
        assert_eq!(
            unread_tasks(&[parent.clone(), child.clone()], &positions),
            std::collections::HashSet::from([1, 2])
        );
        positions.insert(2, "child".into());
        assert!(unread_tasks(&[parent, child], &positions).is_empty());
    }
    #[test]
    fn jumping_to_last_message_reads_the_whole_thread_until_a_new_post() {
        let mut item = task(1, None, "a");
        item.comments = vec![message("z"), message("a"), message("m")];
        let mut positions = std::collections::HashMap::from([(1, "a".into())]);
        assert!(unread_tasks(&[item.clone()], &positions).contains(&1));
        positions.insert(1, "m".into());
        assert!(unread_tasks(&[item.clone()], &positions).is_empty());
        item.comments.push(message("new"));
        assert!(unread_tasks(&[item.clone()], &positions).contains(&1));
        positions.insert(1, "new".into());
        assert!(unread_tasks(&[item], &positions).is_empty());
    }
    #[test]
    fn async_results_require_same_navigation_generation() {
        assert!(navigation_matches(2, 2, 7, Some(7)));
        assert!(!navigation_matches(2, 3, 7, Some(7)));
        assert!(!navigation_matches(2, 2, 7, Some(8)));
        assert!(!navigation_matches(2, 2, 7, None));
    }
    #[test]
    fn search_preserves_hierarchy_and_never_promotes_filtered_children() {
        let mut a = task(1, None, "a");
        a.title = "Alpha".into();
        let mut list = super::super::list::BoardListDelegate::new();
        let mut child = task(2, Some(1), "a");
        child.title = "Child".into();
        list.set_loaded(vec![a, child]);
        list.last_query = "CHILD".into();
        list.rederive();
        assert_eq!(list.filtered[0].id, 2);
        assert_eq!(list.depths, vec![1]);
        list.top_level_only = true;
        list.rederive();
        assert!(list.filtered.is_empty());
    }

    #[test]
    fn closure_filter_preserves_open_children_search_and_unread_history() {
        let mut parent = task(1, None, "a");
        parent.title = "Parent".into();
        parent.is_closed = true;
        let mut child = task(2, Some(1), "a");
        child.title = "Child".into();
        let mut other = task(3, None, "b");
        other.title = "Other".into();
        // Status text never controls visibility.
        other.status = "archived".into();
        let mut list = super::super::list::BoardListDelegate::new();
        list.unread.insert(1);
        list.set_loaded(vec![parent, child, other]);
        assert_eq!(
            list.filtered.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(list.depths, vec![0, 0]);
        assert_eq!(list.all.len(), 3);
        assert!(list.unread.contains(&1));
        list.show_closed = true;
        list.rederive();
        assert_eq!(
            list.filtered.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(list.depths, vec![0, 1, 0]);
        list.last_query = "Parent".into();
        list.rederive();
        assert_eq!(list.filtered[0].id, 1);
        list.show_closed = false;
        list.rederive();
        assert!(list.filtered.is_empty());
        list.last_query.clear();
        list.top_level_only = true;
        list.rederive();
        assert_eq!(
            list.filtered.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![3]
        );
    }
}
