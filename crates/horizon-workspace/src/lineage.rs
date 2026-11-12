//! The sessions as the tree of what each was derived from
//! (`docs/session-relationship-design.md`), and how a session's directory
//! is written short: what a session manager lists.

use std::collections::{HashMap, HashSet};

use crate::types::SessionSummary;
use crate::SessionId;

/// One rendered row: a session summary plus its resolved position in the
/// derivation forest. `depth` (0 for a root) drives indentation;
/// `has_children` gates the subtree-terminate action (design decision 5:
/// only offered on a row that actually has descendants).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRow {
    pub summary: SessionSummary,
    pub depth: usize,
    pub has_children: bool,
}

/// Orders `sessions` as the derivation forest `docs/session-relationship-
/// design.md` decision 4b asks for: each root (a session with no parent,
/// or whose parent isn't present in `sessions` -- e.g. filtered out by an
/// active search query) followed immediately by its descendants,
/// depth-first, one indentation level per generation. Roots keep their
/// relative order from `sessions` (today's flat order); so does each
/// parent's own list of children. Pure and unit-tested without a
/// `Workspace`/GPUI window.
pub fn order_as_lineage_tree(sessions: &[SessionSummary]) -> Vec<SessionRow> {
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
pub fn subtree_session_ids(sessions: &[SessionSummary], root: SessionId) -> Vec<SessionId> {
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
pub fn abbreviate_directory(
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
/// `abbreviate_directory`'s `.horizon/worktrees/<slug>` example rather
/// than collapsing to an empty relative path against itself. No `git`
/// subprocess, just cheap `stat` calls bounded by directory depth -- cheap
/// enough to run on every visible row's render pass.
pub fn enclosing_repo_root(root: &std::path::Path) -> Option<std::path::PathBuf> {
    root.ancestors()
        .find(|ancestor| ancestor.join(".git").is_dir())
        .map(std::path::Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::types::SessionKind;
    use crate::SessionId;

    use std::path::Path;

    use super::{abbreviate_directory, order_as_lineage_tree, subtree_session_ids, SessionSummary};

    fn summary(
        id: SessionId,
        title: &str,
        parent: Option<SessionId>,
        directory: Option<&str>,
    ) -> SessionSummary {
        SessionSummary {
            id,
            kind: SessionKind::Agent,
            display_number: 1,
            title: title.to_string(),
            attached: false,
            directory: directory.map(std::path::PathBuf::from),
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
            abbreviate_directory(
                Path::new("/home/user/project/.horizon/worktrees/calm-otter"),
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
            ),
            ".horizon/worktrees/calm-otter"
        );
    }

    #[test]
    fn a_root_equal_to_the_repo_root_falls_through_to_home_abbreviation() {
        // A shared (non-isolated) session's directory is often the
        // repo root itself, not a subdirectory -- rendering that relative
        // to itself would be an uninformative empty string, so it falls
        // through to the `home` branch instead.
        assert_eq!(
            abbreviate_directory(
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
            abbreviate_directory(
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
            abbreviate_directory(Path::new("/home/user"), None, Some(Path::new("/home/user"))),
            "~"
        );
    }

    #[test]
    fn a_root_under_neither_repo_nor_home_falls_back_to_the_full_path() {
        assert_eq!(
            abbreviate_directory(
                Path::new("/mnt/data/project"),
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
            ),
            "/mnt/data/project"
        );
    }

    #[test]
    fn a_repo_root_that_doesnt_contain_the_directory_is_ignored() {
        // `repo_root` came from walking a *different* path's ancestors --
        // defensive coverage for a mismatched pair, even though
        // `render_item` always derives both from the same `root`.
        assert_eq!(
            abbreviate_directory(
                Path::new("/home/user/other-project/src"),
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
            ),
            "~/other-project/src"
        );
    }
}
