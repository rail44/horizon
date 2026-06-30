use std::time::Instant;

use crossbeam_channel::Sender;

use crate::agent::{AgentEvent, AgentMessageDelta, AgentMessageRole, AgentProviderEvent};

const STREAM_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
const STREAM_FLUSH_CHARS: usize = 320;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StreamDeltaKind {
    Reasoning,
    AssistantText,
}

pub(super) struct StreamDeltaBuffer {
    events_tx: Sender<AgentProviderEvent>,
    kind: StreamDeltaKind,
    role: AgentMessageRole,
    text: String,
    last_flush: Instant,
}

impl StreamDeltaBuffer {
    pub(super) fn new(
        events_tx: Sender<AgentProviderEvent>,
        kind: StreamDeltaKind,
        role: AgentMessageRole,
    ) -> Self {
        Self {
            events_tx,
            kind,
            role,
            text: String::new(),
            last_flush: Instant::now(),
        }
    }

    pub(super) fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }

        let should_flush = text.contains('\n')
            || self.text.chars().count() + text.chars().count() >= STREAM_FLUSH_CHARS;
        self.text.push_str(&text);
        if should_flush || self.last_flush.elapsed() >= STREAM_FLUSH_INTERVAL {
            self.flush();
        }
    }

    pub(super) fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }

        let text = std::mem::take(&mut self.text);
        let event = match self.kind {
            StreamDeltaKind::Reasoning => AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: self.role,
                text,
            }),
            StreamDeltaKind::AssistantText => AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: self.role,
                text,
            }),
        };
        let _ = self.events_tx.send(event.into());
        self.last_flush = Instant::now();
    }
}
