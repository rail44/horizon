//! The session manager modal, on the same gpui-component searchable
//! `List` pattern as the palette: attach or jump to a session on
//! confirm, terminate on secondary confirm. The shell owns the events;
//! this delegate only filters and renders summaries.
//!
//! Rows render `docs/session-relationship-design.md` decision 4b's
//! derivation tree rather than a flat list: [`order_as_lineage_tree`]
//! turns the flat `SessionSummary` list (linked only by `parent_session_id`)
//! into a forest -- roots first, each followed immediately by its
//! descendants, one indentation level per generation. Two row-scoped
//! keybindings ride the same `SessionManager` key context the shell wires
//! up (`src/workspace/render.rs`/`src/workspace/bindings.rs`): `secondary-o`
//! opens the selected row's directory (decision 4a, generalized off the
//! active-session-only v1) and `secondary-shift-t` terminates the selected
//! row's whole subtree (decision 5's explicit opt-in) -- both alongside the
//! existing primary confirm (attach/jump) and secondary confirm (plain
//! per-session terminate).

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, Icon, IconName, IndexPath};
use horizon_workspace::lineage::{
    abbreviate_directory, enclosing_repo_root, order_as_lineage_tree, SessionRow,
};
use horizon_workspace::types::SessionSummary;

use crate::theme;

/// Indentation per lineage depth level -- enough to read clearly as
/// nesting without a custom tree widget (list-with-indentation, per the
/// design doc's decision 4b).
const LINEAGE_INDENT_PX: f32 = 16.0;

pub(crate) struct SessionManagerDelegate {
    all: Vec<SessionSummary>,
    filtered: Vec<SessionRow>,
    // Whether the most recent confirm was the secondary one (cmd-enter /
    // right click) — the List calls `confirm` before emitting
    // `ListEvent::Confirm`, so the shell's event handler reads this to
    // pick attach-or-jump (primary) vs terminate (secondary).
    last_confirm_secondary: bool,
    // The currently-selected row, mirrored from `set_selected_index` --
    // see `PaletteDelegate`'s own field doc (`src/palette.rs`) for why
    // this is the delegate's own responsibility to track.
    selected: Option<IndexPath>,
}

impl SessionManagerDelegate {
    pub(crate) fn new(sessions: Vec<SessionSummary>) -> Self {
        let filtered = order_as_lineage_tree(&sessions);
        Self {
            all: sessions,
            filtered,
            last_confirm_secondary: false,
            selected: None,
        }
    }

    pub(crate) fn summary_at(&self, index: IndexPath) -> Option<&SessionSummary> {
        self.filtered.get(index.row).map(|row| &row.summary)
    }

    /// The full row (summary plus lineage position) at `index` -- the
    /// per-row directory/subtree-terminate actions (`src/workspace/
    /// modals.rs`) need `has_children` alongside the summary itself to
    /// gate subtree-terminate's enablement.
    pub(crate) fn row_at(&self, index: IndexPath) -> Option<&SessionRow> {
        self.filtered.get(index.row)
    }

    pub(crate) fn last_confirm_secondary(&self) -> bool {
        self.last_confirm_secondary
    }

    /// Replaces the listed sessions (after a terminate, keeping the
    /// modal open on fresh data).
    pub(crate) fn reset(&mut self, sessions: Vec<SessionSummary>) {
        self.filtered = order_as_lineage_tree(&sessions);
        self.all = sessions;
    }
}

impl ListDelegate for SessionManagerDelegate {
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
        let query = query.trim().to_ascii_lowercase();
        let matching: Vec<SessionSummary> = self
            .all
            .iter()
            .filter(|summary| {
                query.is_empty() || summary.title.to_ascii_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        self.filtered = order_as_lineage_tree(&matching);
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let row = self.filtered.get(index.row)?;
        let summary = &row.summary;
        let (status, mut status_color) = if summary.attached {
            ("attached", theme::success())
        } else {
            ("detached", theme::text_muted())
        };
        let mut title_color = theme::text_primary();
        // Floor both text colors against the selected-row surface rather
        // than plain `background` -- item 2 of the 2026-07-15 contrast
        // audit; see `PaletteDelegate::render_item`'s own comment.
        if self.selected == Some(index) {
            let surface = theme::surface_selected();
            title_color = theme::readable_on(title_color, surface);
            status_color = theme::readable_on(status_color, surface);
        }
        let title = if row.depth > 0 {
            format!("↳ {}", summary.title)
        } else {
            summary.title.clone()
        };
        Some(
            ListItem::new(index).child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py_0p5()
                    .pl(px(row.depth as f32 * LINEAGE_INDENT_PX))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(title_color)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(status_color)
                            .child(status),
                    )
                    .when_some(summary.directory.as_deref(), |this, root| {
                        let repo_root = enclosing_repo_root(root);
                        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                        this.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_muted())
                                .child(abbreviate_directory(
                                    root,
                                    repo_root.as_deref(),
                                    home.as_deref(),
                                )),
                        )
                    })
                    .when(row.has_children, |this| {
                        this.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_muted())
                                .child("subtree"),
                        )
                    }),
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
        h_flex()
            .size_full()
            .justify_center()
            .text_color(theme::readable_on(
                theme::text_muted(),
                rgb(theme::background()).into(),
            ))
            .child(Icon::new(IconName::Inbox).size_12())
    }

    fn confirm(
        &mut self,
        secondary: bool,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.last_confirm_secondary = secondary;
    }
}
