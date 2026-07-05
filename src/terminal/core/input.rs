use alacritty_terminal::term::TermMode;
use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::{KeyCode, Modifiers};

use crate::terminal::types::{
    TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport,
};

/// Real Kitty `CSI u` encoding for the handful of `KeyCode`s that termwiz's
/// own `KeyCode::encode` cannot express correctly once Kitty progressive
/// enhancement flags are active.
///
/// termwiz 0.23.3 declares `KeyboardEncoding::Kitty(flags)` but never
/// actually matches on it inside `KeyCode::encode` — only
/// `KeyboardEncoding::CsiU` reaches genuine `CSI code;mods u` output in its
/// internal `csi_u_encode` helper. `TerminalCore::encode_modes` used to also
/// derive termwiz's unrelated `modify_other_keys` (an xterm extension
/// Horizon never negotiates) from Kitty's "disambiguate" bit, which sent
/// Enter/Tab/Backspace/Escape-with-a-modifier through `csi_u_encode`'s xterm
/// `modifyOtherKeys` fallback (`CSI 27;mods;codepoint~`) instead — a
/// well-formed but wrong sequence a Kitty-aware reader doesn't expect (a `~`
/// terminator where it expects `u`). That mismatch is exactly the kind of
/// thing that can wedge a client's own input parser, observed as all
/// further keystrokes going missing in Claude Code's TUI after a single
/// Shift+Enter. Rather than patch termwiz, we pre-encode this small,
/// well-defined set of keys ourselves per spec and bypass
/// `KeyCode::encode` for them entirely when Kitty flags are active.
///
/// `None` means "not our concern, fall back to termwiz's own encoding" —
/// covers `flags.is_empty()` (no Kitty protocol negotiated) and keys this
/// function doesn't special-case (arrows, Home/End, PageUp/PageDown, Delete
/// already emit spec-compatible sequences from termwiz, since Kitty reuses
/// xterm's `CSI 1;mods<letter>` / `CSI n;mods~` conventions for those).
///
/// See <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>: "Disambiguate
/// escape codes" for the Enter/Tab/Backspace exception ("The only
/// exceptions are the Enter, Tab and Backspace keys which still generate
/// the same bytes as in legacy mode") and Esc's promotion ("Turning on this
/// flag will cause the terminal to report the Esc ... keys using CSI u"),
/// and "Report all keys as escape codes" for why every key — including
/// Enter/Tab/Backspace, modified or not — gets `CSI u` once that flag is
/// set ("Note that all keys are reported as escape codes, including Enter,
/// Tab, Backspace etc.").
pub(super) fn kitty_override(
    key: KeyCode,
    mods: Modifiers,
    flags: KittyKeyboardFlags,
) -> Option<Vec<u8>> {
    if flags.is_empty() {
        return None;
    }

    let codepoint = match key {
        KeyCode::Enter => 13,
        KeyCode::Tab => 9,
        KeyCode::Backspace => 127,
        KeyCode::Escape => 27,
        _ => return None,
    };

    let report_all_keys = flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let promote = match key {
        // Esc is promoted by the disambiguate flag alone; Enter/Tab/
        // Backspace are the spec's named exceptions and only start being
        // reported as `CSI u` once *every* key is (report-all-keys).
        KeyCode::Escape => {
            report_all_keys || flags.contains(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES)
        }
        _ => report_all_keys,
    };
    if !promote {
        return None;
    }

    let mod_value = 1u32 + u32::from(mods.encode_xterm());
    let mut sequence = format!("\x1b[{codepoint}");
    if mod_value != 1 {
        sequence.push_str(&format!(";{mod_value}"));
    }
    sequence.push('u');
    Some(sequence.into_bytes())
}

pub(super) fn kitty_flags_from_mode(mode: TermMode) -> KittyKeyboardFlags {
    let mut flags = KittyKeyboardFlags::NONE;

    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        flags |= KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES;
    }
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        flags |= KittyKeyboardFlags::REPORT_EVENT_TYPES;
    }
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        flags |= KittyKeyboardFlags::REPORT_ALTERNATE_KEYS;
    }
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        flags |= KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    }
    if mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
        flags |= KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT;
    }

    flags
}

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
