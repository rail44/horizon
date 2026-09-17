use super::*;
pub(super) struct BoardListDelegate {
    pub(super) all: Vec<Item>,
    pub(super) unread: std::collections::HashSet<u64>,
    pub(super) session_states:
        std::collections::HashMap<horizon_workspace::SessionId, BoardSessionState>,
    pub(super) filtered: Vec<Item>,
    /// Display depth per row in `filtered`, parallel to it. 0 for top-level
    /// items, incremented for each level of nesting under a parent. Rebuilt
    /// alongside `filtered` by `rederive`.
    pub(super) depths: Vec<usize>,
    /// Whether to show only top-level items (the "roadmap view") or the full
    /// parent→child tree. Toggled by `BoardPaneView::toggle_expansion`.
    pub(super) top_level_only: bool,
    /// The last search query passed to `perform_search`, saved so `rederive`
    /// can re-filter when the toggle changes without needing access to the
    /// `ListState`'s internal query state.
    pub(super) last_query: String,
    pub(super) selected: Option<IndexPath>,
    pub(super) loading: bool,
    /// Back-reference to the pane view, set after construction so that the
    /// `on_drag_move` callback in `render_item` can reach the view's methods.
    /// Weak to avoid a reference cycle: the view owns the list (strong
    /// `Entity`), so the delegate must hold a `WeakEntity` back -- a strong
    /// `Entity` here would prevent the view's `Drop` from running, leaking the
    /// logd subscribe thread and socket on every pane close.
    pub(super) view: Option<WeakEntity<BoardPaneView>>,
    /// The row and half the cursor is hovering over during an active drag,
    /// for the drop indicator line. Set by `on_drag_move` only at non-no-op
    /// positions (see `drop_position_from_half`); cleared on drop, when the
    /// cursor moves onto a no-op position, or when the drag ends.
    pub(super) drop_indicator: Option<(u64, DropHalf)>,
}

impl BoardListDelegate {
    pub(super) fn new() -> Self {
        Self {
            all: Vec::new(),
            unread: Default::default(),
            session_states: Default::default(),
            filtered: Vec::new(),
            depths: Vec::new(),
            top_level_only: false,
            last_query: String::new(),
            selected: None,
            loading: true,
            view: None,
            drop_indicator: None,
        }
    }

    /// Re-derives `filtered` and `depths` from `all` using the current
    /// `last_query` and `top_level_only`. The single place that rebuilds the
    /// display list — called after every load, search, and toggle.
    pub(super) fn rederive(&mut self) {
        let (items, depths) = flatten_with_depth(&self.all, self.top_level_only);
        let (filtered, depths): (Vec<_>, Vec<_>) = items
            .into_iter()
            .zip(depths)
            .filter(|(item, _)| {
                self.last_query.trim().is_empty()
                    || item
                        .title
                        .to_lowercase()
                        .contains(&self.last_query.trim().to_lowercase())
            })
            .unzip();
        self.filtered = filtered;
        self.depths = depths;
    }

    /// Replaces the loaded items (after the off-thread read returns) and
    /// clears the loading state. Re-derives `filtered` and `depths`.
    pub(super) fn set_loaded(&mut self, items: Vec<Item>) {
        self.all = items;
        self.loading = false;
        self.rederive();
    }

    pub(super) fn item_at(&self, index: IndexPath) -> Option<&Item> {
        self.filtered.get(index.row)
    }
}

impl ListDelegate for BoardListDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.filtered.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.last_query = query.to_string();
        self.rederive();
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let item = self.filtered.get(index.row)?;
        let depth = self.depths.get(index.row).copied().unwrap_or(0);
        let mut title_color = theme::text_primary();
        // Completed tasks take the success tone (see `task_state_color`);
        // everything else stays muted.
        let mut status_color = task_state_color(item);
        if self.selected == Some(index) {
            let surface = theme::surface_selected();
            title_color = theme::readable_on(title_color, surface);
            status_color = theme::readable_on(status_color, surface);
        }
        let drag_value = BoardDragValue {
            item_id: item.id,
            title: item.title.clone(),
        };
        let target_id = item.id;
        // Drop indicator: show a line above or below this row when a drag is
        // active and the cursor is hovering over this row. Gated by
        // `has_active_drag` so the indicator vanishes the instant the drag
        // ends (drop or cancel), even though `drop_indicator` may still hold
        // a stale value until the next interaction.
        let is_drag_active = cx.has_active_drag();
        let show_above = is_drag_active && self.drop_indicator == Some((item.id, DropHalf::Above));
        let show_below = is_drag_active && self.drop_indicator == Some((item.id, DropHalf::Below));
        let view_for_move = self.view.clone();
        Some(
            ListItem::new(index).child(
                h_flex()
                    .id(("board-item", item.id))
                    .relative()
                    .items_center()
                    .pl(px(depth as f32 * 16.0))
                    .gap_2()
                    .py_0p5()
                    .on_drag(
                        drag_value,
                        |drag: &BoardDragValue, _pos, _window, cx: &mut App| {
                            cx.new(|_| drag.clone())
                        },
                    )
                    .on_drag_move(
                        move |event: &DragMoveEvent<BoardDragValue>, _window, cx: &mut App| {
                            let Some(view) = view_for_move.as_ref().and_then(|w| w.upgrade())
                            else {
                                return;
                            };
                            // `on_drag_move` fires for every row that registered a
                            // handler on each mouse move (gpui dispatches it in the
                            // capture phase with no hit-test, unlike `on_drop`), so
                            // each row must guard on cursor containment itself: only
                            // the row under the cursor sets the shared
                            // `drop_indicator`. Without this, every row overwrites
                            // the slot and the last row to handle wins, drawing
                            // the line on the wrong row.
                            let Some(half) =
                                drop_half_for_row(&event.event.position, &event.bounds)
                            else {
                                return;
                            };
                            let dragged_id = event.drag(cx).item_id;
                            view.update(cx, |view, cx| {
                                view.list.update(cx, |list, cx| {
                                    let delegate = list.delegate_mut();
                                    // Suppress the indicator at no-op positions
                                    // (own row, or the near half of an adjacent row)
                                    // so the invariant holds: a position that shows
                                    // an indicator is always one where a drop will
                                    // execute a move. Clearing -- rather than
                                    // leaving the stale value -- also makes the
                                    // indicator vanish as soon as the cursor moves
                                    // onto a no-op position.
                                    let new_indicator = if drop_position_from_half(
                                        dragged_id,
                                        &delegate.filtered,
                                        target_id,
                                        half,
                                    )
                                    .is_some()
                                    {
                                        Some((target_id, half))
                                    } else {
                                        None
                                    };
                                    let changed = delegate.drop_indicator != new_indicator;
                                    delegate.drop_indicator = new_indicator;
                                    if changed {
                                        cx.notify();
                                    }
                                });
                            });
                        },
                    )
                    .when(show_above, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top(px(-1.0))
                                .left_0()
                                .w_full()
                                .h(px(2.0))
                                .bg(theme::accent()),
                        )
                    })
                    .when(show_below, |this| {
                        this.child(
                            div()
                                .absolute()
                                .bottom(px(-1.0))
                                .left_0()
                                .w_full()
                                .h(px(2.0))
                                .bg(theme::accent()),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(title_color)
                            .flex_1()
                            .min_w_0()
                            .child(format!("#{} {}", item.id, item.title)),
                    )
                    .when_some(
                        task_session_state(item, &self.session_states),
                        |row, state| {
                            row.child(state.indicator(item.id, self.selected == Some(index)))
                        },
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(status_color)
                            .child(task_state(item)),
                    )
                    .child(div().text_color(theme::accent()).child(
                        if self.unread.contains(&item.id) {
                            "●"
                        } else {
                            ""
                        },
                    )),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = index;
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let msg = if self.loading {
            "Loading board…"
        } else {
            "No board items"
        };
        h_flex()
            .size_full()
            .justify_center()
            .text_color(theme::readable_on(
                theme::text_muted(),
                rgb(theme::background()).into(),
            ))
            .child(msg)
    }
}
