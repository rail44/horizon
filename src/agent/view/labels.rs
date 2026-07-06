use super::transcript::{BlockKind, TranscriptTone};

/// The static header label for a `Text`-kind block -- `Tool`-kind blocks
/// never reach this: their header is the live, reactively re-derived
/// `tool_header::header_line`, built directly in `tool_view` (see that
/// module's doc comment for why a `Tool` block's header can't be a plain
/// captured string the way this one is).
pub(super) fn block_label(tone: TranscriptTone, kind: &BlockKind) -> String {
    if tone == TranscriptTone::Thinking {
        return "thinking".to_string();
    }

    match kind {
        BlockKind::Text { label, .. } => label.map(str::to_string).unwrap_or_default(),
        BlockKind::Tool(_) => String::new(),
    }
}

pub(super) fn shows_label(tone: TranscriptTone) -> bool {
    !matches!(tone, TranscriptTone::User | TranscriptTone::Assistant)
}
