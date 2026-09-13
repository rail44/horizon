use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Workflow {
    pub revision: u64,
    pub plan_requested: bool,
    pub plan_generation: u64,
    pub goal_revision: u64,
    pub paused: bool,
    pub problem: Option<String>,
    pub plan: Option<Plan>,
    pub task: Option<TaskSpec>,
    pub active: Option<Attempt>,
    pub last_attempt: Option<AttemptOutcome>,
    pub worker: Option<Worker>,
    pub verifier: Option<Worker>,
    pub result: Option<TaskResult>,
    pub verification: Option<Verification>,
    pub integration: Option<Integration>,
    pub merging: Option<Integration>,
    pub integrated: Option<String>,
    pub achieved: bool,
    pub history: Vec<String>,
    /// Explicit owner ordering constraints; AI ranking cannot override them.
    pub before: Vec<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    pub summary: String,
    pub acceptance: Vec<String>,
    /// Canonical board ids, not a second collection of task definitions.
    pub tasks: Vec<u64>,
    pub decisions: Vec<Decision>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PlanDraft {
    pub summary: String,
    pub reason: String,
    pub acceptance: Vec<String>,
    pub tasks: Vec<PlannedTask>,
    pub decisions: Vec<PlannedDecision>,
    pub priorities: Vec<u64>,
    pub implementation_decisions: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PlannedTask {
    pub key: String,
    /// Supply an existing board id to adopt or revise an existing task.
    pub item_id: Option<u64>,
    pub title: String,
    pub instructions: String,
    pub acceptance: Vec<String>,
    /// Draft keys, or #123 for an existing board item outside this draft.
    pub depends_on: Vec<String>,
    pub scope: ChangeScope,
    pub retry: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskSpec {
    pub key: String,
    pub acceptance: Vec<String>,
    pub scope: ChangeScope,
    pub goal_revision: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ChangeScope {
    /// Repository-relative file or directory paths. '*' denotes the project.
    pub paths: Vec<String>,
    /// Shared behavior/interface identifiers assessed by the planner.
    pub functions: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PlannedDecision {
    pub key: String,
    pub question: String,
    pub context: String,
    pub recommendation: String,
    pub consequence: String,
    pub affected_tasks: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    pub key: String,
    pub question: String,
    pub context: String,
    pub recommendation: String,
    pub consequence: String,
    pub affected_tasks: Vec<u64>,
    pub messages: Vec<DiscussionMessage>,
    pub resolution: Option<String>,
    pub retired: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiscussionMessage {
    pub owner: bool,
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
    pub generation: u64,
    /// Provisional until a real assignment turn boundary is observed.
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
    Discuss { key: String, turn: usize },
    Task { key: String },
    Verify,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Report {
    Plan {
        plan: PlanDraft,
    },
    Discussion {
        reply: String,
        resolution: Option<String>,
        /// Only a resolved, explicitly authorized criteria change can set this.
        acceptance: Option<Vec<String>>,
    },
    Task {
        summary: String,
        checks: Vec<String>,
        commit: String,
    },
    Verification {
        verification: Verification,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskResult {
    pub session: String,
    pub summary: String,
    pub checks: Vec<String>,
    pub commit: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Verification {
    pub summary: String,
    pub commit: String,
    pub evidence: Vec<Evidence>,
    pub checks: Vec<String>,
    pub decisions: Vec<PlannedDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub criterion: String,
    pub detail: String,
    pub satisfied: bool,
    /// Human-evaluated evidence references a settled decision on the milestone.
    pub decision: Option<String>,
    /// Exact successful verification command supporting automated evidence.
    pub check: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Integration {
    pub base: String,
    pub head: String,
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
    SetVerifier {
        worker: Worker,
    },
    PrepareVerification {
        base: String,
        head: String,
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
    Restart {
        token: String,
    },
    Attention {
        token: String,
        message: Option<String>,
    },
    Integrated {
        commit: String,
    },
    BeginIntegration,
    IntegrationFailed {
        reason: String,
    },
    Reverify {
        reason: String,
    },
    Repair {
        reason: String,
    },
}

impl Workflow {
    pub fn is_milestone(&self) -> bool {
        self.task.is_none()
    }

    pub fn unanswered(&self) -> Vec<&Decision> {
        self.plan.as_ref().map_or_else(Vec::new, |p| {
            p.decisions
                .iter()
                .filter(|d| d.resolution.is_none() && !d.retired)
                .collect()
        })
    }

    pub fn label(&self) -> &'static str {
        if self.achieved || self.integrated.is_some() {
            return "done";
        }
        if self.paused {
            return "paused";
        }
        if let Some(a) = &self.active {
            if a.attention.is_some() {
                return "session needs attention";
            }
            return match a.work {
                Work::Plan => "planning",
                Work::Discuss { .. } => "discussing",
                Work::Task { .. } => "implementing",
                Work::Verify => "verifying",
            };
        }
        if self.problem.is_some() {
            return "blocked";
        }
        if self.plan_requested {
            return "planning queued";
        }
        if self
            .verification
            .as_ref()
            .is_some_and(|v| v.evidence.iter().all(|e| e.satisfied))
        {
            return "integration queued";
        }
        if self.result.is_some() {
            return "verification queued";
        }
        if !self.unanswered().is_empty() {
            return "decisions pending";
        }
        "in progress"
    }

    pub fn item_status(&self) -> &'static str {
        match self.label() {
            "done" => "done",
            "paused" | "blocked" | "session needs attention" => "blocked",
            _ => "in-progress",
        }
    }
}
