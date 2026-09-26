use std::collections::HashMap;
use std::time::Instant;

use crossbeam_channel::Sender;

use crate::config::RigAgentConfig;
use crate::contract::{Event, MessageDelta, MessageRole, ProviderEvent, ToolCallProgress};

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

/// Each tool stream owns its counters and flush clock until finalization.
/// Tombstones suppress late chunks and duplicate final records.
pub(super) struct ToolCallProgressBuffer {
    events_tx: Sender<ProviderEvent>,
    calls: HashMap<String, ToolReception>,
    flush_interval: std::time::Duration,
}
enum ToolReception {
    Receiving {
        tool_id: Option<String>,
        bytes: usize,
        last_flush: Instant,
    },
    Finalized,
}
impl ToolCallProgressBuffer {
    pub(super) fn new(events_tx: Sender<ProviderEvent>, config: &RigAgentConfig) -> Self {
        Self {
            events_tx,
            calls: HashMap::new(),
            flush_interval: std::time::Duration::from_millis(config.stream_flush_interval_ms),
        }
    }
    fn receiving(&mut self, key: &str) -> &mut ToolReception {
        self.calls
            .entry(key.to_owned())
            .or_insert_with(|| ToolReception::Receiving {
                tool_id: None,
                bytes: 0,
                last_flush: Instant::now(),
            })
    }
    pub(super) fn note_name(&mut self, key: &str, name: String) {
        if let ToolReception::Receiving { tool_id, .. } = self.receiving(key) {
            *tool_id = Some(name);
            self.flush_call(key);
        }
    }
    pub(super) fn note_delta(&mut self, key: &str, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let interval = self.flush_interval;
        if let ToolReception::Receiving {
            bytes, last_flush, ..
        } = self.receiving(key)
        {
            *bytes += chunk.len();
            if last_flush.elapsed() >= interval {
                self.flush_call(key);
            }
        }
    }
    #[cfg(test)]
    pub(super) fn flush_for_tests(&mut self) {
        for key in self.calls.keys().cloned().collect::<Vec<_>>() {
            self.flush_call(&key);
        }
    }
    /// Returns false when this stream already finalized the same call.
    pub(super) fn note_finalized(&mut self, key: &str) -> bool {
        if matches!(
            self.calls.insert(key.to_owned(), ToolReception::Finalized),
            Some(ToolReception::Finalized)
        ) {
            return false;
        }
        let _ = self
            .events_tx
            .send(ProviderEvent::ToolCallProgressClosed(key.to_owned()));
        true
    }
    pub(super) fn truncated_ids(&self) -> Vec<String> {
        self.calls
            .iter()
            .filter_map(|(key, state)| {
                matches!(state, ToolReception::Receiving { .. }).then_some(key.clone())
            })
            .collect()
    }
    fn flush_call(&mut self, key: &str) {
        let Some(ToolReception::Receiving {
            tool_id,
            bytes,
            last_flush,
        }) = self.calls.get_mut(key)
        else {
            return;
        };
        let _ = self
            .events_tx
            .send(ProviderEvent::tool_call_progress(ToolCallProgress {
                key: key.to_owned(),
                tool_id: tool_id.clone(),
                bytes: *bytes,
            }));
        *last_flush = Instant::now();
    }
}
