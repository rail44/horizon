use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
    /// The command itself ran and failed (a panic in the worker, a spawn the
    /// harness could not complete). Retrying the same command is a reasonable
    /// next step.
    Failed,
    /// The containment layer failed before the command could run or report:
    /// the sandbox helper died, its report never arrived, or its report was
    /// rejected. The command's own text is not the problem, so re-running it
    /// unchanged cannot succeed; the harness or the runner state is.
    InfrastructureFailed {
        reason: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BashOutput {
    #[serde(flatten)]
    pub termination: BashTermination,
    /// Why the call ended the way it did, when the harness has something to
    /// say beyond the termination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The command's combined stdout and stderr, capped.
    pub output: String,
    /// Whether the cap dropped output that exists.
    pub truncated: bool,
    /// Where the full output was spilled, when it was spilled.
    pub output_file: Option<String>,
    /// A problem the harness wants the model to know about, such as the drain
    /// cutting the result short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Where the command left bash's working directory, when the wrapper's
    /// report showed it has moved. Absolute, at most one path, and only on a
    /// *change*: repeating an unchanged directory on every result would
    /// re-write the model's prompt cache to say nothing. `None` when no report
    /// arrived or it named the directory the call already started in. A problem
    /// the harness wants to flag is `note`'s job -- this field only ever means
    /// "you are here now".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct TaskReport {
    /// The child session this report is about.
    pub session_id: String,
    /// The label the child was launched with.
    pub description: String,
    /// What the child found, once it finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// Whether the report is shorter than what the child wrote.
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
            BashTermination::InfrastructureFailed { .. } => {
                "sandbox failure (not the command)".into()
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner failure must not read like a command failure: the model
    /// decides whether to re-run the same text from this distinction alone.
    #[test]
    fn infrastructure_failure_is_distinct_from_an_ordinary_failure() {
        let runner = BashOutput {
            termination: BashTermination::InfrastructureFailed {
                reason: "sandbox supervisor report failed".into(),
            },
            message: None,
            output: String::new(),
            truncated: false,
            output_file: None,
            note: None,
            cwd: None,
        };
        assert_eq!(runner.exit_code(), None);
        assert_eq!(runner.summary(), "sandbox failure (not the command)");

        let ordinary = BashOutput {
            termination: BashTermination::Failed,
            ..runner.clone()
        };
        assert_eq!(ordinary.summary(), "execution failed");
        assert_ne!(ordinary.termination, runner.termination);
    }

    /// The new variant round-trips through the serialized tool output that is
    /// persisted in the event log and shown to the model.
    #[test]
    fn infrastructure_failure_round_trips_through_json() {
        let output = BashOutput {
            termination: BashTermination::InfrastructureFailed {
                reason: "helper exited".into(),
            },
            message: Some("helper exited".into()),
            output: String::new(),
            truncated: false,
            output_file: None,
            note: None,
            cwd: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["termination"], "infrastructure_failed");
        assert_eq!(json["reason"], "helper exited");
        assert_eq!(serde_json::from_value::<BashOutput>(json).unwrap(), output);
    }

    /// A reported directory travels with the result and comes back as a path,
    /// not as text the reader has to re-parse.
    #[test]
    fn a_reported_cwd_round_trips_through_json() {
        let output = BashOutput {
            termination: BashTermination::Exited { exit_code: 0 },
            message: None,
            output: String::new(),
            truncated: false,
            output_file: None,
            note: None,
            cwd: Some(PathBuf::from("/repo/crates/horizon-agent")),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["cwd"], "/repo/crates/horizon-agent");
        assert_eq!(serde_json::from_value::<BashOutput>(json).unwrap(), output);
    }

    /// An unchanged directory leaves no trace in the serialized output: the
    /// key is absent, not `null`.
    #[test]
    fn an_absent_cwd_is_omitted_from_json() {
        let output = BashOutput {
            termination: BashTermination::Exited { exit_code: 0 },
            message: None,
            output: "hi".into(),
            truncated: false,
            output_file: None,
            note: None,
            cwd: None,
        };
        assert!(serde_json::to_value(&output).unwrap().get("cwd").is_none());
    }

    /// A result recorded before this field existed carries no `cwd` key at
    /// all, and the event log must replay it unchanged.
    #[test]
    fn a_record_without_a_cwd_still_reads() {
        let record = serde_json::json!({
            "termination": "exited",
            "exit_code": 3,
            "output": "boom",
            "truncated": false,
            "output_file": null,
        });
        let output: BashOutput = serde_json::from_value(record).unwrap();
        assert_eq!(output.cwd, None);
        assert_eq!(output.exit_code(), Some(3));
    }
}
