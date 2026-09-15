//! Ordinary recursive tasks and stable consultation messages.
use crate::event::{BoardEvent, Envelope};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub text: String,
    pub at: Option<u64>,
    pub source: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub status: String,
    pub completed: bool,
    pub rank: String,
    pub parent: Option<u64>,
    pub depends_on: Vec<u64>,
    pub comments: Vec<Comment>,
    pub session_id: Option<String>,
    pub review_session_id: Option<String>,
}
pub fn fold(envelopes: &[Envelope]) -> HashMap<u64, Item> {
    let mut items = HashMap::new();
    for env in envelopes {
        match &env.event {
            BoardEvent::ItemStored { item, .. } | BoardEvent::ImportedItem { item, .. } => {
                items.insert(item.id, item.clone());
            }
            BoardEvent::MessageAdded { id, message } => {
                if let Some(item) = items.get_mut(id) {
                    item.comments.push(message.clone());
                }
            }
            _ => {}
        }
    }
    items
}
pub fn sorted_by_rank(items: &HashMap<u64, Item>) -> Vec<&Item> {
    let mut result: Vec<_> = items.values().collect();
    result.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.id.cmp(&b.id)));
    result
}
/// Returns items in parent→child tree order for display: top-level items
/// by `rank`, each immediately followed by its children (by `rank` among
/// siblings) at `depth + 1`, recursing. An item whose `parent` is `None` or
/// whose parent id is not in `items` (orphan — parent missing, closed, or
/// filtered out of the current view) is a top-level root.
///
/// Invalid imported hierarchy cycles are handled defensively: items whose parent chain forms a cycle never
/// appear as roots, so after the initial DFS any unvisited items are emitted
/// as additional top-level roots. The `visited` set prevents infinite
/// recursion.
///
/// When `top_level_only` is true, returns only the roots at depth 0 — the
/// top-level view; filtered-out parents do not promote their children.
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
        return roots
            .into_iter()
            .filter(|r| r.parent.is_none())
            .map(|r| (r, 0))
            .collect();
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
    #[test]
    fn filtered_children_are_not_top_level_tasks() {
        let items = vec![Item {
            id: 2,
            parent: Some(1),
            rank: "n".into(),
            ..Item::default()
        }];
        assert!(tree_order(&items, true).is_empty());
        assert_eq!(tree_order(&items, false).len(), 1);
    }
    #[test]
    fn hierarchy_uses_parent_order_then_child_order() {
        let items = vec![
            Item {
                id: 1,
                rank: "n".into(),
                ..Item::default()
            },
            Item {
                id: 2,
                rank: "a".into(),
                parent: Some(1),
                ..Item::default()
            },
            Item {
                id: 3,
                rank: "b".into(),
                ..Item::default()
            },
        ];
        assert_eq!(
            tree_order(&items, false)
                .iter()
                .map(|(i, d)| (i.id, *d))
                .collect::<Vec<_>>(),
            vec![(3, 0), (1, 0), (2, 1)]
        );
    }
}
