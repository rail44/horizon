use super::*;
use crate::Item;
use std::collections::HashMap;

/// Applies a validated transaction against the full board. logd persists the
/// changed items in a single event, including allocated task ids.
pub fn apply(
    current: &HashMap<u64, Item>,
    id: u64,
    mutation: Mutation,
    max_id: u64,
) -> Result<Vec<Item>, String> {
    let mut items = current.clone();
    let item = items.get(&id).ok_or("Board item not found")?;
    let mut flow = item.workflow.as_deref().cloned().unwrap_or_default();
    if matches!(mutation, Mutation::Enable) {
        if item.workflow.is_some() || crate::is_closed_status(&item.status) {
            return Err("Reopen an ordinary item before enabling a milestone".into());
        }
        flow.plan_requested = true;
    } else {
        if item.workflow.is_none() {
            return Err("Item has no workflow".into());
        }
        mutate(&mut items, id, &mut flow, mutation, max_id)?;
    }
    let item = items.get_mut(&id).unwrap();
    if item.status != "archived" {
        item.status = flow.item_status().into();
    }
    item.workflow = Some(Box::new(flow));
    let mut changed = Vec::new();
    for (id, mut item) in items {
        if current.get(&id) != Some(&item) {
            if let Some(w) = &mut item.workflow {
                w.revision = current
                    .get(&id)
                    .and_then(|i| i.workflow.as_ref())
                    .map_or(1, |w| w.revision + 1);
            }
            changed.push(item);
        }
    }
    changed.sort_by_key(|i| i.id);
    // Even a no-op consumes the CAS revision.
    if !changed.iter().any(|i| i.id == id) {
        let mut item = current[&id].clone();
        item.workflow.as_mut().unwrap().revision += 1;
        changed.push(item);
    }
    Ok(changed)
}

fn parent_replan(items: &mut HashMap<u64, Item>, id: u64) {
    if let Some(parent) = items[&id].parent.and_then(|p| items.get_mut(&p)) {
        if let Some(w) = &mut parent.workflow {
            w.plan_requested = true;
            w.plan_generation += 1;
            w.verification = None;
            w.achieved = false;
            if parent.status == "done" {
                parent.status = "in-progress".into();
            }
        }
    }
}

fn mutate(
    items: &mut HashMap<u64, Item>,
    id: u64,
    flow: &mut Workflow,
    mutation: Mutation,
    max_id: u64,
) -> Result<(), String> {
    match mutation {
        Mutation::Enable => unreachable!(),
        Mutation::Pause => flow.paused = true,
        Mutation::Resume | Mutation::Replan => {
            if flow.active.is_some() {
                return Err("Wait for the active attempt to stop".into());
            }
            flow.paused = false;
            flow.problem = None;
            if matches!(mutation, Mutation::Replan) {
                if flow.is_milestone() {
                    flow.plan_requested = true;
                    flow.verification = None;
                } else {
                    parent_replan(items, id);
                }
            }
        }
        Mutation::Answer { key, text } => {
            nonempty(&text, "message")?;
            let d = flow
                .plan
                .as_mut()
                .and_then(|p| p.decisions.iter_mut().find(|d| d.key == key && !d.retired))
                .ok_or("Decision not found")?;
            d.messages.push(DiscussionMessage { owner: true, text });
            d.resolution = None;
            flow.problem = None;
            flow.verification = None;
            flow.achieved = false;
        }
        Mutation::Start {
            token,
            session,
            work,
        } => {
            nonempty(&token, "attempt token")?;
            nonempty(&session, "session")?;
            if eligible_work(items, id).as_ref() != Some(&work) {
                return Err("Requested work is no longer eligible".into());
            }
            flow.active = Some(Attempt {
                token,
                session,
                work,
                generation: flow.plan_generation,
                report: None,
                attention: None,
            });
        }
        Mutation::SetWorker { worker } => {
            let active = flow.active.as_ref().ok_or("No active attempt")?;
            if !matches!(active.work, Work::Task { .. })
                || active.session != worker.session
                || flow.worker.is_some()
            {
                return Err("Worktree cannot be replaced".into());
            }
            nonempty(&worker.worktree, "worktree")?;
            nonempty(&worker.branch, "branch")?;
            flow.worker = Some(worker);
        }
        Mutation::SetVerifier { worker } => {
            let active = flow.active.as_ref().ok_or("No active verification")?;
            if active.work != Work::Verify
                || active.session != worker.session
                || flow.verifier.is_some()
            {
                return Err("Verification worktree cannot be replaced".into());
            }
            nonempty(&worker.worktree, "verification worktree")?;
            nonempty(&worker.branch, "verification branch")?;
            flow.verifier = Some(worker);
        }
        Mutation::PrepareVerification { base, head } => {
            if !flow.active.as_ref().is_some_and(|a| a.work == Work::Verify) {
                return Err("No active verification".into());
            }
            commit_id(&base)?;
            commit_id(&head)?;
            flow.integration = Some(Integration { base, head });
        }
        Mutation::Report {
            token,
            session,
            report,
        } => {
            let active = flow
                .active
                .as_ref()
                .filter(|a| a.token == token && a.session == session)
                .ok_or("This session does not own the attempt")?;
            if flow.paused || active.report.is_some() {
                return Err("Attempt paused or already reported".into());
            }
            validate_report(items, id, flow, &active.work, &report, max_id)?;
            flow.active.as_mut().unwrap().report = Some(report);
        }
        Mutation::Finish { token } => {
            let active = flow
                .active
                .take()
                .filter(|a| a.token == token)
                .ok_or("Attempt changed")?;
            finish(items, id, flow, &active, max_id)?;
            flow.last_attempt = Some(AttemptOutcome {
                attempt: active,
                problem: flow.problem.clone(),
            });
        }
        Mutation::Interrupt { token, reason } => {
            let attempt = flow
                .active
                .take()
                .filter(|a| a.token == token)
                .ok_or("Attempt changed")?;
            flow.last_attempt = Some(AttemptOutcome {
                attempt,
                problem: Some(reason.clone()),
            });
            flow.problem = Some(reason);
            parent_replan(items, id);
        }
        Mutation::Restart { token } => {
            let attempt = flow
                .active
                .take()
                .filter(|a| a.token == token)
                .ok_or("Attempt changed")?;
            let reason =
                "Runtime restarted; preserved work must be checked before continuing".to_string();
            if matches!(attempt.work, Work::Task { .. }) {
                flow.problem = Some(reason.clone());
                parent_replan(items, id);
            } else {
                flow.problem = None;
                if attempt.work == Work::Verify {
                    flow.verification = None;
                    flow.integration = None;
                } else {
                    flow.plan_requested = true;
                }
            }
            flow.last_attempt = Some(AttemptOutcome {
                attempt,
                problem: Some(reason),
            });
        }
        Mutation::Attention { token, message } => {
            flow.active
                .as_mut()
                .filter(|a| a.token == token)
                .ok_or("Attempt changed")?
                .attention = message;
        }
        Mutation::Integrated { commit } => {
            if flow.merging.as_ref().is_none_or(|i| i.head != commit) {
                return Err("No reserved integration candidate".into());
            }
            flow.integrated = Some(commit);
            flow.merging = None;
            parent_replan(items, id);
        }
        Mutation::BeginIntegration => {
            if crate::is_closed_status(&items[&id].status)
                || flow.paused
                || flow.active.is_some()
                || flow.problem.is_some()
                || flow.integrated.is_some()
                || flow.verification.as_ref().is_none_or(|v| {
                    v.evidence.is_empty()
                        || !v.decisions.is_empty()
                        || v.evidence.iter().any(|e| !e.satisfied)
                })
            {
                return Err("No verified integration candidate".into());
            }
            let parent = items[&id]
                .parent
                .and_then(|p| items.get(&p))
                .ok_or("Task has no milestone")?;
            let parent_flow = parent.workflow.as_ref().ok_or("Task has no milestone")?;
            if crate::is_closed_status(&parent.status)
                || parent_flow.paused
                || flow
                    .task
                    .as_ref()
                    .is_none_or(|t| t.goal_revision != parent_flow.goal_revision)
                || task_waits_for_decision(items, id)
            {
                return Err("Integration awaits a decision or resume".into());
            }
            flow.merging = Some(flow.integration.clone().ok_or("No prepared candidate")?);
        }
        Mutation::IntegrationFailed { reason } => {
            flow.merging = None;
            flow.problem = Some(reason);
            parent_replan(items, id);
        }
        Mutation::Reverify { ref reason } | Mutation::Repair { ref reason } => {
            if flow.active.is_some() || flow.integrated.is_some() {
                return Err("Wait for the active attempt or preserve integrated work".into());
            }
            if matches!(mutation, Mutation::Repair { .. }) {
                flow.result = None;
            }
            flow.history.push(reason.clone());
            flow.verification = None;
            flow.integration = None;
            flow.merging = None;
            flow.problem = None;
        }
    }
    Ok(())
}

fn commit_id(value: &str) -> Result<(), String> {
    if (value.len() == 40 || value.len() == 64) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("Provide the full Git commit id".into())
    }
}

fn validate_report(
    items: &HashMap<u64, Item>,
    id: u64,
    flow: &Workflow,
    work: &Work,
    report: &Report,
    max_id: u64,
) -> Result<(), String> {
    match (work, report) {
        (Work::Plan, Report::Plan { plan }) => {
            planning::apply_plan(&mut items.clone(), id, &mut flow.clone(), plan, max_id)
        }
        (
            Work::Discuss { .. },
            Report::Discussion {
                reply,
                resolution,
                acceptance,
            },
        ) => {
            nonempty(reply, "discussion reply")?;
            if let Some(text) = resolution {
                nonempty(text, "decision")?;
            }
            if let Some(criteria) = acceptance {
                if resolution.is_none() {
                    return Err("Criteria changes require a settled owner decision".into());
                }
                nonempty_list(criteria, "acceptance criteria")?;
            }
            Ok(())
        }
        (
            Work::Task { .. },
            Report::Task {
                summary,
                checks,
                commit,
            },
        ) => {
            nonempty(summary, "result summary")?;
            nonempty_list(checks, "actual checks and outcomes")?;
            commit_id(commit)
        }
        (Work::Verify, Report::Verification { verification: v }) => {
            nonempty(&v.summary, "verification summary")?;
            nonempty_list(&v.checks, "verification checks")?;
            if flow.integration.as_ref().is_none_or(|i| i.head != v.commit) {
                return Err("Verification must identify the prepared commit".into());
            }
            let criteria = flow
                .task
                .as_ref()
                .map(|t| &t.acceptance)
                .or_else(|| flow.plan.as_ref().map(|p| &p.acceptance))
                .ok_or("No acceptance criteria")?;
            if criteria.len() != v.evidence.len()
                || criteria
                    .iter()
                    .any(|c| v.evidence.iter().filter(|e| &e.criterion == c).count() != 1)
            {
                return Err("Provide evidence for every acceptance criterion exactly once".into());
            }
            let decisions = flow.plan.as_ref().or_else(|| {
                items[&id]
                    .parent
                    .and_then(|p| items.get(&p))?
                    .workflow
                    .as_ref()?
                    .plan
                    .as_ref()
            });
            for e in &v.evidence {
                nonempty(&e.detail, "verification evidence")?;
                if e.satisfied
                    && e.decision.is_none()
                    && e.check
                        .as_ref()
                        .is_none_or(|command| !v.checks.contains(command))
                {
                    return Err("Automated evidence must reference a verification command".into());
                }
                if e.satisfied
                    && e.decision.as_ref().is_some_and(|key| {
                        decisions.is_none_or(|p| {
                            !p.decisions
                                .iter()
                                .any(|d| &d.key == key && d.resolution.is_some() && !d.retired)
                        })
                    })
                {
                    return Err("Human evaluation has not been settled".into());
                }
            }
            Ok(())
        }
        (_, Report::Blocked { reason }) => nonempty(reason, "blocker"),
        _ => Err("Report type does not match assigned work".into()),
    }
}

fn finish(
    items: &mut HashMap<u64, Item>,
    id: u64,
    flow: &mut Workflow,
    active: &Attempt,
    max_id: u64,
) -> Result<(), String> {
    let Some(report) = &active.report else {
        flow.problem =
            Some("Session stopped without a result; inspect the preserved work and retry".into());
        parent_replan(items, id);
        return Ok(());
    };
    match report {
        Report::Plan { plan } => {
            planning::apply_plan(items, id, flow, plan, max_id)?;
            flow.plan_requested = flow.plan_generation != active.generation;
        }
        Report::Discussion {
            reply,
            resolution,
            acceptance,
        } => {
            let Work::Discuss { key, turn } = &active.work else {
                return Err("Invalid discussion attempt".into());
            };
            let plan = flow.plan.as_mut().ok_or("Plan disappeared")?;
            let d = plan
                .decisions
                .iter_mut()
                .find(|d| &d.key == key)
                .ok_or("Decision disappeared")?;
            if d.messages.len() != *turn {
                return Ok(());
            }
            d.messages.push(DiscussionMessage {
                owner: false,
                text: reply.clone(),
            });
            d.resolution = resolution.clone();
            if resolution.is_some() {
                for affected in &d.affected_tasks {
                    if let Some(w) = items.get_mut(affected).and_then(|i| i.workflow.as_mut()) {
                        w.verification = None;
                        w.problem = None;
                    }
                }
                if let Some(acceptance) = acceptance {
                    plan.acceptance = acceptance.clone();
                }
                flow.plan_requested = true;
                flow.plan_generation += 1;
                flow.verification = None;
            }
        }
        Report::Task {
            summary,
            checks,
            commit,
        } => {
            flow.result = Some(TaskResult {
                session: active.session.clone(),
                summary: summary.clone(),
                checks: checks.clone(),
                commit: commit.clone(),
            });
            parent_replan(items, id);
        }
        Report::Verification { verification: v } => {
            validate_report(items, id, flow, &Work::Verify, report, max_id)?;
            flow.verification = Some(v.clone());
            if !v.decisions.is_empty() {
                let parent_id = if flow.is_milestone() {
                    id
                } else {
                    items[&id].parent.ok_or("Task has no milestone")?
                };
                let keys: HashMap<_, _> = items
                    .values()
                    .filter(|i| i.parent == Some(parent_id))
                    .filter_map(|i| Some((i.workflow.as_ref()?.task.as_ref()?.key.clone(), i.id)))
                    .collect();
                if parent_id == id {
                    let plan = flow.plan.as_mut().ok_or("Missing plan")?;
                    plan.decisions =
                        planning::decisions(&plan.decisions, &v.decisions, &keys, items)?;
                } else {
                    let mut parent = items[&parent_id].clone();
                    let plan = parent
                        .workflow
                        .as_mut()
                        .and_then(|w| w.plan.as_mut())
                        .ok_or("Missing parent plan")?;
                    plan.decisions =
                        planning::decisions(&plan.decisions, &v.decisions, &keys, items)?;
                    items.insert(parent_id, parent);
                }
            }
            if v.evidence.iter().all(|e| e.satisfied) && v.decisions.is_empty() {
                if flow.is_milestone() {
                    flow.achieved = true;
                }
            } else if v.decisions.is_empty() {
                flow.history.push(v.summary.clone());
                flow.verification = None;
                if flow.is_milestone() {
                    flow.plan_requested = true;
                } else {
                    flow.result = None;
                    parent_replan(items, id);
                }
            }
        }
        Report::Blocked { reason } => {
            flow.problem = Some(reason.clone());
            parent_replan(items, id);
        }
    }
    Ok(())
}
