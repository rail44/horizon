use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "termination", rename_all = "snake_case")]
pub enum BashTermination {
    Exited {
        exit_code: i32,
    },
    TimedOut {
        timeout_secs: u64,
    },
    Reused {
        exit_code: Option<i64>,
        reused_output: bool,
    },
    Terminated,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BashOutput {
    #[serde(flatten)]
    pub termination: BashTermination,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub output: String,
    pub truncated: bool,
    pub output_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TaskReport {
    pub session_id: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub capped: bool,
    /// A child may fail with a useful partial report. This is independent of
    /// the cap and report, and is the authority for task-output failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
impl TaskReport {
    pub(crate) fn failed(&self) -> bool {
        self.message.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum TaskOutput {
    Started {
        session_id: String,
        description: String,
    },
    Running {
        session_id: String,
        description: String,
        message: String,
    },
    Finished {
        #[serde(flatten)]
        report: TaskReport,
    },
}

impl BashOutput {
    pub fn exit_code(&self) -> Option<i64> {
        match self.termination {
            BashTermination::Exited { exit_code } => Some(exit_code.into()),
            BashTermination::Reused { exit_code, .. } => exit_code,
            _ => None,
        }
    }
    pub fn summary(&self) -> String {
        match self.termination {
            BashTermination::Exited { exit_code } => format!("exit {exit_code}"),
            BashTermination::Reused { .. } => "reused output".into(),
            BashTermination::TimedOut { .. } => "timed out".into(),
            BashTermination::Terminated => "terminated".into(),
            BashTermination::Failed => "execution failed".into(),
        }
    }
}
impl TaskOutput {
    pub(crate) fn description(&self) -> &str {
        match self {
            Self::Started { description, .. } | Self::Running { description, .. } => description,
            Self::Finished { report } => &report.description,
        }
    }
    pub(crate) fn status(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Running { .. } => "running",
            Self::Finished { .. } => "finished",
        }
    }
}
