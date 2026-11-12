//! The two views' pure half is `horizon-board-view`'s `model`, shared with
//! the Blitz shell. What is here is the part in gpui's units: where the
//! pointer is over a row, the row being dragged, whether a post is in
//! view, and the header's narrow decision.

use gpui::{
    div, px, Bounds, Context, IntoElement, ParentElement as _, Pixels, Point, Render, Styled as _,
    Window,
};

pub(crate) use horizon_board_view::model::*;

use super::spec::{action_pad, CELL_EM, HEADER_TITLE_MIN_CELLS};
use crate::theme;
use crate::ui::spec::{body, gap_tight, gap_unit, pad_x, t1};

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

/// Whether a post's marker is inside the scroll region that is on screen.
/// A post the viewport never covered was never displayed, so it never
/// advances the read position.
pub(crate) fn message_visible(marker: &Bounds<Pixels>, viewport: &Bounds<Pixels>) -> bool {
    marker.intersects(viewport)
}

/// How wide the header band has to be to keep its title and its actions on
/// one row: the band's own padding, the title's floor, the gap to the
/// actions, and the actions themselves at their label widths.
pub(crate) fn header_one_row_width(actions: &[&str]) -> Pixels {
    let cell = body().size * CELL_EM;
    let buttons = actions.iter().fold(px(0.0), |total, label| {
        total + cell * display_width(label) as f32 + action_pad()
    });
    let gaps = gap_tight() * actions.len().saturating_sub(1) as f32;
    pad_x() * 2.0 + measure_title_floor() + gap_unit() + buttons + gaps
}

fn measure_title_floor() -> Pixels {
    t1().size * (CELL_EM * HEADER_TITLE_MIN_CELLS)
}

/// Whether the band stacks into two rows — the title on its own, the chips
/// and the actions under it — instead of squeezing the title to nothing.
pub(crate) fn header_stacks(available: Pixels, actions: &[&str]) -> bool {
    available < header_one_row_width(actions)
}

#[cfg(test)]
mod tests {
    use super::{
        bound_sessions, command_for_key, drop_half_for_row, drop_position_from_half,
        finished_summary, fold_preview, header_one_row_width, header_stacks, is_finished,
        list_command_for_key, message_visible, parse_new_task, rendered_lines, sibling_move,
        status_text, step_post, step_selection, task_session_id, tree_rows, unread_count,
        visible_rows, DropHalf, ListCommand, ThreadCommand, Voice, FOLD_RENDERED_LINES,
    };
    use crate::board::chrome;
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
        let actions = [chrome::REPLY, chrome::CLOSE, chrome::SESSION];
        let needed = header_one_row_width(&actions);
        assert!(!header_stacks(needed, &actions));
        assert!(!header_stacks(needed + px(200.0), &actions));
        assert!(header_stacks(needed - px(1.0), &actions));
        // Fewer actions need less room, so the same pane stacks later.
        let fewer = [chrome::REPLY, chrome::CLOSE];
        assert!(header_one_row_width(&fewer) < needed);
        assert!(!header_stacks(needed, &fewer));
        // A pane the width of a narrow split stacks; a wide one does not.
        assert!(header_stacks(px(360.0), &actions));
        assert!(!header_stacks(px(900.0), &actions));
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
