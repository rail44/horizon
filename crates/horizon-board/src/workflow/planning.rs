use super::*;
use crate::{rank_between, Item};
use std::collections::{HashMap, HashSet};

fn resolve(
    reference: &str,
    ids: &HashMap<String, u64>,
    items: &HashMap<u64, Item>,
) -> Result<u64, String> {
    ids.get(reference)
        .copied()
        .or_else(|| {
            reference
                .strip_prefix('#')?
                .parse::<u64>()
                .ok()
                .filter(|id| items.contains_key(id))
        })
        .ok_or_else(|| format!("Unknown task reference {reference}"))
}

pub(super) fn decisions(
    previous: &[Decision],
    drafts: &[PlannedDecision],
    ids: &HashMap<String, u64>,
    items: &HashMap<u64, Item>,
) -> Result<Vec<Decision>, String> {
    let mut result = previous.to_vec();
    let mut keys = HashSet::new();
    for d in drafts {
        for (value, name) in [
            (&d.key, "decision key"),
            (&d.question, "question"),
            (&d.context, "reason"),
            (&d.recommendation, "recommendation"),
            (&d.consequence, "effect"),
        ] {
            nonempty(value, name)?;
        }
        if !keys.insert(&d.key) {
            return Err("Duplicate decision key".into());
        }
        let affected_tasks = d
            .affected_tasks
            .iter()
            .map(|s| resolve(s, ids, items))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(old) = result.iter_mut().find(|old| old.key == d.key) {
            if old.question != d.question {
                return Err("Use a new decision key for a different question".into());
            }
            old.context = d.context.clone();
            old.recommendation = d.recommendation.clone();
            old.consequence = d.consequence.clone();
            old.affected_tasks = affected_tasks;
            old.retired = false;
        } else {
            result.push(Decision {
                key: d.key.clone(),
                question: d.question.clone(),
                context: d.context.clone(),
                recommendation: d.recommendation.clone(),
                consequence: d.consequence.clone(),
                affected_tasks,
                ..Decision::default()
            });
        }
    }
    Ok(result)
}

pub(super) fn apply_plan(
    items: &mut HashMap<u64, Item>,
    id: u64,
    flow: &mut Workflow,
    draft: &PlanDraft,
    max_id: u64,
) -> Result<(), String> {
    nonempty(&draft.summary, "plan summary")?;
    nonempty(&draft.reason, "plan change reason")?;
    nonempty_list(&draft.acceptance, "milestone acceptance criteria")?;
    if flow
        .plan
        .as_ref()
        .is_some_and(|p| !p.acceptance.is_empty() && p.acceptance != draft.acceptance)
    {
        return Err("Acceptance criteria changes require an explicit owner decision first".into());
    }
    if draft.tasks.is_empty() && draft.decisions.is_empty() {
        return Err("Provide tasks or a concrete unresolved decision".into());
    }
    let mut next_id = max_id.max(items.keys().copied().max().unwrap_or(0));
    let mut ids = HashMap::new();
    let mut used = HashSet::new();
    for task in &draft.tasks {
        nonempty(&task.key, "task key")?;
        let existing = task.item_id.or_else(|| {
            items
                .values()
                .find(|i| {
                    i.parent == Some(id)
                        && i.workflow
                            .as_ref()
                            .and_then(|w| w.task.as_ref())
                            .is_some_and(|t| t.key == task.key)
                })
                .map(|i| i.id)
        });
        let task_id = if let Some(existing) = existing {
            let item = items.get(&existing).ok_or("Unknown board item")?;
            if existing == id
                || item.parent.is_some_and(|p| p != id)
                || item.workflow.as_ref().is_some_and(|w| w.is_milestone())
                || (item.workflow.is_none()
                    && (crate::is_closed_status(&item.status) || !item.assignee.is_empty()))
            {
                return Err("Task already belongs to another milestone or execution owner".into());
            }
            existing
        } else {
            next_id = next_id.checked_add(1).ok_or("Board id space exhausted")?;
            next_id
        };
        if ids.insert(task.key.clone(), task_id).is_some() || !used.insert(task_id) {
            return Err("Duplicate task identity".into());
        }
    }
    let mut last_rank = items
        .values()
        .map(|i| i.rank.as_str())
        .max()
        .unwrap_or("")
        .to_string();
    for task in &draft.tasks {
        nonempty(&task.title, "task title")?;
        nonempty(&task.instructions, "task instructions")?;
        nonempty_list(&task.acceptance, "task acceptance criteria")?;
        nonempty_list(&task.scope.paths, "planned source scope")?;
        nonempty_list(&task.scope.functions, "planned functional scope")?;
        for path in &task.scope.paths {
            if path != "*"
                && (path.starts_with('/')
                    || path.split('/').any(|p| p == "." || p == "..")
                    || path.contains("//")
                    || path.contains('*'))
            {
                return Err(
                    "Source scope must use normalized relative files/directories, or *".into(),
                );
            }
        }
        let task_id = ids[&task.key];
        let depends_on = task
            .depends_on
            .iter()
            .map(|r| resolve(r, &ids, items))
            .collect::<Result<Vec<_>, _>>()?;
        let goal_revision = items
            .get(&task_id)
            .and_then(|i| i.workflow.as_ref())
            .filter(|w| w.result.is_some() || w.integrated.is_some())
            .and_then(|w| w.task.as_ref())
            .map_or(flow.goal_revision, |t| t.goal_revision);
        let spec = TaskSpec {
            key: task.key.clone(),
            acceptance: task.acceptance.clone(),
            scope: task.scope.clone(),
            goal_revision,
        };
        let item = items.entry(task_id).or_insert_with(|| {
            last_rank = rank_between((!last_rank.is_empty()).then_some(last_rank.as_str()), None)
                .unwrap_or_else(|| format!("{last_rank}m"));
            Item {
                id: task_id,
                rank: last_rank.clone(),
                ..Item::default()
            }
        });
        let mut task_flow = item.workflow.as_deref().cloned().unwrap_or_default();
        let changed = item.title != task.title
            || item.body != task.instructions
            || item.depends_on != depends_on
            || task_flow.task.as_ref() != Some(&spec);
        if changed
            && (task_flow.active.is_some()
                || task_flow.result.is_some()
                || task_flow.integrated.is_some())
        {
            return Err(format!(
                "Preserve running or completed task #{task_id}; add corrective work separately"
            ));
        }
        item.title = task.title.clone();
        item.body = task.instructions.clone();
        item.parent = Some(id);
        item.depends_on = depends_on;
        task_flow.task = Some(spec);
        if task.retry && task_flow.active.is_none() && task_flow.integrated.is_none() {
            task_flow.problem = None;
            task_flow.history.push(format!("Retry: {}", draft.reason));
        }
        if item.status != "archived" {
            item.status = task_flow.item_status().into();
        }
        item.workflow = Some(Box::new(task_flow));
    }
    let previous = flow.plan.clone().unwrap_or_default();
    for old_id in &previous.tasks {
        if used.contains(old_id) {
            continue;
        }
        let item = items.get_mut(old_id).ok_or("Previous task disappeared")?;
        let w = item
            .workflow
            .as_ref()
            .ok_or("Previous task lost workflow")?;
        if w.active.is_some() || w.result.is_some() || w.integrated.is_some() {
            return Err(format!("Retain running or completed task #{old_id}"));
        }
        item.status = "archived".into();
    }
    let mut decisions = decisions(&previous.decisions, &draft.decisions, &ids, items)?;
    for d in &mut decisions {
        if !d.affected_tasks.is_empty()
            && d.affected_tasks
                .iter()
                .all(|id| items.get(id).is_some_and(|i| i.status == "archived"))
        {
            d.retired = true;
        }
    }
    super::scheduling::validate_dependencies(items)?;
    flow.plan = Some(Plan {
        summary: draft.summary.clone(),
        acceptance: draft.acceptance.clone(),
        tasks: draft.tasks.iter().map(|t| ids[&t.key]).collect(),
        decisions,
    });
    flow.history.push(draft.reason.clone());
    flow.history
        .extend(draft.implementation_decisions.iter().cloned());
    let mut seen = HashSet::new();
    let priorities = draft
        .priorities
        .iter()
        .copied()
        .chain(draft.tasks.iter().map(|t| ids[&t.key]))
        .collect::<Vec<_>>();
    let mut rank = String::new();
    for item_id in priorities {
        if !seen.insert(item_id) {
            continue;
        }
        let target = items
            .get_mut(&item_id)
            .ok_or("Priority references an unknown board item")?;
        rank = rank_between((!rank.is_empty()).then_some(rank.as_str()), None)
            .ok_or("Priority rank exhausted")?;
        target.rank = rank.clone();
    }
    let order: Vec<_> = ordered_items(items).iter().map(|i| i.id).collect();
    rank.clear();
    for item_id in order {
        rank = rank_between((!rank.is_empty()).then_some(rank.as_str()), None)
            .ok_or("Priority rank exhausted")?;
        items.get_mut(&item_id).unwrap().rank = rank.clone();
    }
    Ok(())
}
