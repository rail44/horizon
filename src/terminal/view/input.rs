use crate::terminal::{TerminalMouseButton, TerminalMouseModifiers, TerminalSelectionPoint};
use floem::{peniko::kurbo::Point, pointer::PointerButton};

use super::metrics::TerminalMetrics;
use super::{PADDING_X, PADDING_Y};

pub(super) fn cell_from_point(point: Point, metrics: TerminalMetrics) -> TerminalSelectionPoint {
    let col = ((point.x - PADDING_X) / metrics.cell_width)
        .max(0.0)
        .floor() as usize;
    let row = ((point.y - PADDING_Y) / metrics.line_height)
        .max(0.0)
        .floor() as usize;
    TerminalSelectionPoint { row, col }
}

pub(super) fn scroll_lines_from_wheel(delta_y: f64) -> Option<i32> {
    if delta_y.abs() < f64::EPSILON {
        return None;
    }

    Some(if delta_y > 0.0 { -3 } else { 3 })
}

pub(super) fn terminal_mouse_button(button: PointerButton) -> Option<TerminalMouseButton> {
    if button.is_primary() {
        Some(TerminalMouseButton::Left)
    } else if button.is_auxiliary() {
        Some(TerminalMouseButton::Middle)
    } else if button.is_secondary() {
        Some(TerminalMouseButton::Right)
    } else {
        None
    }
}

pub(super) fn terminal_mouse_modifiers(
    modifiers: floem::keyboard::Modifiers,
) -> TerminalMouseModifiers {
    TerminalMouseModifiers {
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        control: modifiers.control(),
    }
}
