use std::time::Instant;

use crossbeam_channel::Sender;

use crate::agent::contract::{Event, MessageDelta, MessageRole, ProviderEvent, ToolCallProgress};

const STREAM_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
const STREAM_FLUSH_CHARS: usize = 320;

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
}

impl StreamDeltaBuffer {
    pub(super) fn new(
        events_tx: Sender<ProviderEvent>,
        kind: StreamDeltaKind,
        role: MessageRole,
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

/// Coalesces rig's `StreamedAssistantContent::ToolCallDelta` chunks (a tool
/// call's name and JSON arguments, streamed piecemeal before the call is
/// complete) into periodic [`ToolCallProgress`] ticks, the same
/// time-gated-flush shape as [`StreamDeltaBuffer`] but keyed by rig's
/// `internal_call_id` — the one identifier stable across every chunk of a
/// single tool call from the very first one (the provider's own tool-call
/// id may still be empty at that point).
///
/// A name chunk always flushes immediately (it's a discrete, rare event
/// worth surfacing right away, e.g. "preparing `fs.write`…"); argument
/// chunks flush on the same cadence as text/reasoning deltas.
pub(super) struct ToolCallProgressBuffer {
    events_tx: Sender<ProviderEvent>,
    key: Option<String>,
    tool_id: Option<String>,
    bytes: usize,
    last_flush: Instant,
}

impl ToolCallProgressBuffer {
    pub(super) fn new(events_tx: Sender<ProviderEvent>) -> Self {
        Self {
            events_tx,
            key: None,
            tool_id: None,
            bytes: 0,
            last_flush: Instant::now(),
        }
    }

    pub(super) fn note_name(&mut self, key: &str, name: String) {
        self.ensure_key(key);
        self.tool_id = Some(name);
        self.flush_now();
    }

    pub(super) fn note_delta(&mut self, key: &str, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.ensure_key(key);
        self.bytes += chunk.len();
        if self.last_flush.elapsed() >= STREAM_FLUSH_INTERVAL {
            self.flush_now();
        }
    }

    /// Forces an immediate flush regardless of the time gate, for
    /// deterministic tests.
    #[cfg(test)]
    pub(super) fn flush_for_tests(&mut self) {
        self.flush_now();
    }

    fn ensure_key(&mut self, key: &str) {
        if self.key.as_deref() != Some(key) {
            self.key = Some(key.to_string());
            self.tool_id = None;
            self.bytes = 0;
        }
    }

    fn flush_now(&mut self) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let _ = self
            .events_tx
            .send(ProviderEvent::tool_call_progress(ToolCallProgress {
                key,
                tool_id: self.tool_id.clone(),
                bytes: self.bytes,
            }));
        self.last_flush = Instant::now();
    }
}
