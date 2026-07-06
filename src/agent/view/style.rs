use floem::peniko::Color;
use floem::style::Style;

use crate::ui::fonts::font_family;
use crate::ui::theme;

use super::transcript::{is_error_output, ToolStatus, TranscriptTone};

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

/// Whether a block's header, rather than always showing its body, defaults
/// to collapsed and toggles on click -- `Thinking` (pre-slice-1 behavior)
/// and `Tool` (`docs/agent-output-ui-design.md` decision 2: "collapsed is
/// the default for every tool state including errors").
pub(super) fn is_collapsible(tone: TranscriptTone) -> bool {
    matches!(tone, TranscriptTone::Thinking | TranscriptTone::Tool)
}

/// The shared header-row chrome (background/border/padding/font from
/// `block_colors`/`block_label_size`, plus the "merge visually into the
/// body below" border-bottom removal for collapsible tones once expanded)
/// -- built once here so the plain-label header (`mod.rs`) and the tool
/// header (`tool_view::tool_header_view`) can't drift apart on anything but
/// their text color, which each applies on top of this.
pub(super) fn header_row_style(s: Style, tone: TranscriptTone, expanded: bool) -> Style {
    let (background, border) = block_colors(tone);
    let s = s
        .width_full()
        .min_height(28)
        .items_center()
        .padding_horiz(10)
        .padding_vert(5)
        .font_family(font_family().to_string())
        .font_size(block_label_size(tone))
        .line_height(1.35)
        .background(background)
        .border(1.0)
        .border_color(border);

    if expanded && is_collapsible(tone) {
        s.border_bottom(0.0)
    } else {
        s
    }
}

/// A tool block header's text color, driven by its live status rather than
/// its (constant) `Tool` tone -- pending/preparing reads as subtle, running
/// as the accent color, finished as muted, and a failed result as danger
/// (`docs/agent-output-ui-design.md` decision 2).
pub(super) fn tool_status_color(status: &ToolStatus) -> Color {
    match status {
        ToolStatus::Preparing { .. } | ToolStatus::Requested => theme::text_subtle(),
        ToolStatus::Started => theme::accent(),
        ToolStatus::Finished { output } if is_error_output(output) => theme::danger(),
        ToolStatus::Finished { .. } => theme::text_muted(),
    }
}
