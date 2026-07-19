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

use std::collections::{HashMap, HashSet};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, Icon, IconName, IndexPath};
use horizon_workspace::types::SessionSummary;
use horizon_workspace::SessionId;

use crate::theme;

/// Indentation per lineage depth level -- enough to read clearly as
/// nesting without a custom tree widget (list-with-indentation, per the
/// design doc's decision 4b).
const LINEAGE_INDENT_PX: f32 = 16.0;

/// One rendered row: a session summary plus its resolved position in the
/// derivation forest. `depth` (0 for a root) drives indentation;
/// `has_children` gates the subtree-terminate action (design decision 5:
/// only offered on a row that actually has descendants).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionRow {
    pub(crate) summary: SessionSummary,
    pub(crate) depth: usize,
    pub(crate) has_children: bool,
}

/// Orders `sessions` as the derivation forest `docs/session-relationship-
/// design.md` decision 4b asks for: each root (a session with no parent,
/// or whose parent isn't present in `sessions` -- e.g. filtered out by an
/// active search query) followed immediately by its descendants,
/// depth-first, one indentation level per generation. Roots keep their
/// relative order from `sessions` (today's flat order); so does each
/// parent's own list of children. Pure and unit-tested without a
/// `Workspace`/GPUI window.
pub(crate) fn order_as_lineage_tree(sessions: &[SessionSummary]) -> Vec<SessionRow> {
    let present: HashSet<SessionId> = sessions.iter().map(|session| session.id).collect();
    let mut children: HashMap<SessionId, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        match session.parent_session_id {
            Some(parent) if present.contains(&parent) => {
                children.entry(parent).or_default().push(index);
            }
            _ => roots.push(index),
        }
    }

    let mut rows = Vec::with_capacity(sessions.len());
    let mut visited: HashSet<usize> = HashSet::new();
    // LIFO stack, so children/roots are pushed in reverse so popping
    // restores their original relative order.
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|index| (index, 0)).collect();
    while let Some((index, depth)) = stack.pop() {
        if !visited.insert(index) {
            continue;
        }
        let summary = sessions[index].clone();
        let has_children = children
            .get(&summary.id)
            .is_some_and(|kids| !kids.is_empty());
        if let Some(kids) = children.get(&summary.id) {
            stack.extend(kids.iter().rev().map(|&child| (child, depth + 1)));
        }
        rows.push(SessionRow {
            summary,
            depth,
            has_children,
        });
    }
    // Defensive: a malformed cycle (e.g. two sessions each naming the
    // other as parent) would leave both unreachable from any real root --
    // append anything the traversal above never reached as its own root,
    // rather than silently dropping it from the list.
    for (index, session) in sessions.iter().enumerate() {
        if !visited.contains(&index) {
            rows.push(SessionRow {
                summary: session.clone(),
                depth: 0,
                has_children: false,
            });
        }
    }
    rows
}

/// Every session in the subtree rooted at `root` (that session plus every
/// transitive descendant) -- design decision 5's subtree-terminate: "clean
/// a whole lineage branch at once." Guards against a malformed parent
/// cycle the same way [`order_as_lineage_tree`] does, so corrupt data can
/// never spin this into an infinite loop.
pub(crate) fn subtree_session_ids(sessions: &[SessionSummary], root: SessionId) -> Vec<SessionId> {
    let mut children: HashMap<SessionId, Vec<SessionId>> = HashMap::new();
    for session in sessions {
        if let Some(parent) = session.parent_session_id {
            children.entry(parent).or_default().push(session.id);
        }
    }
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        result.push(id);
        if let Some(kids) = children.get(&id) {
            stack.extend(kids.iter().copied());
        }
    }
    result
}

/// Abbreviates `root` for a session-manager row's secondary-info text:
/// relative to `repo_root` when `root` sits strictly inside it (e.g.
/// `.horizon/worktrees/calm-otter` -- an isolated child's own worktree thus
/// reads as visibly distinct from a shared session's directory at a
/// glance), else `~`-relative when `root` sits under `home`, else the full
/// path (rare -- neither is normally unset, but still a valid fallback
/// rather than an empty label). `repo_root`/`home` are resolved by the
/// caller (`render_item`) rather than discovered here, so this stays pure
/// and unit-testable without touching the filesystem or `$HOME`. A `root`
/// equal to `repo_root` itself (a shared session spawned at the repo root,
/// not a subdirectory) falls through to the `home` branch instead of
/// rendering an empty relative path.
pub(crate) fn abbreviate_workspace_root(
    root: &std::path::Path,
    repo_root: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> String {
    if let Some(repo_root) = repo_root {
        if let Ok(relative) = root.strip_prefix(repo_root) {
            if !relative.as_os_str().is_empty() {
                return relative.display().to_string();
            }
        }
    }
    if let Some(home) = home {
        if let Ok(relative) = root.strip_prefix(home) {
            return if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", relative.display())
            };
        }
    }
    root.display().to_string()
}

/// Walks `root`'s ancestors for the nearest **main** repository checkout --
/// skipping a linked worktree's own `.git` (a plain *file*, not a
/// directory, pointing back at the main repo's `.git/worktrees/<name>`), so
/// an isolated session's own worktree directory (itself a valid, if linked,
/// repo) still resolves to the *enclosing* project root -- matching
/// `abbreviate_workspace_root`'s `.horizon/worktrees/<slug>` example rather
/// than collapsing to an empty relative path against itself. No `git`
/// subprocess, just cheap `stat` calls bounded by directory depth -- cheap
/// enough to run on every visible row's render pass.
fn enclosing_repo_root(root: &std::path::Path) -> Option<std::path::PathBuf> {
    root.ancestors()
        .find(|ancestor| ancestor.join(".git").is_dir())
        .map(std::path::Path::to_path_buf)
}

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
                    .when_some(summary.workspace_root.as_deref(), |this, root| {
                        let repo_root = enclosing_repo_root(root);
                        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                        this.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_muted())
                                .child(abbreviate_workspace_root(
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use horizon_workspace::types::SessionKind;
    use horizon_workspace::SessionId;

    use std::path::Path;

    use super::{
        abbreviate_workspace_root, order_as_lineage_tree, subtree_session_ids, SessionSummary,
    };

    fn summary(
        id: SessionId,
        title: &str,
        parent: Option<SessionId>,
        workspace_root: Option<&str>,
    ) -> SessionSummary {
        SessionSummary {
            id,
            kind: SessionKind::Agent,
            display_number: 1,
            title: title.to_string(),
            attached: false,
            workspace_root: workspace_root.map(std::path::PathBuf::from),
            parent_session_id: parent,
        }
    }

    #[test]
    fn unrelated_sessions_are_all_roots_at_depth_zero() {
        let a = SessionId::new();
        let b = SessionId::new();
        let sessions = vec![
            summary(a, "Agent #1", None, None),
            summary(b, "Terminal #1", None, None),
        ];

        let rows = order_as_lineage_tree(&sessions);

        assert_eq!(
            rows.iter().map(|row| row.summary.id).collect::<Vec<_>>(),
            [a, b]
        );
        assert!(rows.iter().all(|row| row.depth == 0));
        assert!(rows.iter().all(|row| !row.has_children));
    }

    #[test]
    fn a_child_is_indented_directly_under_its_parent() {
        let parent = SessionId::new();
        let child = SessionId::new();
        let sessions = vec![
            summary(parent, "Agent #1", None, None),
            summary(child, "Agent #2", Some(parent), None),
        ];

        let rows = order_as_lineage_tree(&sessions);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].summary.id, parent);
        assert_eq!(rows[0].depth, 0);
        assert!(rows[0].has_children);
        assert_eq!(rows[1].summary.id, child);
        assert_eq!(rows[1].depth, 1);
        assert!(!rows[1].has_children);
    }

    #[test]
    fn a_childs_own_children_appear_before_the_next_root_depth_first() {
        let root_a = SessionId::new();
        let child_a = SessionId::new();
        let grandchild_a = SessionId::new();
        let root_b = SessionId::new();
        let sessions = vec![
            summary(root_a, "root a", None, None),
            summary(root_b, "root b", None, None),
            summary(child_a, "child a", Some(root_a), None),
            summary(grandchild_a, "grandchild a", Some(child_a), None),
        ];

        let rows = order_as_lineage_tree(&sessions);

        let order: Vec<_> = rows.iter().map(|row| row.summary.id).collect();
        assert_eq!(order, [root_a, child_a, grandchild_a, root_b]);
        assert_eq!(
            rows.iter().map(|row| row.depth).collect::<Vec<_>>(),
            [0, 1, 2, 0]
        );
    }

    #[test]
    fn a_parent_missing_from_the_list_demotes_its_child_to_a_root() {
        // e.g. the parent was filtered out by an active search query --
        // "unrelated sessions as today" (decision 4b).
        let known_parent = SessionId::new();
        let unknown_parent = SessionId::new();
        let orphan = SessionId::new();
        let sessions = vec![summary(orphan, "orphan", Some(unknown_parent), None)];
        let _ = known_parent;

        let rows = order_as_lineage_tree(&sessions);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].summary.id, orphan);
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn a_malformed_parent_cycle_never_drops_a_session() {
        let a = SessionId::new();
        let b = SessionId::new();
        let sessions = vec![
            summary(a, "a", Some(b), None),
            summary(b, "b", Some(a), None),
        ];

        let rows = order_as_lineage_tree(&sessions);

        let ids: HashSet<_> = rows.iter().map(|row| row.summary.id).collect();
        assert_eq!(ids, [a, b].into_iter().collect());
    }

    #[test]
    fn subtree_session_ids_collects_the_root_and_every_descendant() {
        let root = SessionId::new();
        let child = SessionId::new();
        let grandchild = SessionId::new();
        let unrelated = SessionId::new();
        let sessions = vec![
            summary(root, "root", None, None),
            summary(child, "child", Some(root), None),
            summary(grandchild, "grandchild", Some(child), None),
            summary(unrelated, "unrelated", None, None),
        ];

        let ids: HashSet<_> = subtree_session_ids(&sessions, root).into_iter().collect();

        assert_eq!(ids, [root, child, grandchild].into_iter().collect());
    }

    #[test]
    fn subtree_session_ids_is_just_the_root_when_it_has_no_children() {
        let root = SessionId::new();
        let unrelated = SessionId::new();
        let sessions = vec![
            summary(root, "root", None, None),
            summary(unrelated, "unrelated", None, None),
        ];

        assert_eq!(subtree_session_ids(&sessions, root), vec![root]);
    }

    #[test]
    fn subtree_session_ids_never_loops_on_a_malformed_parent_cycle() {
        let a = SessionId::new();
        let b = SessionId::new();
        let sessions = vec![
            summary(a, "a", Some(b), None),
            summary(b, "b", Some(a), None),
        ];

        let ids: HashSet<_> = subtree_session_ids(&sessions, a).into_iter().collect();

        assert_eq!(ids, [a, b].into_iter().collect());
    }

    #[test]
    fn a_root_strictly_inside_the_repo_renders_relative_to_it() {
        assert_eq!(
            abbreviate_workspace_root(
                Path::new("/home/user/project/.horizon/worktrees/calm-otter"),
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
            ),
            ".horizon/worktrees/calm-otter"
        );
    }

    #[test]
    fn a_root_equal_to_the_repo_root_falls_through_to_home_abbreviation() {
        // A shared (non-isolated) session's workspace_root is often the
        // repo root itself, not a subdirectory -- rendering that relative
        // to itself would be an uninformative empty string, so it falls
        // through to the `home` branch instead.
        assert_eq!(
            abbreviate_workspace_root(
                Path::new("/home/user/project"),
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
            ),
            "~/project"
        );
    }

    #[test]
    fn a_root_outside_any_repo_renders_home_relative() {
        assert_eq!(
            abbreviate_workspace_root(
                Path::new("/home/user/scratch/notes"),
                None,
                Some(Path::new("/home/user")),
            ),
            "~/scratch/notes"
        );
    }

    #[test]
    fn a_root_equal_to_home_itself_renders_the_bare_tilde() {
        assert_eq!(
            abbreviate_workspace_root(Path::new("/home/user"), None, Some(Path::new("/home/user"))),
            "~"
        );
    }

    #[test]
    fn a_root_under_neither_repo_nor_home_falls_back_to_the_full_path() {
        assert_eq!(
            abbreviate_workspace_root(
                Path::new("/mnt/data/project"),
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
            ),
            "/mnt/data/project"
        );
    }

    #[test]
    fn a_repo_root_that_doesnt_contain_the_workspace_root_is_ignored() {
        // `repo_root` came from walking a *different* path's ancestors --
        // defensive coverage for a mismatched pair, even though
        // `render_item` always derives both from the same `root`.
        assert_eq!(
            abbreviate_workspace_root(
                Path::new("/home/user/other-project/src"),
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
            ),
            "~/other-project/src"
        );
    }
}
