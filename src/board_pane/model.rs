use super::*;
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

/// The `Position` (if any) for dropping `dragged_id` onto the given `half`
/// of `target_id`'s row, or `None` when the drop is a no-op -- it would
/// leave the item where it already is.
///
/// `Above` maps to `Before(target_id)` and `Below` to `After(target_id)`,
/// but both are suppressed when the resulting insertion point equals the
/// dragged item's current position. A drop is a no-op when the dragged
/// and target items are the same, or when dropping on the near half of an
/// adjacent row (the half that faces the dragged item): `Above` on the row
/// immediately *below* the dragged item, or `Below` on the row immediately
/// *above* it. This is the invariant behind the indicator: a position that
/// shows an indicator is always one where a drop will execute a move.
pub(super) fn drop_position_from_half(
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
    let di = items.iter().position(|i| i.id == dragged_id)?;
    let ti = items.iter().position(|i| i.id == target_id)?;
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

/// Which half of a row the cursor is in during a drag, used to decide
/// whether the drop indicator line shows above or below the row and
/// whether the move is `Before` or `After`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DropHalf {
    Above,
    Below,
}

/// The drop-half decision for a row during a drag, gated on the cursor
/// actually being over the row. Returns `None` when the cursor is outside
/// `row_bounds`, so the per-row `on_drag_move` caller skips writing the
/// shared `drop_indicator` for rows the cursor isn't over.
///
/// `on_drag_move` is dispatched in the capture phase with no hit-test, so a
/// handler registered on every row fires for every row on each mouse move.
/// Without this containment guard every row would overwrite the single
/// `drop_indicator` slot and the last row to handle would win, drawing the
/// indicator on the wrong row. The drop itself is dispatched at the list level
/// (a single `on_drop` on the list wrapper) and reads the target from
/// `drop_indicator`, so a release anywhere over the list -- row, gap, or
/// padding -- executes the insertion the indicator was showing.
pub(super) fn drop_half_for_row(
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

/// The drag payload for board item reordering: carried by GPUI's native
/// `on_drag`/`on_drop` system. Also implements `Render` to produce the
/// ghost view that follows the cursor during the drag.
#[derive(Clone)]
pub(super) struct BoardDragValue {
    pub(super) item_id: u64,
    pub(super) title: String,
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

/// What a live-update poke should refresh: the whole item list, or just the
/// currently-open detail item. The pure decision behind [`BoardPaneView::on_poke`],
/// extracted so the poke->reload mapping is unit-testable without a GPUI window.
pub(super) enum PokeReloadTarget {
    /// Reload the full list (list mode).
    List,
    /// Reload just this item (detail mode).
    Item(u64),
}

/// The pure decision behind a live-update poke: `None` (list view, no item
/// open) reloads the whole list; `Some(id)` (a detail view open on `id`)
/// reloads just that item.
pub(super) fn poke_reload_target(open_item_id: Option<u64>) -> PokeReloadTarget {
    match open_item_id {
        Some(id) => PokeReloadTarget::Item(id),
        None => PokeReloadTarget::List,
    }
}

/// The pure fallback behind the pane's root resolution, kept free of
/// `WorkspaceShell`/`App` so it's unit-testable without a GPUI window: the
/// active session's `workspace_root` wins; when that is absent (no active
/// session, or a terminal/resumed session with no recorded root -- the common
/// terminal-only state) the shell process's own cwd stands in. Both are
/// *starting* directories -- `Store::from_dir` does the worktree -> main-root
/// collapse.
pub(crate) fn board_root_dir(
    session_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    session_root.or(cwd)
}

/// The pure decision behind the list's `ListEvent::Confirm` handler: a
/// confirm on a row opens the detail view *iff* both the row's item and a
/// resolvable store root are present. Extracted so the event→transition
/// mapping is unit-testable without a GPUI window.
pub(super) fn board_confirm_transition(
    item: Option<Item>,
    root: Option<PathBuf>,
) -> Option<(Item, PathBuf)> {
    let item = item?;
    let root = root?;
    Some((item, root))
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

pub(super) fn bound_sessions(items: &[Item]) -> Vec<horizon_workspace::SessionId> {
    let mut seen = std::collections::HashSet::new();
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

pub(super) fn task_state(item: &Item) -> String {
    match (item.completed, item.status.is_empty()) {
        (true, true) => "Completed".into(),
        (true, false) => format!("{} · Completed", item.status),
        (false, _) => item.status.clone(),
    }
}

/// The tone for a task's displayed state: `completed` is the one state fact
/// the product itself vouches for (the separate completion flag), so it
/// earns the success tone. The free-form `status` text is project-defined,
/// so it stays muted rather than inventing semantics for project-specific
/// words.
pub(super) fn task_state_color(item: &Item) -> Hsla {
    if item.completed {
        theme::success()
    } else {
        theme::text_muted()
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

pub(super) fn sibling_move(items: &[Item], id: u64, up: bool) -> Option<Position> {
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

pub(super) fn message_visible(marker: &Bounds<Pixels>, viewport: &Bounds<Pixels>) -> bool {
    marker.intersects(viewport)
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
    use super::{
        drop_position_from_half, flatten_with_depth, message_visible, navigation_matches,
        sibling_move, unread_tasks, DropHalf,
    };
    use horizon_board::{Item, Position};
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
    fn refreshed_bindings_include_task_and_reviewer_once_each() {
        let task_id = uuid::Uuid::new_v4();
        let reviewer_id = uuid::Uuid::new_v4();
        let mut a = task(1, None, "a");
        a.session_id = Some(task_id.to_string());
        a.review_session_id = Some(reviewer_id.to_string());
        let mut b = task(2, None, "b");
        b.session_id = Some(task_id.to_string());
        b.review_session_id = Some("invalid".into());
        assert_eq!(
            super::bound_sessions(&[a, b]),
            vec![
                horizon_workspace::SessionId::from_uuid(task_id),
                horizon_workspace::SessionId::from_uuid(reviewer_id)
            ]
        );
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
    fn long_consultation_viewport_excludes_unseen_messages() {
        use gpui::{bounds, point, px, size};
        let viewport = bounds(point(px(0.), px(210.)), size(px(300.), px(60.)));
        let markers =
            [20., 120., 220.].map(|y| bounds(point(px(0.), px(y)), size(px(300.), px(80.))));
        assert!(!message_visible(&markers[0], &viewport));
        assert!(!message_visible(&markers[1], &viewport));
        assert!(message_visible(&markers[2], &viewport));
    }
    #[test]
    fn async_results_require_same_navigation_generation() {
        assert!(navigation_matches(2, 2, 7, Some(7)));
        assert!(!navigation_matches(2, 3, 7, Some(7)));
        assert!(!navigation_matches(2, 2, 7, Some(8)));
        assert!(!navigation_matches(2, 2, 7, None));
    }
    #[test]
    fn reorder_stays_within_siblings_even_with_interleaved_descendants() {
        let tasks = vec![
            task(1, None, "a"),
            task(2, Some(1), "a"),
            task(3, None, "b"),
        ];
        assert_eq!(sibling_move(&tasks, 3, true), Some(Position::Before(1)));
        assert_eq!(drop_position_from_half(2, &tasks, 3, DropHalf::Above), None);
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
}
