//! Durable milestone planning and serial execution. Comments remain history;
//! this projection is the current plan and the outstanding owner decisions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Workflow {
    pub revision: u64,
    pub plan_requested: bool,
    pub paused: bool,
    pub problem: Option<String>,
    pub plan: Option<Plan>,
    pub answers: Vec<Answer>,
    pub results: Vec<TaskResult>,
    pub active: Option<Attempt>,
    pub last_attempt: Option<AttemptOutcome>,
    pub worker: Option<Worker>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    pub summary: String,
    pub acceptance: Vec<String>,
    /// Priority order. Dependencies take precedence over this order.
    pub tasks: Vec<PlannedTask>,
    pub decisions: Vec<Decision>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlannedTask {
    pub key: String,
    pub title: String,
    pub instructions: String,
    pub acceptance: Vec<String>,
    pub depends_on: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    pub key: String,
    pub question: String,
    pub context: String,
    pub recommendation: String,
    pub consequence: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Answer {
    pub key: String,
    pub question: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Worker {
    pub session: String,
    pub worktree: String,
    pub branch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Attempt {
    pub token: String,
    pub session: String,
    pub work: Work,
    /// A report is provisional until the session actually returns to idle.
    pub report: Option<Report>,
    pub attention: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttemptOutcome {
    pub attempt: Attempt,
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Work {
    Plan,
    Task { key: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Report {
    Plan {
        plan: Plan,
    },
    Task {
        summary: String,
        checks: Vec<String>,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskResult {
    pub key: String,
    pub session: String,
    pub summary: String,
    /// Agent-reported checks, not an independent verification certificate.
    pub checks: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Mutation {
    Enable,
    Answer {
        key: String,
        text: String,
    },
    Pause,
    Resume,
    Replan,
    Start {
        token: String,
        session: String,
        work: Work,
    },
    SetWorker {
        worker: Worker,
    },
    Report {
        token: String,
        session: String,
        report: Report,
    },
    Finish {
        token: String,
    },
    Interrupt {
        token: String,
        reason: String,
    },
    Attention {
        token: String,
        message: Option<String>,
    },
}

impl Workflow {
    pub fn item_status(&self) -> &'static str {
        match self.label() {
            "paused" | "blocked" | "needs your answer" | "session needs attention" => "blocked",
            "review results" => "review",
            _ => "in-progress",
        }
    }
    pub fn unanswered(&self) -> Vec<&Decision> {
        self.plan.as_ref().map_or_else(Vec::new, |plan| {
            plan.decisions
                .iter()
                .filter(|decision| {
                    !self.answers.iter().any(|answer| {
                        answer.key == decision.key && answer.question == decision.question
                    })
                })
                .collect()
        })
    }

    pub fn next_work(&self) -> Option<Work> {
        if self.paused || self.problem.is_some() || self.active.is_some() {
            return None;
        }
        if self.plan_requested {
            return Some(Work::Plan);
        }
        if !self.unanswered().is_empty() {
            return None;
        }
        self.plan
            .as_ref()?
            .tasks
            .iter()
            .find(|task| {
                !self.finished(&task.key) && task.depends_on.iter().all(|key| self.finished(key))
            })
            .map(|task| Work::Task {
                key: task.key.clone(),
            })
    }

    pub fn finished(&self, key: &str) -> bool {
        self.results.iter().any(|result| result.key == key)
    }

    pub fn label(&self) -> &'static str {
        if self.paused {
            return "paused";
        }
        if self.problem.is_some() {
            return "blocked";
        }
        if let Some(attempt) = &self.active {
            if attempt.attention.is_some() {
                return "session needs attention";
            }
            return match attempt.work {
                Work::Plan => "planning",
                Work::Task { .. } => "implementing",
            };
        }
        if self.plan_requested {
            return "planning queued";
        }
        if !self.unanswered().is_empty() {
            return "needs your answer";
        }
        if self.next_work().is_some() {
            return "implementation queued";
        }
        "review results"
    }
}

/// Validates and applies one operation to a clone. A caller persists the
/// resulting snapshot in a single event, under the board writer's lock.
pub fn apply(current: Option<&Workflow>, mutation: Mutation) -> Result<Workflow, String> {
    if matches!(mutation, Mutation::Enable) {
        if current.is_some() {
            return Err("This item is already a milestone".into());
        }
        return Ok(Workflow {
            revision: 1,
            plan_requested: true,
            ..Workflow::default()
        });
    }
    let mut flow = current.cloned().ok_or("This item is not a milestone")?;
    match mutation {
        Mutation::Enable => unreachable!(),
        Mutation::Pause => flow.paused = true,
        Mutation::Resume | Mutation::Replan => {
            if flow.active.is_some() {
                return Err("Wait for the current attempt to stop".into());
            }
            flow.paused = false;
            flow.problem = None;
            if matches!(mutation, Mutation::Replan) {
                flow.plan_requested = true;
            }
        }
        Mutation::Answer { key, text } => {
            if flow.active.is_some() {
                return Err("The current plan is still being updated".into());
            }
            nonempty(&text, "answer")?;
            let question = flow
                .unanswered()
                .into_iter()
                .find(|d| d.key == key)
                .ok_or("That decision is no longer awaiting an answer")?
                .question
                .clone();
            flow.answers.push(Answer {
                key,
                question,
                text,
            });
            if flow.unanswered().is_empty() {
                flow.plan_requested = true;
            }
        }
        Mutation::Start {
            token,
            session,
            work,
        } => {
            nonempty(&token, "attempt token")?;
            nonempty(&session, "session")?;
            if flow.next_work().as_ref() != Some(&work) {
                return Err("The requested work is no longer eligible".into());
            }
            flow.active = Some(Attempt {
                token,
                session,
                work,
                report: None,
                attention: None,
            });
        }
        Mutation::SetWorker { worker } => {
            let active = flow.active.as_ref().ok_or("No active attempt")?;
            if !matches!(active.work, Work::Task { .. }) || active.session != worker.session {
                return Err("Worker does not own the active task".into());
            }
            if flow.worker.is_some() {
                return Err("The milestone already has a worktree".into());
            }
            nonempty(&worker.worktree, "worktree")?;
            nonempty(&worker.branch, "branch")?;
            flow.worker = Some(worker);
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
                .ok_or("This session does not own the current attempt")?;
            if flow.paused {
                return Err("The milestone is paused".into());
            }
            if active.report.is_some() {
                return Err("This attempt has already submitted a report".into());
            }
            match (&active.work, &report) {
                (Work::Plan, Report::Plan { plan }) => validate_plan(&flow, plan)?,
                (Work::Task { .. }, Report::Task { summary, checks }) => {
                    nonempty(summary, "result summary")?;
                    nonempty_list(checks, "checks with commands and outcomes")?;
                }
                (_, Report::Blocked { reason }) => nonempty(reason, "blocker")?,
                _ => return Err("Report type does not match the active work".into()),
            }
            flow.active.as_mut().unwrap().report = Some(report);
        }
        Mutation::Finish { token } => {
            let active = flow
                .active
                .take()
                .filter(|a| a.token == token)
                .ok_or("The attempt has already changed")?;
            match active.report.clone() {
                Some(Report::Plan { plan }) => { flow.plan = Some(plan); flow.plan_requested = false; }
                Some(Report::Task { summary, checks }) => {
                    let Work::Task { key } = active.work.clone() else { return Err("Invalid task report".into()); };
                    flow.results.push(TaskResult { key, session: active.session.clone(), summary, checks });
                }
                Some(Report::Blocked { reason }) => flow.problem = Some(reason),
                None => flow.problem = Some("The session stopped without saving a result. Inspect the session, then retry or revise the plan.".into()),
            }
            flow.last_attempt = Some(AttemptOutcome {
                attempt: active,
                problem: flow.problem.clone(),
            });
        }
        Mutation::Attention { token, message } => {
            let active = flow
                .active
                .as_mut()
                .filter(|a| a.token == token)
                .ok_or("The attempt has already changed")?;
            active.attention = message;
        }
        Mutation::Interrupt { token, reason } => {
            if flow.active.as_ref().is_none_or(|a| a.token != token) {
                return Err("The attempt has already changed".into());
            }
            flow.last_attempt = flow.active.take().map(|attempt| AttemptOutcome {
                attempt,
                problem: Some(reason.clone()),
            });
            flow.problem = Some(reason);
        }
    }
    flow.revision += 1;
    Ok(flow)
}

fn nonempty(text: &str, field: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        Err(format!("Missing {field}"))
    } else {
        Ok(())
    }
}

fn nonempty_list(items: &[String], field: &str) -> Result<(), String> {
    if items.is_empty() || items.iter().any(|s| s.trim().is_empty()) {
        Err(format!("Provide nonempty {field}"))
    } else {
        Ok(())
    }
}

fn validate_plan(flow: &Workflow, plan: &Plan) -> Result<(), String> {
    use std::collections::HashSet;
    nonempty(&plan.summary, "plan summary")?;
    nonempty_list(&plan.acceptance, "milestone acceptance criteria")?;
    if plan.tasks.is_empty()
        && plan.decisions.iter().all(|d| {
            flow.answers
                .iter()
                .any(|a| a.key == d.key && a.question == d.question)
        })
    {
        return Err("A plan needs tasks or an unresolved decision".into());
    }
    let mut keys = HashSet::new();
    for task in &plan.tasks {
        nonempty(&task.key, "task key")?;
        nonempty(&task.title, "task title")?;
        nonempty(&task.instructions, "task instructions")?;
        nonempty_list(&task.acceptance, "task acceptance criteria")?;
        if !keys.insert(task.key.as_str()) {
            return Err("Duplicate task key".into());
        }
    }
    for task in &plan.tasks {
        if task
            .depends_on
            .iter()
            .any(|key| key == &task.key || !keys.contains(key.as_str()))
        {
            return Err(format!("Invalid dependency in {}", task.key));
        }
    }
    let mut visited = HashSet::new();
    loop {
        let before = visited.len();
        for task in &plan.tasks {
            if task
                .depends_on
                .iter()
                .all(|key| visited.contains(key.as_str()))
            {
                visited.insert(task.key.as_str());
            }
        }
        if visited.len() == plan.tasks.len() {
            break;
        }
        if visited.len() == before {
            return Err("Task dependencies contain a cycle".into());
        }
    }
    keys.clear();
    for decision in &plan.decisions {
        nonempty(&decision.key, "decision key")?;
        nonempty(&decision.question, "decision question")?;
        nonempty(&decision.context, "decision context")?;
        nonempty(&decision.recommendation, "decision recommendation")?;
        nonempty(&decision.consequence, "decision consequence")?;
        if !keys.insert(decision.key.as_str()) {
            return Err("Duplicate decision key".into());
        }
    }
    if let Some(previous) = &flow.plan {
        for result in &flow.results {
            let old = previous.tasks.iter().find(|t| t.key == result.key);
            let new = plan.tasks.iter().find(|t| t.key == result.key);
            if old.is_none() || old != new {
                return Err(format!(
                    "Preserve completed task {}; add follow-up work with a new key",
                    result.key
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
