//! Provider response outcomes, before the session decides whether another round is needed.

use std::{collections::HashMap, num::NonZeroUsize};

use crate::contract::ToolCallId;

use super::ToolCallDescriptor;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::providers::rig) enum Truncation {
    Tools {
        unfinished: NonZeroUsize,
        output_cap: bool,
    },
    OutputCap,
}

#[derive(Debug, Eq, PartialEq)]
pub(in crate::providers::rig) enum CompletionStop {
    Finished { text: String },
    Cancelled,
    Failed,
    Refused,
    Unknown { reason: String },
    Truncated(Truncation),
}

impl Default for CompletionStop {
    fn default() -> Self {
        Self::Finished {
            text: String::new(),
        }
    }
}

impl CompletionStop {
    pub(in crate::providers::rig) fn from_response(
        cancelled: bool,
        unfinished: usize,
        output_cap: bool,
        text: String,
    ) -> Self {
        if cancelled {
            Self::Cancelled
        } else if let Some(unfinished) = NonZeroUsize::new(unfinished) {
            Self::Truncated(Truncation::Tools {
                unfinished,
                output_cap,
            })
        } else if output_cap {
            Self::Truncated(Truncation::OutputCap)
        } else {
            Self::Finished { text }
        }
    }

    pub(super) fn from_provider(
        reason: Option<rig_core::completion::FinishReason>,
        unfinished: usize,
        output_tokens: Option<u64>,
        cap: u64,
        text: String,
    ) -> Self {
        use rig_core::completion::FinishReason;
        match reason {
            Some(FinishReason::ContentFilter) => Self::Refused,
            Some(FinishReason::Other(reason)) => Self::Unknown { reason },
            Some(FinishReason::Length) => Self::from_response(false, unfinished, true, text),
            Some(FinishReason::Stop | FinishReason::ToolCalls) => {
                Self::from_response(false, unfinished, false, text)
            }
            None => Self::from_response(
                false,
                unfinished,
                super::output_cap_truncated(output_tokens, cap, false),
                text,
            ),
        }
    }

    pub(in crate::providers::rig) fn truncation(&self) -> Option<Truncation> {
        match self {
            Self::Truncated(reason) => Some(*reason),
            Self::Finished { .. }
            | Self::Cancelled
            | Self::Failed
            | Self::Refused
            | Self::Unknown { .. } => None,
        }
    }
}

/// Issued calls and reported usage survive cancellation and truncation. They
/// remain available for history repair and compaction even without a final answer.
#[derive(Debug, Default)]
pub(in crate::providers::rig) struct TurnCompletion {
    pub(in crate::providers::rig) stop: CompletionStop,
    pub(in crate::providers::rig) requested_tool_call_ids: Vec<ToolCallId>,
    pub(in crate::providers::rig) requested_tool_calls: HashMap<ToolCallId, ToolCallDescriptor>,
    pub(in crate::providers::rig) input_tokens: Option<u64>,
    pub(in crate::providers::rig) output_tokens: Option<u64>,
}

impl TurnCompletion {
    pub(in crate::providers::rig) fn is_completing(&self) -> bool {
        matches!(self.stop, CompletionStop::Finished { .. })
            && self.requested_tool_call_ids.is_empty()
    }

    #[cfg(test)]
    pub(in crate::providers::rig) fn final_text(&self) -> Option<&str> {
        match &self.stop {
            CompletionStop::Finished { text } => Some(text),
            _ => None,
        }
    }
}
