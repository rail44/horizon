use super::transcript::{TranscriptBlock, TranscriptTone};

pub(super) fn block_label(block: &TranscriptBlock, expanded: bool) -> String {
    if block.tone == TranscriptTone::Thinking {
        return if expanded {
            "thinking".to_string()
        } else {
            "thinking".to_string()
        };
    }

    match block.label {
        Some(label) => label.to_string(),
        None => String::new(),
    }
}

pub(super) fn shows_label(tone: TranscriptTone) -> bool {
    !matches!(tone, TranscriptTone::User | TranscriptTone::Assistant)
}
