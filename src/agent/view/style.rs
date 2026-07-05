use floem::peniko::Color;

use crate::ui::theme;

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
        TranscriptTone::Status => theme::text_muted(),
        TranscriptTone::Thinking => theme::text_muted(),
        TranscriptTone::Tool | TranscriptTone::Approval => theme::text_primary(),
        TranscriptTone::Error => theme::danger(),
        _ => theme::text_primary(),
    }
}

pub(super) fn block_colors(tone: TranscriptTone) -> (Color, Color) {
    match tone {
        TranscriptTone::User => (theme::surface_raised(), theme::border_default()),
        TranscriptTone::Assistant => (theme::surface_raised(), theme::border_default()),
        TranscriptTone::Thinking => (theme::surface_panel(), theme::border_subtle()),
        TranscriptTone::Status => (theme::surface_chrome(), theme::border_default()),
        TranscriptTone::Tool => (theme::surface_raised(), theme::border_default()),
        TranscriptTone::Approval => (theme::surface_raised(), theme::accent()),
        TranscriptTone::Error => (theme::surface_raised(), theme::danger()),
        TranscriptTone::Lifecycle => (theme::surface_raised(), theme::border_subtle()),
    }
}
