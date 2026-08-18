//! The folded item model and the event→state fold.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{BoardEvent, Envelope};

/// A comment attached to an item. `author` is a free-form string
/// (convention: `owner` / `session:<uuid>` / future prefix-tagged forms).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    pub author: String,
    pub text: String,
    pub at: u64,
}

/// The full state of one work item after folding all events.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    pub id: u64,
    pub title: String,
    pub body: String,
    /// Free-form slug string (recommended vocabulary: proposed / ready /
    /// in-progress / review / done / blocked / archived). Empty until first
    /// set. `done` and `archived` are closed statuses — hidden from the
    /// default `list` view by `is_closed_status`.
    pub status: String,
    /// Lexicographic rank string (lowercase a-z).
    pub rank: String,
    /// Free-form assignee. Empty = unassigned.
    pub assignee: String,
    /// Optional parent item id.
    pub parent: Option<u64>,
    /// Items this one depends on.
    pub depends_on: Vec<u64>,
    /// Free-form links (session ids, branch names, doc paths, …).
    pub links: Vec<String>,
    /// Comments in chronological order.
    pub comments: Vec<Comment>,
}

/// Folds a chronologically-ordered slice of envelopes into a map of
/// item id → `Item`. Events referencing unknown items (e.g. an update
/// for an id whose `item-created` was in a corrupt/skipped line) are
/// silently dropped — the fold is a best-effort projection.
pub fn fold(envelopes: &[Envelope]) -> HashMap<u64, Item> {
    let mut items: HashMap<u64, Item> = HashMap::new();
    for env in envelopes {
        match &env.event {
            BoardEvent::ItemCreated {
                id,
                title,
                body,
                rank,
            } => {
                items.insert(
                    *id,
                    Item {
                        id: *id,
                        title: title.clone(),
                        body: body.clone(),
                        rank: rank.clone(),
                        ..Item::default()
                    },
                );
            }
            BoardEvent::ItemUpdated {
                id,
                status,
                rank,
                assignee,
                parent,
                depends_on,
                links,
                title,
                body,
            } => {
                if let Some(item) = items.get_mut(id) {
                    if let Some(v) = status {
                        item.status = v.clone();
                    }
                    if let Some(v) = rank {
                        item.rank = v.clone();
                    }
                    if let Some(v) = assignee {
                        item.assignee = v.clone();
                    }
                    if let Some(v) = parent {
                        item.parent = *v;
                    }
                    if let Some(v) = depends_on {
                        item.depends_on = v.clone();
                    }
                    if let Some(v) = links {
                        item.links = v.clone();
                    }
                    if let Some(v) = title {
                        item.title = v.clone();
                    }
                    if let Some(v) = body {
                        item.body = v.clone();
                    }
                }
            }
            BoardEvent::CommentAdded { id, author, text } => {
                if let Some(item) = items.get_mut(id) {
                    item.comments.push(Comment {
                        author: author.clone(),
                        text: text.clone(),
                        at: env.at,
                    });
                }
            }
        }
    }
    items
}

/// Whether an item's status counts as "closed" — hidden from the default
/// list view. `done` and `archived` are closed; everything else (including the
/// empty/unset status) is open. Centralised so the closed-set is defined in
/// one testable place rather than scattered as string literals across the
/// CLI, UI, and daemon.
pub fn is_closed_status(status: &str) -> bool {
    matches!(status, "done" | "archived")
}

/// Returns items sorted by rank (lexicographic).
pub fn sorted_by_rank(items: &HashMap<u64, Item>) -> Vec<&Item> {
    let mut v: Vec<&Item> = items.values().collect();
    v.sort_by(|a, b| a.rank.cmp(&b.rank));
    v
}

/// Returns items in parent→child tree order for display: top-level items
/// by `rank`, each immediately followed by its children (by `rank` among
/// siblings) at `depth + 1`, recursing. An item whose `parent` is `None` or
/// whose parent id is not in `items` (orphan — parent missing, closed, or
/// filtered out of the current view) is a top-level root.
///
/// Cycles (possible because `add --parent` does not validate referential
/// integrity) are broken: items whose parent chain forms a cycle never
/// appear as roots, so after the initial DFS any unvisited items are emitted
/// as additional top-level roots. The `visited` set prevents infinite
/// recursion.
///
/// When `top_level_only` is true, returns only the roots at depth 0 — the
/// "roadmap view" that hides decomposed children.
pub fn tree_order(items: &[Item], top_level_only: bool) -> Vec<(&Item, usize)> {
    let id_set: HashSet<u64> = items.iter().map(|i| i.id).collect();

    // Build parent → children map (only for parents that exist in the set;
    // items whose parent is absent are orphans and become roots).
    let mut children: HashMap<u64, Vec<&Item>> = HashMap::new();
    for item in items {
        if let Some(parent) = item.parent {
            if id_set.contains(&parent) {
                children.entry(parent).or_default().push(item);
            }
        }
    }
    for kids in children.values_mut() {
        kids.sort_by(|a, b| a.rank.cmp(&b.rank));
    }

    // Roots: no parent, or parent not in the visible set (orphan).
    let mut roots: Vec<&Item> = items
        .iter()
        .filter(|item| match item.parent {
            None => true,
            Some(p) => !id_set.contains(&p),
        })
        .collect();
    roots.sort_by(|a, b| a.rank.cmp(&b.rank));

    if top_level_only {
        return roots.into_iter().map(|r| (r, 0)).collect();
    }

    let mut result: Vec<(&Item, usize)> = Vec::new();
    let mut visited: HashSet<u64> = HashSet::new();

    // Iterative DFS: push roots in reverse so the first root pops first.
    let mut stack: Vec<(&Item, usize)> = roots.into_iter().rev().map(|r| (r, 0)).collect();
    while let Some((item, depth)) = stack.pop() {
        if !visited.insert(item.id) {
            continue;
        }
        result.push((item, depth));
        if let Some(kids) = children.get(&item.id) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }

    // Cycle members: items whose parent chain forms a cycle, so none were
    // roots and the DFS never reached them. Emit as additional top-level
    // roots (sorted by rank).
    let mut cycle_roots: Vec<&Item> = items
        .iter()
        .filter(|item| !visited.contains(&item.id))
        .collect();
    cycle_roots.sort_by(|a, b| a.rank.cmp(&b.rank));
    stack.extend(cycle_roots.into_iter().rev().map(|r| (r, 0)));
    while let Some((item, depth)) = stack.pop() {
        if !visited.insert(item.id) {
            continue;
        }
        result.push((item, depth));
        if let Some(kids) = children.get(&item.id) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{BoardEvent, Envelope, SCHEMA, VERSION};

    fn env(at: u64, event: BoardEvent) -> Envelope {
        Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event,
        }
    }

    #[test]
    fn fold_create_update_comment_roundtrip() {
        let envelopes = vec![
            env(
                1000,
                BoardEvent::ItemCreated {
                    id: 1,
                    title: "Task".to_string(),
                    body: "Do thing".to_string(),
                    rank: "n".to_string(),
                },
            ),
            env(
                2000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: Some("in-progress".to_string()),
                    rank: None,
                    assignee: Some("owner".to_string()),
                    parent: Some(Some(5)),
                    depends_on: None,
                    links: Some(vec!["branch-x".to_string()]),
                    title: None,
                    body: None,
                },
            ),
            env(
                3000,
                BoardEvent::CommentAdded {
                    id: 1,
                    author: "owner".to_string(),
                    text: "Started".to_string(),
                },
            ),
        ];

        let items = fold(&envelopes);
        let item = &items[&1];
        assert_eq!(item.title, "Task");
        assert_eq!(item.body, "Do thing");
        assert_eq!(item.rank, "n");
        assert_eq!(item.status, "in-progress");
        assert_eq!(item.assignee, "owner");
        assert_eq!(item.parent, Some(5));
        assert_eq!(item.links, vec!["branch-x"]);
        assert_eq!(item.comments.len(), 1);
        assert_eq!(item.comments[0].text, "Started");
        assert_eq!(item.comments[0].at, 3000);
    }

    #[test]
    fn fold_clears_parent_with_some_none() {
        let envelopes = vec![
            env(
                1000,
                BoardEvent::ItemCreated {
                    id: 1,
                    title: "T".to_string(),
                    body: String::new(),
                    rank: "n".to_string(),
                },
            ),
            env(
                2000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: None,
                    rank: None,
                    assignee: None,
                    parent: Some(Some(3)),
                    depends_on: None,
                    links: None,
                    title: None,
                    body: None,
                },
            ),
            env(
                3000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: None,
                    rank: None,
                    assignee: None,
                    parent: Some(None), // clear
                    depends_on: None,
                    links: None,
                    title: None,
                    body: None,
                },
            ),
        ];
        let items = fold(&envelopes);
        assert_eq!(items[&1].parent, None);
    }

    #[test]
    fn fold_unknown_status_and_author_dont_break() {
        let envelopes = vec![
            env(
                1000,
                BoardEvent::ItemCreated {
                    id: 1,
                    title: "T".to_string(),
                    body: String::new(),
                    rank: "n".to_string(),
                },
            ),
            env(
                2000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: Some("weird-custom-status".to_string()),
                    rank: None,
                    assignee: None,
                    parent: None,
                    depends_on: None,
                    links: None,
                    title: None,
                    body: None,
                },
            ),
            env(
                3000,
                BoardEvent::CommentAdded {
                    id: 1,
                    author: "session:abc-123".to_string(),
                    text: "note".to_string(),
                },
            ),
        ];
        let items = fold(&envelopes);
        assert_eq!(items[&1].status, "weird-custom-status");
        assert_eq!(items[&1].comments[0].author, "session:abc-123");
    }

    #[test]
    fn is_closed_status_recognises_done_and_archived() {
        assert!(is_closed_status("done"));
        assert!(is_closed_status("archived"));
        assert!(!is_closed_status(""));
        assert!(!is_closed_status("proposed"));
        assert!(!is_closed_status("in-progress"));
        assert!(!is_closed_status("review"));
        assert!(!is_closed_status("blocked"));
    }

    // -- tree_order: parent→child display ordering -----------------------

    fn tree_item(id: u64, title: &str, rank: &str, parent: Option<u64>) -> Item {
        Item {
            id,
            title: title.to_string(),
            rank: rank.to_string(),
            parent,
            ..Item::default()
        }
    }

    fn tree_ids(ordered: &[(&Item, usize)]) -> Vec<(u64, usize)> {
        ordered
            .iter()
            .map(|(item, depth)| (item.id, *depth))
            .collect()
    }

    #[test]
    fn tree_order_flat_list_matches_rank_order() {
        // No parents → every item is a root, depth 0, rank-sorted.
        let items = vec![
            tree_item(3, "c", "c", None),
            tree_item(1, "a", "a", None),
            tree_item(2, "b", "b", None),
        ];
        assert_eq!(
            tree_ids(&tree_order(&items, false)),
            vec![(1, 0), (2, 0), (3, 0)]
        );
    }

    #[test]
    fn tree_order_groups_children_under_parent() {
        // Parent 1 has children 2 and 3; child ranks are not adjacent to the
        // parent's rank (child 3 has a higher rank than standalone item 4),
        // but tree_order still groups them under the parent.
        let items = vec![
            tree_item(1, "parent", "a", None),
            tree_item(2, "child1", "b", Some(1)),
            tree_item(4, "sibling", "c", None),
            tree_item(3, "child2", "d", Some(1)),
        ];
        assert_eq!(
            tree_ids(&tree_order(&items, false)),
            vec![(1, 0), (2, 1), (3, 1), (4, 0)]
        );
    }

    #[test]
    fn tree_order_sorts_siblings_by_rank() {
        let items = vec![
            tree_item(1, "parent", "a", None),
            tree_item(3, "child2", "c", Some(1)),
            tree_item(2, "child1", "b", Some(1)),
        ];
        assert_eq!(
            tree_ids(&tree_order(&items, false)),
            vec![(1, 0), (2, 1), (3, 1)]
        );
    }

    #[test]
    fn tree_order_recurses_into_grandchildren() {
        let items = vec![
            tree_item(1, "root", "a", None),
            tree_item(2, "child", "b", Some(1)),
            tree_item(3, "grandchild", "c", Some(2)),
        ];
        assert_eq!(
            tree_ids(&tree_order(&items, false)),
            vec![(1, 0), (2, 1), (3, 2)]
        );
    }

    #[test]
    fn tree_order_orphan_treated_as_top_level() {
        // Item 2's parent (99) does not exist in the set → orphan → root.
        let items = vec![
            tree_item(1, "root", "a", None),
            tree_item(2, "orphan", "b", Some(99)),
        ];
        assert_eq!(tree_ids(&tree_order(&items, false)), vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn tree_order_top_level_only_returns_roots() {
        let items = vec![
            tree_item(1, "parent", "a", None),
            tree_item(2, "child", "b", Some(1)),
            tree_item(3, "root2", "c", None),
        ];
        assert_eq!(tree_ids(&tree_order(&items, true)), vec![(1, 0), (3, 0)]);
    }

    #[test]
    fn tree_order_breaks_cycle_by_reclassifying_as_root() {
        // A→B→A: neither is a root initially, but the cycle is broken and
        // both appear (the first by rank becomes a root, the other its child).
        let items = vec![
            tree_item(1, "a", "a", Some(2)),
            tree_item(2, "b", "b", Some(1)),
        ];
        let ordered = tree_order(&items, false);
        // Both items appear exactly once.
        assert_eq!(ordered.len(), 2);
        let ids: Vec<u64> = ordered.iter().map(|(item, _)| item.id).collect();
        assert_eq!(ids, vec![1, 2]);
        // The first (by rank) is at depth 0, the second at depth 1.
        assert_eq!(ordered[0].1, 0);
        assert_eq!(ordered[1].1, 1);
    }

    #[test]
    fn tree_order_self_parent_appears_once_at_root() {
        let items = vec![tree_item(1, "self", "a", Some(1))];
        let ordered = tree_order(&items, false);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].0.id, 1);
        assert_eq!(ordered[0].1, 0);
    }

    #[test]
    fn tree_order_empty_input() {
        assert_eq!(tree_order(&[], false), Vec::<(&Item, usize)>::new());
        assert_eq!(tree_order(&[], true), Vec::<(&Item, usize)>::new());
    }

    #[test]
    fn fold_update_for_unknown_item_is_dropped() {
        let envelopes = vec![env(
            1000,
            BoardEvent::ItemUpdated {
                id: 99,
                status: Some("x".to_string()),
                rank: None,
                assignee: None,
                parent: None,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            },
        )];
        let items = fold(&envelopes);
        assert!(items.is_empty());
    }
}
