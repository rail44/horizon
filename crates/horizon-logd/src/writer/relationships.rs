//! Sibling ordering and relationship validation for a locked board snapshot.

use std::collections::HashMap;

use horizon_board::wire::LogError;
use horizon_board::{rank_between, sorted_by_rank, Item, Position};

use super::invalid;

pub(super) fn compute_rank(
    items: &HashMap<u64, Item>,
    parent: Option<u64>,
    position: &Position,
) -> Result<String, LogError> {
    let siblings: HashMap<_, _> = items
        .iter()
        .filter(|(_, i)| i.parent == parent)
        .map(|(id, i)| (*id, i.clone()))
        .collect();
    let sorted = sorted_by_rank(&siblings);
    let (lo, hi) = match position {
        Position::Top => (None, sorted.first().map(|i| i.rank.as_str())),
        Position::Bottom => (sorted.last().map(|i| i.rank.as_str()), None),
        Position::After(id) | Position::Before(id) => {
            let index = sorted
                .iter()
                .position(|i| i.id == *id)
                .ok_or_else(|| invalid("Reorder target must be another sibling"))?;
            if matches!(position, Position::After(_)) {
                (
                    Some(sorted[index].rank.as_str()),
                    sorted.get(index + 1).map(|i| i.rank.as_str()),
                )
            } else {
                (
                    index.checked_sub(1).map(|n| sorted[n].rank.as_str()),
                    Some(sorted[index].rank.as_str()),
                )
            }
        }
    };
    rank_between(lo, hi).ok_or(LogError::RankExhausted)
}

pub(super) fn validate_dependencies(
    items: &HashMap<u64, Item>,
    id: u64,
    dependencies: &[u64],
) -> Result<(), LogError> {
    let mut unique = std::collections::HashSet::new();
    for dependency in dependencies {
        if !unique.insert(dependency) {
            return Err(invalid("Duplicate dependency"));
        }
        let mut pending = vec![*dependency];
        let mut seen = std::collections::HashSet::new();
        while let Some(next) = pending.pop() {
            if next == id {
                return Err(invalid("Dependency cycle"));
            }
            if seen.insert(next) {
                let item = items.get(&next).ok_or(LogError::ItemNotFound(next))?;
                pending.extend(&item.depends_on);
            }
        }
    }
    Ok(())
}

pub(super) fn set_parent(
    items: &HashMap<u64, Item>,
    item: &mut Item,
    parent: Option<u64>,
    position: &Position,
) -> Result<(), LogError> {
    let mut ancestor = parent;
    let mut seen = std::collections::HashSet::new();
    while let Some(next) = ancestor {
        if next == item.id || !seen.insert(next) {
            return Err(invalid("Parent cycle"));
        }
        ancestor = items.get(&next).ok_or(LogError::ItemNotFound(next))?.parent;
    }
    item.rank = compute_rank(items, parent, position)?;
    item.parent = parent;
    Ok(())
}
