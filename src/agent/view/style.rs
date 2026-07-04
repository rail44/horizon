use floem::peniko::Color;

use super::transcript::TranscriptTone;

pub(super) fn block_label_size(tone: TranscriptTone) -> f32 {
    match tone {
        TranscriptTone::User => 12.0,
        TranscriptTone::Assistant => 12.0,
        TranscriptTone::Thinking => 12.0,
        TranscriptTone::Status => 12.0,
        _ => 11.0,
    }
}

pub(super) fn block_max_width(tone: TranscriptTone) -> f32 {
    match tone {
        TranscriptTone::User => 620.0,
        TranscriptTone::Assistant => 1120.0,
        _ => 1200.0,
    }
}

pub(super) fn block_text_color(tone: TranscriptTone) -> Color {
    match tone {
        TranscriptTone::Status => Color::from_rgb8(166, 174, 188),
        TranscriptTone::Thinking => Color::from_rgb8(172, 178, 190),
        TranscriptTone::Tool | TranscriptTone::Approval => Color::from_rgb8(214, 221, 232),
        TranscriptTone::Error => Color::from_rgb8(255, 174, 178),
        _ => Color::from_rgb8(235, 238, 244),
    }
}

pub(super) fn block_colors(tone: TranscriptTone) -> (Color, Color) {
    match tone {
        TranscriptTone::User => (Color::from_rgb8(30, 43, 63), Color::from_rgb8(65, 94, 133)),
        TranscriptTone::Assistant => (Color::from_rgb8(29, 33, 40), Color::from_rgb8(48, 56, 68)),
        TranscriptTone::Thinking => (Color::from_rgb8(23, 26, 31), Color::from_rgb8(43, 48, 57)),
        TranscriptTone::Status => (Color::from_rgb8(25, 30, 37), Color::from_rgb8(47, 56, 68)),
        TranscriptTone::Tool => (Color::from_rgb8(23, 32, 34), Color::from_rgb8(42, 66, 66)),
        TranscriptTone::Approval => (Color::from_rgb8(38, 34, 26), Color::from_rgb8(78, 66, 44)),
        TranscriptTone::Error => (Color::from_rgb8(42, 28, 32), Color::from_rgb8(88, 52, 58)),
        TranscriptTone::Lifecycle => (Color::from_rgb8(28, 32, 39), Color::from_rgb8(42, 48, 58)),
    }
}
