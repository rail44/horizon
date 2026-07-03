use super::transcript::{TranscriptBlock, TranscriptTone};

pub(super) fn block_label(block: &TranscriptBlock) -> String {
    if block.tone == TranscriptTone::Thinking {
        return "thinking".to_string();
    }

    match block.label {
        Some(label) => label.to_string(),
        None => String::new(),
    }
}

pub(super) fn shows_label(tone: TranscriptTone) -> bool {
    !matches!(tone, TranscriptTone::User | TranscriptTone::Assistant)
}
