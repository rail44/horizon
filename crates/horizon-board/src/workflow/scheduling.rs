use super::{ChangeScope, Work};
use crate::{is_closed_status, Item};
use std::collections::{HashMap, HashSet};

pub fn scopes_conflict(a: &ChangeScope, b: &ChangeScope) -> bool {
    a.paths.iter().any(|x| {
        b.paths.iter().any(|y| {
            let x = x.trim_end_matches('/');
            let y = y.trim_end_matches('/');
            x == "*"
                || y == "*"
                || x == y
                || x.starts_with(&format!("{y}/"))
                || y.starts_with(&format!("{x}/"))
        })
    }) || a.functions.iter().any(|x| b.functions.contains(x))
}

/// Stable AI ranks constrained by explicit owner precedence.
pub fn ordered_items(items: &HashMap<u64, Item>) -> Vec<&Item> {
    let mut remaining: Vec<_> = items.values().collect();
    remaining.sort_by(|a, b| {
        let milestone = |item: &Item| {
            item.parent
                .and_then(|id| items.get(&id))
                .map_or_else(|| item.rank.clone(), |p| p.rank.clone())
        };
        milestone(a)
            .cmp(&milestone(b))
            .then(a.rank.cmp(&b.rank))
            .then(a.id.cmp(&b.id))
    });
    let mut result = Vec::new();
    while !remaining.is_empty() {
        let index = remaining
            .iter()
            .position(|candidate| {
                !remaining.iter().any(|other| {
                    other.id != candidate.id
                        && other
                            .workflow
                            .as_ref()
                            .is_some_and(|w| w.before.contains(&candidate.id))
                })
            })
            .unwrap_or(0);
        result.push(remaining.remove(index));
    }
    result
}

pub fn eligible_work(items: &HashMap<u64, Item>, id: u64) -> Option<Work> {
    let item = items.get(&id)?;
    let flow = item.workflow.as_ref()?;
    if is_closed_status(&item.status)
        || flow.paused
        || flow.active.is_some()
        || flow.problem.is_some()
    {
        return None;
    }
    if let Some(task) = &flow.task {
        let parent_item = items.get(&item.parent?)?;
        if is_closed_status(&parent_item.status) {
            return None;
        }
        let parent = parent_item.workflow.as_ref()?;
        if parent.paused || parent.achieved || parent.goal_revision != task.goal_revision {
            return None;
        }
        if task_waits_for_decision(items, id) {
            return None;
        }
        if !item.depends_on.iter().all(|dep| {
            items.get(dep).is_some_and(|d| {
                if let Some(w) = &d.workflow {
                    w.integrated.is_some() || w.achieved
                } else {
                    d.status == "done"
                }
            })
        }) {
            return None;
        }
        if flow.integrated.is_some() {
            return None;
        }
        if flow.result.is_some() {
            return flow.verification.is_none().then_some(Work::Verify);
        }
        if items.values().any(|other| {
            other.id != id
                && other.workflow.as_ref().is_some_and(|w| {
                    w.integrated.is_none()
                        && w.problem.is_none()
                        && (w.active.is_some() || w.result.is_some())
                        && w.task
                            .as_ref()
                            .is_some_and(|t| scopes_conflict(&task.scope, &t.scope))
                })
        }) {
            return None;
        }
        return Some(Work::Task {
            key: task.key.clone(),
        });
    }
    if let Some(d) = flow
        .unanswered()
        .into_iter()
        .find(|d| d.messages.last().is_some_and(|m| m.owner))
    {
        return Some(Work::Discuss {
            key: d.key.clone(),
            turn: d.messages.len(),
        });
    }
    if flow.plan_requested {
        return Some(Work::Plan);
    }
    let plan = flow.plan.as_ref()?;
    if flow.verification.is_none()
        && flow.unanswered().is_empty()
        && !plan.tasks.is_empty()
        && plan.tasks.iter().all(|id| {
            items.get(id).is_some_and(|i| {
                i.status == "archived"
                    || i.workflow.as_ref().is_some_and(|w| w.integrated.is_some())
            })
        })
    {
        return Some(Work::Verify);
    }
    None
}

/// A reopened decision can affect an already integrated prerequisite too.
pub fn task_waits_for_decision(items: &HashMap<u64, Item>, id: u64) -> bool {
    let mut pending = vec![id];
    let mut dependencies = HashSet::new();
    while let Some(id) = pending.pop() {
        if dependencies.insert(id) {
            if let Some(item) = items.get(&id) {
                pending.extend(item.depends_on.iter().copied());
            }
        }
    }
    items.values().filter_map(|i| i.workflow.as_ref()).any(|w| {
        w.unanswered()
            .iter()
            .any(|d| d.affected_tasks.iter().any(|id| dependencies.contains(id)))
    })
}

pub(super) fn validate_dependencies(items: &HashMap<u64, Item>) -> Result<(), String> {
    fn visit(
        id: u64,
        items: &HashMap<u64, Item>,
        active: &mut HashSet<u64>,
        done: &mut HashSet<u64>,
    ) -> bool {
        if done.contains(&id) {
            return true;
        }
        if !active.insert(id) {
            return false;
        }
        let valid = items.get(&id).is_some_and(|i| {
            i.depends_on
                .iter()
                .all(|dep| visit(*dep, items, active, done))
        });
        active.remove(&id);
        done.insert(id);
        valid
    }
    let mut done = HashSet::new();
    for item in items
        .values()
        .filter(|i| i.workflow.as_ref().is_some_and(|w| w.task.is_some()))
    {
        if !visit(item.id, items, &mut HashSet::new(), &mut done) {
            return Err("Unknown or cyclic task dependency".into());
        }
    }
    Ok(())
}
