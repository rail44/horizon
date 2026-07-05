//! Horizon's own Kitty keyboard protocol encoder.
//!
//! Horizon pins termwiz 0.23.3, which declares
//! `KeyboardEncoding::Kitty(flags)` as a variant of its `KeyCodeEncodeModes`
//! but — read `KeyCode::encode` in the vendored source — never actually
//! matches on it. `modes.encoding` is consulted in exactly two places in
//! that function, and both are gated on `KeyboardEncoding::CsiU`, a variant
//! Horizon never constructs. So termwiz's own "Kitty support" is dead code
//! from any caller that only ever builds `Xterm` or `Kitty(flags)`: the two
//! produce byte-identical output for every key. That's how the "Shift+Enter
//! kills all subsequent terminal input" bug shipped — Horizon believed it
//! was emitting genuine Kitty `CSI u` and was actually emitting xterm's
//! `modifyOtherKeys` form (`CSI 27;mods;codepoint~`), which a Kitty-aware
//! reader like Claude Code's own TUI parser doesn't expect and can wedge on.
//!
//! Patching termwiz key-by-key as more gaps surface doesn't scale and keeps
//! Horizon's actual compliance story implicit, scattered across whichever
//! override happens to exist. This module owns the protocol outright
//! instead: whenever the terminal has any Kitty progressive-enhancement
//! flag active, `encode` is the *only* thing that decides what bytes a key
//! produces — `terminal::core::TerminalCore::encode_key` no longer falls
//! through to termwiz in that state at all. termwiz's `KeyCode::encode`
//! remains in use solely for the legacy path (no Kitty flag negotiated,
//! `flags.is_empty()`), where its `Xterm` output is exactly what Horizon
//! wants.
//!
//! `KITTY_COMPLIANCE` below is the resident conformance table: one entry per
//! protocol feature crossed with the key class it governs, each naming the
//! test(s) that hold it to account. `cargo test print_compliance_matrix --
//! -- --nocapture` prints it.

use alacritty_terminal::term::TermMode;
use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::{KeyCode, Modifiers, CSI, SS3};

/// Read the terminal's negotiated Kitty progressive-enhancement flags off
/// its live `TermMode` (set by `CSI > flags u` / `CSI = flags ; mode u`,
/// handled upstream in `alacritty_terminal::Term`).
pub(crate) fn flags_from_mode(mode: TermMode) -> KittyKeyboardFlags {
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

/// Encode a key-down event while at least one Kitty flag is active (callers
/// must check `flags.is_empty()` themselves and use termwiz's own encoder
/// for the legacy case — see the module doc). `application_cursor_keys`
/// (DECCKM) and `newline_mode` (LNM) are threaded through from the
/// terminal's live `TermMode` because they still affect a handful of
/// keys' *legacy* byte forms even while Kitty flags are active — the Kitty
/// spec doesn't touch either mode, and Horizon's terminal doesn't stop
/// tracking them just because Kitty is also negotiated.
///
/// Key-up events never reach here: `TerminalCore::encode_key` returns empty
/// bytes for `is_down == false` before consulting Kitty state at all (see
/// `KITTY_COMPLIANCE`'s "Report event types" row for why release events
/// aren't supported regardless).
pub(crate) fn encode(
    key: KeyCode,
    mods: Modifiers,
    flags: KittyKeyboardFlags,
    application_cursor_keys: bool,
    newline_mode: bool,
) -> Vec<u8> {
    if let Some(bytes) = kitty_override(key, mods, flags) {
        return bytes;
    }

    legacy_bytes(key, mods, application_cursor_keys, newline_mode).into_bytes()
}

/// Real Kitty `CSI u` encoding for the handful of `KeyCode`s that termwiz's
/// own encoder cannot express correctly once Kitty flags are active (see the
/// module doc for why termwiz's `Kitty(flags)` output is unusable in the
/// first place). `None` means "not one of those keys, or promotion doesn't
/// apply this key press" — the caller falls back to `legacy_bytes`, which
/// covers both "no override exists for this key at all" (arrows, Home/End,
/// etc. — already spec-compatible via their shared xterm/Kitty `CSI`
/// conventions) and "this is Enter/Tab/Backspace/Escape but the active
/// flags don't promote it" (bare Enter/Tab/Backspace before
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES`, or any of the four when no flag
/// requires promotion at all, e.g. `REPORT_EVENT_TYPES` alone).
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
///
/// One deliberate, documented deviation from that spec text: the
/// unmodified-only reading. The spec's own exception text has no modifier
/// carve-out — literally, Shift+Enter should stay `\r` under disambiguate
/// alone, same as bare Enter — and its stated rationale is narrow: keep
/// `reset<Enter>` typeable after a crashed program leaves the mode on.
/// That rationale only needs the *bare* key preserved. We instead promote
/// Enter/Tab/Backspace to `CSI u` under disambiguate alone whenever a
/// modifier is held, verified against a real client rather than the text
/// alone: capturing `claude` (Claude Code 2.1.201)'s own startup negotiation
/// shows it pushes only `CSI>1u` (disambiguate, nothing else), and replaying
/// both `\x1b[13;2u` (Kitty CSI u) and the older `\x1b[27;2;13~` (xterm
/// `modifyOtherKeys`) back into a live session through Horizon's own
/// `TerminalCore` renders a correctly-inserted second input line for either
/// — not the "wedges the parser" failure the strict, unconditional
/// legacy-bytes reading was chosen to avoid. Bare Enter/Tab/Backspace still
/// fall through to legacy bytes here, so the crash-recovery case is
/// unaffected. See `KITTY_COMPLIANCE`'s "Enter/Tab/Backspace exception" row.
fn kitty_override(key: KeyCode, mods: Modifiers, flags: KittyKeyboardFlags) -> Option<Vec<u8>> {
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
    let disambiguate = flags.contains(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES);
    let promote = match key {
        // Esc has no legacy-mode exception in the spec: disambiguate alone
        // promotes it, modified or not.
        KeyCode::Escape => report_all_keys || disambiguate,
        // Enter/Tab/Backspace: promoted once *any* modifier is held (see
        // the deviation note above); the bare key stays legacy until
        // report-all-keys, preserving `reset<Enter>` recovery.
        _ => report_all_keys || (disambiguate && !mods.is_empty()),
    };
    if !promote {
        return None;
    }

    // `Modifiers::encode_xterm()` (termwiz, via `wezterm-input-types`) only
    // ever encodes shift(1)/alt(2)/ctrl(4) — it silently drops `SUPER` even
    // though `app::keymap::termwiz_modifiers` does carry it through from
    // the OS's Cmd/Win key. The Kitty spec reserves bit `0b1000` (8) for
    // super in this same field, so add it back for the keys we encode
    // ourselves; `Modifiers` has no distinct hyper/true-meta/caps_lock/
    // num_lock bits at all (`ALT` doubles as "meta" in this crate), so
    // those spec bits (16/32/64/128) are unreachable here — see
    // `KITTY_COMPLIANCE`'s "Extended modifier bits" row.
    let mut mod_bits = u32::from(mods.encode_xterm());
    if mods.contains(Modifiers::SUPER) {
        mod_bits |= 0b1000;
    }
    let mod_value = 1u32 + mod_bits;
    let mut sequence = format!("\x1b[{codepoint}");
    if mod_value != 1 {
        sequence.push_str(&format!(";{mod_value}"));
    }
    sequence.push('u');
    Some(sequence.into_bytes())
}

/// Every key `kitty_override` doesn't (or, for
/// Enter/Tab/Backspace/Escape, doesn't *this time*) promote to genuine
/// Kitty `CSI u`, encoded the same way termwiz 0.23.3's `KeyCode::encode`
/// would — faithfully ported rather than delegated to, per the module doc's
/// "own it outright" boundary. This is a safe port, not a guess: because
/// termwiz's `Kitty(flags)` and `Xterm` encodings are behaviorally
/// identical in that version (see module doc), this function reproduces
/// termwiz's `Xterm`-mode output exactly (dropping only the dead
/// `KeyboardEncoding::CsiU` and `modify_other_keys` branches, which
/// Horizon's terminal never negotiates and so can never take).
fn legacy_bytes(
    key: KeyCode,
    mods: Modifiers,
    application_cursor_keys: bool,
    newline_mode: bool,
) -> String {
    match key {
        KeyCode::Char(c) => encode_char(c, mods),

        KeyCode::Backspace => {
            let c = if mods.contains(Modifiers::CTRL) {
                '\x08'
            } else {
                '\x7f'
            };
            let mut out = String::new();
            if mods.contains(Modifiers::ALT) {
                out.push('\x1b');
            }
            out.push(c);
            out
        }

        KeyCode::Enter | KeyCode::Escape => {
            let is_enter = matches!(key, KeyCode::Enter);
            let c = if is_enter { '\r' } else { '\x1b' };
            let mut out = String::new();
            if mods.contains(Modifiers::ALT) {
                out.push('\x1b');
            }
            out.push(c);
            if newline_mode
                && is_enter
                && !mods.contains(Modifiers::SHIFT)
                && !mods.contains(Modifiers::CTRL)
            {
                out.push('\n');
            }
            out
        }

        KeyCode::Tab => {
            let mut out = String::new();
            if mods.contains(Modifiers::ALT) {
                out.push('\x1b');
            }
            let mods = mods - Modifiers::ALT;
            if mods == Modifiers::CTRL {
                out.push_str("\x1b[9;5u");
            } else if mods == Modifiers::CTRL | Modifiers::SHIFT {
                out.push_str("\x1b[1;5Z");
            } else if mods == Modifiers::SHIFT {
                out.push_str("\x1b[Z");
            } else {
                out.push('\t');
            }
            out
        }

        KeyCode::Home
        | KeyCode::KeyPadHome
        | KeyCode::End
        | KeyCode::KeyPadEnd
        | KeyCode::UpArrow
        | KeyCode::DownArrow
        | KeyCode::RightArrow
        | KeyCode::LeftArrow
        | KeyCode::ApplicationUpArrow
        | KeyCode::ApplicationDownArrow
        | KeyCode::ApplicationRightArrow
        | KeyCode::ApplicationLeftArrow => {
            let (force_app, c) = match key {
                KeyCode::UpArrow => (false, 'A'),
                KeyCode::DownArrow => (false, 'B'),
                KeyCode::RightArrow => (false, 'C'),
                KeyCode::LeftArrow => (false, 'D'),
                KeyCode::KeyPadHome | KeyCode::Home => (false, 'H'),
                KeyCode::End | KeyCode::KeyPadEnd => (false, 'F'),
                KeyCode::ApplicationUpArrow => (true, 'A'),
                KeyCode::ApplicationDownArrow => (true, 'B'),
                KeyCode::ApplicationRightArrow => (true, 'C'),
                KeyCode::ApplicationLeftArrow => (true, 'D'),
                _ => unreachable!(),
            };
            let csi_or_ss3 = if force_app || application_cursor_keys {
                SS3
            } else {
                CSI
            };
            if mods.contains(Modifiers::ALT)
                || mods.contains(Modifiers::SHIFT)
                || mods.contains(Modifiers::CTRL)
            {
                format!("{CSI}1;{}{c}", 1 + mods.encode_xterm())
            } else {
                format!("{csi_or_ss3}{c}")
            }
        }

        KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::KeyPadPageUp
        | KeyCode::KeyPadPageDown
        | KeyCode::Insert
        | KeyCode::Delete => {
            let n = match key {
                KeyCode::Insert => 2,
                KeyCode::Delete => 3,
                KeyCode::KeyPadPageUp | KeyCode::PageUp => 5,
                KeyCode::KeyPadPageDown | KeyCode::PageDown => 6,
                _ => unreachable!(),
            };
            if mods.contains(Modifiers::ALT)
                || mods.contains(Modifiers::SHIFT)
                || mods.contains(Modifiers::CTRL)
            {
                format!("\x1b[{n};{}~", 1 + mods.encode_xterm())
            } else {
                format!("\x1b[{n}~")
            }
        }

        KeyCode::Function(n) => encode_function_key(n, mods),

        KeyCode::Numpad0 | KeyCode::Numpad3 | KeyCode::Numpad9 | KeyCode::Decimal => {
            let intro = match key {
                KeyCode::Numpad0 => "\x1b[2",
                KeyCode::Numpad3 | KeyCode::Numpad9 => "\x1b[6",
                KeyCode::Decimal => "\x1b[3",
                _ => unreachable!(),
            };
            let encoded_mods = mods.encode_xterm();
            if encoded_mods == 0 {
                format!("{intro}~")
            } else {
                format!("{intro};{}~", 1 + encoded_mods)
            }
        }

        KeyCode::Numpad1
        | KeyCode::Numpad2
        | KeyCode::Numpad4
        | KeyCode::Numpad5
        | KeyCode::KeyPadBegin
        | KeyCode::Numpad6
        | KeyCode::Numpad7
        | KeyCode::Numpad8 => {
            let c = match key {
                KeyCode::Numpad1 => "F",
                KeyCode::Numpad2 => "B",
                KeyCode::Numpad4 => "D",
                KeyCode::KeyPadBegin | KeyCode::Numpad5 => "E",
                KeyCode::Numpad6 => "C",
                KeyCode::Numpad7 => "H",
                KeyCode::Numpad8 => "A",
                _ => unreachable!(),
            };
            let encoded_mods = mods.encode_xterm();
            if encoded_mods == 0 {
                format!("{CSI}{c}")
            } else {
                format!("{CSI}1;{}{c}", 1 + encoded_mods)
            }
        }

        // Everything else (bare modifier keys, media keys, arithmetic
        // keypad operators, ...): termwiz's own encoder expands none of
        // these to anything regardless of encoding mode, and
        // `app::keymap` never constructs most of them from a real key
        // event in the first place. See `KITTY_COMPLIANCE`'s
        // "Standalone modifier keys" row.
        _ => String::new(),
    }
}

/// `KeyCode::Char` handling, split out of `legacy_bytes` for readability.
/// Mirrors termwiz's own `Char` handling exactly (shift-to-uppercase
/// normalization first, then Ctrl/Alt-aware byte selection) — see
/// `KITTY_COMPLIANCE`'s "Report all keys as escape codes (text keys)" row
/// for the known, deliberately-unfixed gap this still leaves: a modified
/// Char here never becomes genuine `CSI u`, matching termwiz's
/// `Kitty(flags)`-is-really-`Xterm` behavior this whole module otherwise
/// replaces.
fn encode_char(c: char, mods: Modifiers) -> String {
    let c = if mods.contains(Modifiers::SHIFT) && c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    };
    let mods = if (c.is_ascii_punctuation() || c.is_ascii_uppercase())
        && mods.contains(Modifiers::SHIFT)
    {
        mods & !Modifiers::SHIFT
    } else {
        mods
    };

    if mods.contains(Modifiers::CTRL) {
        if let Some(mapped) = ctrl_mapping(c) {
            let mut out = String::new();
            if mods.contains(Modifiers::ALT) {
                out.push('\x1b');
            }
            out.push(mapped);
            return out;
        }
    }
    if (c.is_ascii_alphanumeric() || c.is_ascii_punctuation()) && mods.contains(Modifiers::ALT) {
        let mut out = String::new();
        out.push('\x1b');
        out.push(c);
        return out;
    }

    let mut out = String::new();
    if mods.contains(Modifiers::ALT) {
        out.push('\x1b');
    }
    out.push(c);
    out
}

/// `KeyCode::Function(n)` handling, split out of `legacy_bytes` for
/// readability. Ported from termwiz's own table: F1-F4 use SS3 when
/// unmodified and CSI when modified, F5-F24 reuse legacy rxvt-style `CSI
/// n~` numbers (which the Kitty spec's own "Functional key definitions"
/// table also documents as F1-F12's alternate numeric forms — see
/// `KITTY_COMPLIANCE`'s "F13-F35" row for where this stops being spec-legal:
/// termwiz's F13-F24 numbers there are legacy rxvt numbers, not Kitty's PUA
/// codes, and F25+ has no representation at all).
fn encode_function_key(n: u8, mods: Modifiers) -> String {
    if mods.is_empty() && n < 5 {
        return match n {
            1 => "\x1bOP",
            2 => "\x1bOQ",
            3 => "\x1bOR",
            4 => "\x1bOS",
            _ => unreachable!(),
        }
        .to_string();
    }
    if n < 5 {
        let code = match n {
            1 => 'P',
            2 => 'Q',
            3 => 'R',
            4 => 'S',
            _ => unreachable!(),
        };
        return format!("\x1b[1;{}{code}", 1 + mods.encode_xterm());
    }

    let Some(intro) = (match n {
        5 => Some("\x1b[15"),
        6 => Some("\x1b[17"),
        7 => Some("\x1b[18"),
        8 => Some("\x1b[19"),
        9 => Some("\x1b[20"),
        10 => Some("\x1b[21"),
        11 => Some("\x1b[23"),
        12 => Some("\x1b[24"),
        13 => Some("\x1b[25"),
        14 => Some("\x1b[26"),
        15 => Some("\x1b[28"),
        16 => Some("\x1b[29"),
        17 => Some("\x1b[31"),
        18 => Some("\x1b[32"),
        19 => Some("\x1b[33"),
        20 => Some("\x1b[34"),
        21 => Some("\x1b[42"),
        22 => Some("\x1b[43"),
        23 => Some("\x1b[44"),
        24 => Some("\x1b[45"),
        _ => None,
    }) else {
        // F25+: no representation in termwiz's table at all (its `bail!`
        // becomes an empty string once `TerminalCore::encode_key` unwraps
        // the `Result` with `.unwrap_or_default()`).
        return String::new();
    };
    let encoded_mods = mods.encode_xterm();
    if encoded_mods == 0 {
        format!("{intro}~")
    } else {
        format!("{intro};{}~", 1 + encoded_mods)
    }
}

/// Ported from `wezterm_input_types::ctrl_mapping` (termwiz's own
/// dependency for this table; not part of termwiz's public API, so it
/// can't be called directly without depending on that crate ourselves for
/// one function). Maps a character to the byte it produces when Ctrl is
/// held, per xterm's legacy (X11-inherited) Ctrl translation.
fn ctrl_mapping(c: char) -> Option<char> {
    Some(match c {
        '@' | '`' | ' ' | '2' => '\x00',
        'A' | 'a' => '\x01',
        'B' | 'b' => '\x02',
        'C' | 'c' => '\x03',
        'D' | 'd' => '\x04',
        'E' | 'e' => '\x05',
        'F' | 'f' => '\x06',
        'G' | 'g' => '\x07',
        'H' | 'h' => '\x08',
        'I' | 'i' => '\x09',
        'J' | 'j' => '\x0a',
        'K' | 'k' => '\x0b',
        'L' | 'l' => '\x0c',
        'M' | 'm' => '\x0d',
        'N' | 'n' => '\x0e',
        'O' | 'o' => '\x0f',
        'P' | 'p' => '\x10',
        'Q' | 'q' => '\x11',
        'R' | 'r' => '\x12',
        'S' | 's' => '\x13',
        'T' | 't' => '\x14',
        'U' | 'u' => '\x15',
        'V' | 'v' => '\x16',
        'W' | 'w' => '\x17',
        'X' | 'x' => '\x18',
        'Y' | 'y' => '\x19',
        'Z' | 'z' => '\x1a',
        '[' | '3' | '{' => '\x1b',
        '\\' | '4' | '|' => '\x1c',
        ']' | '5' | '}' => '\x1d',
        '^' | '6' | '~' => '\x1e',
        '_' | '7' | '/' => '\x1f',
        '8' | '?' => '\x7f',
        _ => return None,
    })
}

/// A verdict for one `FeatureEntry` cell of `KITTY_COMPLIANCE`.
///
/// `KITTY_COMPLIANCE` and its supporting types are `#[cfg(test)]`-only: the
/// table's entire purpose is to be checked against, and printed from, the
/// tests in `kitty_keyboard::tests` (see `compliance_table_tests_are_registered_and_correctly_flagged`
/// and `print_compliance_matrix`), so gating it keeps it "resident" — a
/// first-class, always-compiled-with-the-tests part of this module — without
/// carrying dead-code weight into release builds.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum Verdict {
    /// Matches the spec; the entry's named test(s) assert the exact bytes.
    Compliant,
    /// Deliberately diverges from the spec text, for the given reason.
    Deviation(&'static str),
    /// Not implemented; explains the blocker and, where estimated, the
    /// effort to close it.
    Unimplemented(&'static str),
    /// The real UI never drives this code path at all; names what bypasses
    /// it.
    Bypassed(&'static str),
}

/// One cell of the Kitty keyboard protocol conformance matrix: a specific
/// protocol feature crossed with the key class it governs.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct FeatureEntry {
    pub(crate) feature: &'static str,
    pub(crate) key_class: &'static str,
    pub(crate) verdict: Verdict,
    /// Names of the `#[test]` function(s) (in `terminal::tests` unless
    /// otherwise noted) that hold this cell to account. Checked against
    /// `tests::TEST_REGISTRY` — see that module for the mechanism.
    pub(crate) tests: &'static [&'static str],
}

/// The resident Kitty keyboard protocol compliance table. `cargo test
/// print_compliance_matrix -- --nocapture` prints it as a report.
#[cfg(test)]
pub(crate) const KITTY_COMPLIANCE: &[FeatureEntry] = &[
    FeatureEntry {
        feature: "Disambiguate escape codes: Enter/Tab/Backspace exception",
        key_class: "Enter, Tab, Backspace",
        verdict: Verdict::Deviation(
            "promotes modified Enter/Tab/Backspace to CSI u under disambiguate alone, not just \
             under report-all-keys as the spec's exception text literally reads — verified \
             against Claude Code 2.1.201's real negotiation; see kitty_override's doc comment",
        ),
        tests: &["kitty_csi_u_truth_table"],
    },
    FeatureEntry {
        feature: "Disambiguate escape codes: Esc promotion",
        key_class: "Escape",
        verdict: Verdict::Compliant,
        tests: &["kitty_csi_u_truth_table"],
    },
    FeatureEntry {
        feature: "Report all keys as escape codes",
        key_class: "Enter, Tab, Backspace, Escape",
        verdict: Verdict::Compliant,
        tests: &["kitty_csi_u_truth_table"],
    },
    FeatureEntry {
        feature: "Super modifier bit (0b1000)",
        key_class: "Enter, Tab, Backspace, Escape",
        verdict: Verdict::Compliant,
        tests: &["kitty_override_reports_super_modifier"],
    },
    FeatureEntry {
        feature: "Extended modifier bits (hyper, true-meta, caps_lock, num_lock)",
        key_class: "all keys",
        verdict: Verdict::Unimplemented(
            "termwiz's Modifiers type (from wezterm-input-types) has no distinct bits for these \
             — ALT doubles as \"meta\" and there is no hyper/caps_lock/num_lock bit at all, so \
             the spec's bits 16/32/64/128 are structurally unreachable without a new modifiers \
             type; no test is possible without one",
        ),
        tests: &[],
    },
    FeatureEntry {
        feature: "Functional key definitions: navigation keys",
        key_class: "arrows, Home, End, PageUp, PageDown, Delete",
        verdict: Verdict::Compliant,
        tests: &["navigation_keys_are_flag_invariant_and_spec_compliant"],
    },
    FeatureEntry {
        feature: "Report event types (key release)",
        key_class: "all keys",
        verdict: Verdict::Unimplemented(
            "termwiz's KeyCode::encode hardcodes empty output for is_down == false before it \
             ever looks at the encoding mode, for every key without exception; \
             app::keymap::handle_terminal_key also hardcodes is_down: true on every key it \
             sends, so no release event is even constructed today. Effort: medium — needs a \
             from-scratch CSI u release-event encoder plus app-layer key-up wiring",
        ),
        tests: &["release_events_are_unimplemented_regardless_of_flags"],
    },
    FeatureEntry {
        feature: "Functional key definitions: F13-F35",
        key_class: "function keys (high)",
        verdict: Verdict::Unimplemented(
            "no PUA table exists for F13 and up: F13-F24 here reuse termwiz's legacy rxvt \
             numbers (spec-WRONG, not just missing), F25-F35 produce nothing at all. Also \
             BYPASSED at the app layer: app::keymap never maps any function key to a \
             TermKeyCode, so this path isn't reachable from the real UI regardless. Effort: \
             small for the PUA table itself, once the app-layer gap is separately closed",
        ),
        tests: &["very_high_function_keys_are_unimplemented"],
    },
    FeatureEntry {
        feature: "Functional key definitions: standalone modifier keypresses",
        key_class: "bare Shift, Ctrl, Alt, Super, ...",
        verdict: Verdict::Unimplemented(
            "termwiz's encoder puts every modifier KeyCode in its final catch-all arm \
             unconditionally. Also BYPASSED at the app layer: app::keymap never constructs a \
             modifier-only TermKeyCode from a bare keypress. Effort: medium — a PUA table plus \
             new app-layer wiring to recognize bare modifier KeyEvents at all",
        ),
        tests: &["standalone_modifier_keypresses_are_unimplemented"],
    },
    FeatureEntry {
        feature: "Legacy functional keys: keypad disambiguation",
        key_class: "keypad (Numpad0-9, Decimal, ...)",
        verdict: Verdict::Unimplemented(
            "the spec moves keypad keys to dedicated PUA codes once disambiguate is active; \
             this module's keypad handling (ported from termwiz) ignores that flag entirely, \
             same as termwiz did. Also BYPASSED at the app layer: app::keymap has no keypad \
             wiring. Effort: small-medium for a PUA table gated on disambiguate; the app-layer \
             gap is separate and larger",
        ),
        tests: &["keypad_keys_ignore_disambiguate_flag"],
    },
    FeatureEntry {
        feature: "Report all keys as escape codes (text keys)",
        key_class: "shifted letters / printable text",
        verdict: Verdict::Bypassed("app::keymap::character_input"),
        tests: &["shift_letter_ignores_kitty_flags_even_with_report_all_keys_active"],
    },
    FeatureEntry {
        feature: "Report alternate keys",
        key_class: "text keys",
        verdict: Verdict::Unimplemented(
            "the flag is tracked (flags_from_mode sets REPORT_ALTERNATE_KEYS from \
             TermMode::REPORT_ALTERNATE_KEYS) but no code path ever emits the alternate-key CSI \
             u subfield it requires; no test is possible without an implementation to test",
        ),
        tests: &[],
    },
    FeatureEntry {
        feature: "Report associated text",
        key_class: "text keys",
        verdict: Verdict::Unimplemented(
            "the flag is tracked (flags_from_mode sets REPORT_ASSOCIATED_TEXT from \
             TermMode::REPORT_ASSOCIATED_TEXT) but no code path ever emits the associated-text \
             CSI u subfield it requires; no test is possible without an implementation to test",
        ),
        tests: &[],
    },
];

#[cfg(test)]
mod tests;
