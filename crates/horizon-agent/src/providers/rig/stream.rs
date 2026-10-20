use std::time::Instant;

use crossbeam_channel::Sender;

use crate::config::RigAgentConfig;
use crate::contract::{Event, MessageDelta, MessageRole, ProviderEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StreamDeltaKind {
    Reasoning,
    AssistantText,
}

pub(super) struct StreamDeltaBuffer {
    events_tx: Sender<ProviderEvent>,
    kind: StreamDeltaKind,
    role: MessageRole,
    text: String,
    last_flush: Instant,
    flush_interval: std::time::Duration,
    flush_chars: usize,
}

impl StreamDeltaBuffer {
    pub(super) fn new(
        events_tx: Sender<ProviderEvent>,
        kind: StreamDeltaKind,
        role: MessageRole,
        config: &RigAgentConfig,
    ) -> Self {
        Self {
            events_tx,
            kind,
            role,
            text: String::new(),
            last_flush: Instant::now(),
            flush_interval: std::time::Duration::from_millis(config.stream_flush_interval_ms),
            flush_chars: config.stream_flush_chars,
        }
    }

    pub(super) fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }

        let should_flush = text.contains('\n')
            || self.text.chars().count() + text.chars().count() >= self.flush_chars;
        self.text.push_str(&text);
        if should_flush || self.last_flush.elapsed() >= self.flush_interval {
            self.flush();
        }
    }

    pub(super) fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }

        let text = std::mem::take(&mut self.text);
        let event = match self.kind {
            StreamDeltaKind::Reasoning => Event::ReasoningDelta(MessageDelta {
                role: self.role,
                text,
            }),
            StreamDeltaKind::AssistantText => Event::AssistantTextDelta(MessageDelta {
                role: self.role,
                text,
            }),
        };
        let _ = self.events_tx.send(event.into());
        self.last_flush = Instant::now();
    }
}
