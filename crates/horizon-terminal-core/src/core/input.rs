use crate::types::{
    TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport,
};

/// Kitty keyboard protocol handling (flag interpretation, key overrides,
/// modifier mapping) lives in `terminal::protocol::kitty_keyboard` — see
/// that module's doc for why. Only non-keyboard-protocol input helpers
/// (mouse reporting, scroll-as-arrow-keys) remain here.
pub(super) fn arrow_scroll_input(lines: i32) -> Vec<u8> {
    let sequence = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
    let repeat = lines.unsigned_abs().max(1) as usize;
    let mut input = Vec::with_capacity(sequence.len() * repeat);
    for _ in 0..repeat {
        input.extend_from_slice(sequence);
    }
    input
}

pub(super) fn sgr_mouse_wheel_input(lines: i32, col: usize, row: usize) -> Vec<u8> {
    let button = if lines > 0 { 64 } else { 65 };
    let repeat = lines.unsigned_abs().max(1) as usize;
    let mut input = Vec::new();
    for _ in 0..repeat {
        input.extend_from_slice(format!("\x1b[<{button};{col};{row}M").as_bytes());
    }
    input
}

pub(super) fn sgr_mouse_input(report: TerminalMouseReport) -> Vec<u8> {
    let button = match report.kind {
        TerminalMouseKind::Release => 3,
        TerminalMouseKind::Press | TerminalMouseKind::Drag => {
            let mut code = match report.button {
                TerminalMouseButton::Left => 0,
                TerminalMouseButton::Middle => 1,
                TerminalMouseButton::Right => 2,
            };
            if matches!(report.kind, TerminalMouseKind::Drag) {
                code += 32;
            }
            code + mouse_modifier_code(report.modifiers)
        }
    };
    let col = report.point.col.saturating_add(1);
    let row = report.point.row.saturating_add(1);
    let suffix = if matches!(report.kind, TerminalMouseKind::Release) {
        'm'
    } else {
        'M'
    };

    format!("\x1b[<{button};{col};{row}{suffix}").into_bytes()
}

fn mouse_modifier_code(modifiers: TerminalMouseModifiers) -> u8 {
    let mut code = 0;
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    code
}
