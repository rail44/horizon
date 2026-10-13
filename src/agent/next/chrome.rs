//! Every fixed label the pane draws, in each chrome language. The
//! conversation's own text is data and is never looked up here.

use super::display::Task;

/// The language of the pane's fixed labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Chrome {
    Japanese,
    English,
}

/// The fixed labels of one chrome language. Labels with a count or a task
/// in them are [`Chrome`]'s methods.
pub(crate) struct Labels {
    /// The running session's state chip, and a running receipt's chip.
    pub(crate) running: &'static str,
    /// The state chip while an approval is pending.
    pub(crate) waiting_for_approval: &'static str,
    pub(crate) stopped: &'static str,
    pub(crate) failed: &'static str,
    pub(crate) stop: &'static str,
    pub(crate) approve: &'static str,
    pub(crate) deny: &'static str,
    /// The chip on the pending approval's row.
    pub(crate) approval_needed: &'static str,
    pub(crate) thinking: &'static str,
    pub(crate) placeholder: &'static str,
    pub(crate) placeholder_ended: &'static str,
    pub(crate) placeholder_unreachable: &'static str,
    /// What follows the `Enter` key chip in the composer.
    pub(crate) send_hint: &'static str,
    pub(crate) session_ended: &'static str,
    pub(crate) runtime_unreachable: &'static str,
}

const JAPANESE: Labels = Labels {
    running: "実行中",
    waiting_for_approval: "承認待ち",
    stopped: "中断",
    failed: "失敗",
    stop: "停止",
    approve: "承認",
    deny: "拒否",
    approval_needed: "承認待ち",
    thinking: "考え中",
    placeholder: "メッセージを書く",
    placeholder_ended: "このセッションは終了しています",
    placeholder_unreachable: "ランタイムに到達できません",
    send_hint: "で送信",
    session_ended: "セッションは終了しました",
    runtime_unreachable: "ランタイムに到達できません",
};

const ENGLISH: Labels = Labels {
    running: "Running",
    waiting_for_approval: "Waiting for approval",
    stopped: "Stopped",
    failed: "Failed",
    stop: "Stop",
    approve: "Approve",
    deny: "Deny",
    approval_needed: "Approval needed",
    thinking: "Thinking",
    placeholder: "Write a message",
    placeholder_ended: "This session has ended",
    placeholder_unreachable: "Runtime unreachable",
    send_hint: "to send",
    session_ended: "Session ended",
    runtime_unreachable: "Runtime unreachable",
};

impl Chrome {
    pub(crate) fn labels(self) -> &'static Labels {
        match self {
            Self::Japanese => &JAPANESE,
            Self::English => &ENGLISH,
        }
    }

    /// A merged receipt line's count of failed calls.
    pub(crate) fn failed_calls(self, count: usize) -> String {
        match self {
            Self::Japanese => format!("失敗 {count}件"),
            Self::English => format!("{count} failed"),
        }
    }

    /// The line under an output whose tail was cut.
    pub(crate) fn more_lines(self, count: usize) -> String {
        match self {
            Self::Japanese => format!("…他 {count} 行"),
            Self::English => format!("… {count} more lines"),
        }
    }

    /// One background task's line.
    pub(crate) fn task_line(self, task: &Task) -> String {
        let lead = match self {
            Self::Japanese => "バックグラウンド",
            Self::English => "Background",
        };
        format!(
            "{lead} · {} · {} · {}",
            task.description, task.elapsed, task.activity
        )
    }

    /// The folded line a thought of `chars` characters is drawn as.
    pub(crate) fn thought(self, chars: usize) -> String {
        match self {
            Self::Japanese => format!("思考 {chars} 文字"),
            Self::English => format!("Thought · {chars} characters"),
        }
    }

    /// Every label this chrome can draw, the counted ones at `count` and
    /// the task line over `task`.
    #[cfg(test)]
    pub(crate) fn all(self, count: usize, task: &Task) -> Vec<String> {
        let labels = self.labels();
        let mut all: Vec<String> = [
            labels.running,
            labels.waiting_for_approval,
            labels.stopped,
            labels.failed,
            labels.stop,
            labels.approve,
            labels.deny,
            labels.approval_needed,
            labels.thinking,
            labels.placeholder,
            labels.placeholder_ended,
            labels.placeholder_unreachable,
            labels.send_hint,
            labels.session_ended,
            labels.runtime_unreachable,
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        all.extend([
            self.failed_calls(count),
            self.more_lines(count),
            self.task_line(task),
            self.thought(count),
        ]);
        all
    }
}
