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

use crate::types::KeyEventKind;

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

/// Encode a key event while at least one Kitty flag is active (callers must
/// check `flags.is_empty()` themselves and use termwiz's own encoder for the
/// legacy case — see the module doc). `application_cursor_keys` (DECCKM) and
/// `newline_mode` (LNM) are threaded through from the terminal's live
/// `TermMode` because they still affect a handful of keys' *legacy* byte
/// forms even while Kitty flags are active — the Kitty spec doesn't touch
/// either mode, and Horizon's terminal doesn't stop tracking them just
/// because Kitty is also negotiated.
///
/// `event` is only ever `Release` here when `REPORT_EVENT_TYPES` is active
/// (`TerminalCore::encode_key` filters out every other release before this
/// function is even called — see its doc comment). A release produces bytes
/// for the keys `kitty_override` promotes to genuine `CSI u` at these flags
/// (Enter/Tab/Backspace/Escape/F13-F24) and for the navigation keys
/// `navigation_key_event_override` decorates in place (arrows, Home/End,
/// PageUp/PageDown, Insert, Delete — see `KITTY_COMPLIANCE`'s "Report event
/// types" navigation row); every other key here — keypad, standalone
/// modifiers, F25+, ... — has no representation to fall back to and must
/// produce nothing, since re-emitting the press bytes would read as a
/// second press to any Kitty-aware client.
pub(crate) fn encode(
    key: KeyCode,
    mods: Modifiers,
    flags: KittyKeyboardFlags,
    event: KeyEventKind,
    text: Option<&str>,
    application_cursor_keys: bool,
    newline_mode: bool,
) -> Vec<u8> {
    if let Some(bytes) = kitty_override(key, mods, flags, event, text) {
        return bytes;
    }

    if let Some(bytes) = navigation_key_event_override(key, mods, flags, event, text) {
        return bytes;
    }

    if !event.is_down() {
        return Vec::new();
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
///
/// `event`'s only effect is the optional trailing event-type subfield (see
/// `event_type_subfield`): the *promotion* decision above (whether this key
/// reaches `CSI u` at all, at these flags) is identical for press, repeat
/// and release. That symmetry is deliberate, not just convenient: the
/// spec's own "Enter/Tab/Backspace won't have release events unless
/// report-all-keys" carve-out (see `KITTY_COMPLIANCE`'s "Report event
/// types" rows) falls out of it for free — a bare Enter/Tab/Backspace stays
/// unpromoted (so its release produces nothing, via `encode`'s fallback)
/// until report-all-keys promotes it, exactly like the spec says. Extending
/// that same promotion test to release/repeat for the *modified* case this
/// module already deviates on (previous paragraph) keeps a promoted press
/// from ever having an orphan release with no representation at all, which
/// re-deriving the spec's carve-out as its own separate condition here
/// would have produced.
///
/// F13-F24 are a second, unrelated class this function also promotes,
/// unconditionally once any Kitty flag is negotiated: unlike F1-F12 (whose
/// SS3/`CSI`-letter and `CSI n~` legacy numbers the Kitty spec's own
/// "Functional key definitions" table documents as legal alternate forms —
/// see `encode_function_key`), the spec has no legacy encoding for F13 and
/// up at all, only dedicated Private-Use-Area `CSI u` codepoints
/// (`57376`-`57398` for F13-F35; termwiz's own `KeyCode::Function` doc caps
/// out at F24, matching this module's own scope — see `KITTY_COMPLIANCE`'s
/// "Functional key definitions: F13-F24"/"F25-F35" rows). kitty's own
/// reference (`key_encoding.c`) always emits these PUA codes for F13+, even
/// with zero progressive-enhancement flags negotiated — the spec explicitly
/// permits terminals to choose otherwise for keys with no legacy form
/// ("terminals may instead choose to ignore such keys in legacy mode
/// instead, or have an option to control this behavior"), and this module
/// takes that option: with `flags.is_empty()` (the earlier check above),
/// F13-F24 still fall through to `legacy_bytes`' existing xterm/rxvt-style
/// numbers, preserving compatibility for old programs that never negotiate
/// Kitty at all. Once any flag is negotiated, though, promotion is
/// unconditional (no report-all-keys/disambiguate distinction the way
/// Enter/Tab/Backspace/Escape have) since there's no legacy form worth
/// preserving here in the first place.
fn kitty_override(
    key: KeyCode,
    mods: Modifiers,
    flags: KittyKeyboardFlags,
    event: KeyEventKind,
    text: Option<&str>,
) -> Option<Vec<u8>> {
    if flags.is_empty() {
        return None;
    }

    let codepoint = match key {
        KeyCode::Enter => 13,
        KeyCode::Tab => 9,
        KeyCode::Backspace => 127,
        KeyCode::Escape => 27,
        // F13-F24's dedicated Private-Use-Area codepoints (57376-57387) —
        // see this function's doc comment.
        KeyCode::Function(n) if (13..=24).contains(&n) => 57376 + u32::from(n - 13),
        _ => return None,
    };

    let report_all_keys = flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let disambiguate = flags.contains(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES);
    let promote = match key {
        // Esc has no legacy-mode exception in the spec: disambiguate alone
        // promotes it, modified or not.
        KeyCode::Escape => report_all_keys || disambiguate,
        // F13-F24 have no legacy form to preserve at all, so any negotiated
        // flag promotes them unconditionally — see this function's doc
        // comment.
        KeyCode::Function(n) if (13..=24).contains(&n) => true,
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
    let event_type = event_type_subfield(event, flags);
    let text = associated_text_subfield(text, flags, event);
    let mut sequence = format!("\x1b[{codepoint}");
    if mod_value != 1 || event_type.is_some() || text.is_some() {
        sequence.push_str(&format!(";{mod_value}"));
        if let Some(event_type) = event_type {
            sequence.push_str(&format!(":{event_type}"));
        }
        if let Some(text) = text {
            sequence.push(';');
            sequence.push_str(&text);
        }
    }
    sequence.push('u');
    Some(sequence.into_bytes())
}

/// The associated-text subfield: decimal codepoints, colon-separated, only
/// emitted when `REPORT_EVENT_TYPES` and `REPORT_ASSOCIATED_TEXT` are both
/// negotiated and the event is a press or repeat with non-empty, non-control
/// text. Returns `None` for any other case, so callers simply skip the field.
fn associated_text_subfield(
    text: Option<&str>,
    flags: KittyKeyboardFlags,
    event: KeyEventKind,
) -> Option<String> {
    let text = text?;
    if !event.is_down() {
        return None;
    }
    if !flags.contains(KittyKeyboardFlags::REPORT_EVENT_TYPES)
        || !flags.contains(KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT)
    {
        return None;
    }
    if text.is_empty() || text.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(
        text.chars()
            .map(|c| (c as u32).to_string())
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// The Kitty spec's "event-type" subfield value for `event`
/// (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>, "event types":
/// "The press event type has value 1 and is the default if no event type
/// sub field is present. The repeat type is 2 and the release type is 3"),
/// or `None` when it should be omitted from the sequence entirely — either
/// because `event` is a plain press (the implicit default) or because
/// `REPORT_EVENT_TYPES` isn't negotiated, in which case a repeat must stay
/// byte-for-byte indistinguishable from a press (a release never reaches
/// either `CSI u` builder that calls this without the flag active —
/// `TerminalCore::encode_key` filters it out first).
fn event_type_subfield(event: KeyEventKind, flags: KittyKeyboardFlags) -> Option<u8> {
    if !flags.contains(KittyKeyboardFlags::REPORT_EVENT_TYPES) {
        return None;
    }
    match event {
        // Skew catch-all: an unknown event kind encodes like a press (see
        // `KeyEventKind::Unknown` — dropping a press would lose input).
        KeyEventKind::Press | KeyEventKind::Unknown => None,
        KeyEventKind::Repeat => Some(2),
        KeyEventKind::Release => Some(3),
    }
}

/// Adds the Kitty "Report event types" modifier sub-field
/// (`event_type_subfield`) to the navigation keys' own Kitty-flag-invariant
/// legacy `CSI` forms (`legacy_bytes`'s Home/End/arrows and
/// PageUp/PageDown/Insert/Delete arms) — the one thing those forms were
/// missing relative to the genuine `CSI u` forms `kitty_override`/
/// `csi_u_text_key` already decorate.
///
/// This is `Compliant`, not a deviation, despite a narrower reading of the
/// spec text that takes the "central escape code" section's literal `CSI
/// ... u` framing to mean the event-type sub-field is `CSI u`-only: the
/// spec's own "Event types" section defines it generically as "a sub-field
/// of the modifiers field", and its "Legacy functional keys" section says
/// explicitly that these forms "encoded as described in the modifiers
/// section, above" — the same section event types are a part of. Verified
/// against both reference implementations the spec names as implementing
/// this protocol: kitty's own `key_encoding.c` (`serialize()` appends the
/// `mods:action` sub-field to every functional key uniformly, regardless of
/// whether the trailing byte is `u`, `~`, or a legacy CSI letter — there is
/// no special-casing for `CSI u` there at all) and alacritty's
/// `build_sequence` (same: `kitty_event_type` decorates the payload before
/// the terminator is even chosen). See `KITTY_COMPLIANCE`'s "Report event
/// types" navigation row.
///
/// `None` for every other key, or when there's nothing to add (a plain
/// press, or `REPORT_EVENT_TYPES` not negotiated) — `encode`'s existing
/// fallback (`legacy_bytes`, or "no representation" for an unpromoted
/// release) is already correct in both cases.
fn navigation_key_event_override(
    key: KeyCode,
    mods: Modifiers,
    flags: KittyKeyboardFlags,
    event: KeyEventKind,
    text: Option<&str>,
) -> Option<Vec<u8>> {
    let event_type = event_type_subfield(event, flags)?;
    let (intro, terminator) = navigation_key_form(key)?;

    // Once an event-type sub-field is present the modifiers field can't be
    // omitted either — spec: "If no modifiers are present, the modifiers
    // field must have the value 1 and the event type sub-field the type of
    // event" — which also means the `SS3` alternate form `legacy_bytes`
    // uses for an unmodified press in application-cursor-keys mode can't
    // carry a Repeat/Release at all (it has no field to hold either
    // sub-field in), so this always uses the `CSI` form instead — matching
    // kitty's own reference, whose `cursor_key_mode` `SS3` special case is
    // itself gated on "legacy mode", unconditionally false once
    // `REPORT_EVENT_TYPES` is negotiated.
    let mut mod_bits = u32::from(mods.encode_xterm());
    if mods.contains(Modifiers::SUPER) {
        mod_bits |= 0b1000;
    }
    let mod_value = 1u32 + mod_bits;
    let text = associated_text_subfield(text, flags, event);
    let mut sequence = format!("{intro};{mod_value}:{event_type}");
    if let Some(text) = text {
        sequence.push(';');
        sequence.push_str(&text);
    }
    sequence.push(terminator);
    Some(sequence.into_bytes())
}

/// The `CSI` intro/terminator pair `legacy_bytes` uses for the navigation
/// keys `KITTY_COMPLIANCE`'s "Report event types" navigation row covers:
/// the letter-terminated arrows/Home/End (always the `CSI 1;...<letter>`
/// form here, the `1` never omitted once decorating — see
/// `navigation_key_event_override`), and the `~`-terminated
/// PageUp/PageDown/Insert/Delete. `None` for every other key (F1-F35,
/// keypad, standalone modifiers, ... — outside that row's scope).
fn navigation_key_form(key: KeyCode) -> Option<(&'static str, char)> {
    Some(match key {
        KeyCode::UpArrow | KeyCode::ApplicationUpArrow => ("\x1b[1", 'A'),
        KeyCode::DownArrow | KeyCode::ApplicationDownArrow => ("\x1b[1", 'B'),
        KeyCode::RightArrow | KeyCode::ApplicationRightArrow => ("\x1b[1", 'C'),
        KeyCode::LeftArrow | KeyCode::ApplicationLeftArrow => ("\x1b[1", 'D'),
        KeyCode::Home | KeyCode::KeyPadHome => ("\x1b[1", 'H'),
        KeyCode::End | KeyCode::KeyPadEnd => ("\x1b[1", 'F'),
        KeyCode::Insert => ("\x1b[2", '~'),
        KeyCode::Delete => ("\x1b[3", '~'),
        KeyCode::PageUp | KeyCode::KeyPadPageUp => ("\x1b[5", '~'),
        KeyCode::PageDown | KeyCode::KeyPadPageDown => ("\x1b[6", '~'),
        _ => return None,
    })
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
/// normalization first, then Ctrl/Alt-aware byte selection).
///
/// Dead in practice as of `encode_text_key`: `TerminalCore::encode_key`
/// intercepts every `KeyCode::Char` before it ever reaches `encode`/
/// `kitty_override`/`legacy_bytes`, so this function (and the `legacy_bytes`
/// arm that calls it) is never actually invoked with a real key press —
/// text keys have their own dedicated, Kitty-flag-aware path now (see
/// `encode_text_key`'s doc comment for why it isn't simply routed through
/// here: this port's Ctrl+Alt handling differs from
/// `app::keymap::character_input`'s pre-existing algorithm, which
/// `encode_text_key`'s legacy branch had to match exactly instead). Left in
/// place, unmodified, as the still-correct byte-for-byte port of termwiz's
/// real `Char` encoder it always was — `legacy_bytes` remains the active
/// path for every other `KeyCode` variant.
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

/// Entry point for `KeyCode::Char` — the only key class `TerminalCore::
/// encode_key` special-cases ahead of `encode`/`kitty_override`/
/// `legacy_bytes` (see its call site's doc comment). `c` follows termwiz's
/// own `KeyCode::Char` convention, the same one `encode_char` above already
/// assumes: the *base* (unshifted) character, with `Modifiers::SHIFT`
/// carrying the shift state separately — e.g. `'a'` + `SHIFT`, not `'A'`,
/// for a Shift+A press. `app::keymap::terminal_key_from_key` reconstructs
/// that convention from winit's already-shifted `Key::Character` text
/// before a real key event ever reaches here.
///
/// Dispatches purely on `REPORT_ALL_KEYS_AS_ESCAPE_CODES`: that is the only
/// flag the Kitty spec ties to text-key promotion
/// (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>, "Report all keys
/// as escape codes": "turns on key reporting even for key events that
/// generate text"; contrast "Disambiguate escape codes", which explicitly
/// scopes its own promotion to "key events that do not generate text").
/// Disambiguate/report-event-types/report-alternate-keys/
/// report-associated-text alone (or no flags at all) fall to
/// `legacy_text_key`, unchanged from before this function existed.
///
/// `event` only ever affects the promoted (`csi_u_text_key`) branch: plain
/// UTF-8 text has no release/repeat representation at all short of
/// promotion (spec: "Key events that result in text are reported as plain
/// UTF-8 text, so events are not supported for them, unless the application
/// requests key report mode"), so a release here (only ever reached once
/// `REPORT_EVENT_TYPES` is active — see `TerminalCore::encode_key`) produces
/// nothing rather than falling through to `legacy_text_key`, and a repeat
/// falls through unchanged (identical bytes to a press, matching every
/// other un-promoted key in this module).
pub(crate) fn encode_text_key(
    c: char,
    mods: Modifiers,
    flags: KittyKeyboardFlags,
    event: KeyEventKind,
    text: Option<&str>,
) -> Vec<u8> {
    if flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES) {
        return csi_u_text_key(c, mods, flags, event, text);
    }
    if !event.is_down() {
        return Vec::new();
    }
    legacy_text_key(c, mods)
}

/// Byte-identical port of `app::keymap::character_input`/`control_input`'s
/// pre-existing algorithm — the exact raw bytes Horizon has always sent for
/// a text key before this module owned the decision, preserved verbatim
/// now that `terminal::core::TerminalCore` makes it instead of the app
/// layer. Deliberately NOT `encode_char` above: that function faithfully
/// mirrors termwiz's *real* `Char` encoder, which ESC-prefixes Ctrl+Alt
/// combinations; `character_input`'s Ctrl branch returns before ever
/// checking Alt, so Ctrl+Alt+<letter> has always produced Horizon's bare
/// Ctrl byte with no `ESC` — a pre-existing mismatch between the two that
/// this function preserves rather than "fixes", since fixing it here would
/// be an observable legacy-output change this task doesn't call for.
///
/// One deliberate widening: Ctrl lookups go through `ctrl_mapping` below
/// (the fuller, wezterm-derived table already used elsewhere in this file)
/// rather than `control_input`'s smaller hand-written one, so e.g.
/// Ctrl+Space now sends NUL instead of nothing — see `KITTY_COMPLIANCE`.
fn legacy_text_key(c: char, mods: Modifiers) -> Vec<u8> {
    if mods.contains(Modifiers::CTRL) {
        return match ctrl_mapping(c) {
            Some(mapped) => vec![mapped as u8],
            None => Vec::new(),
        };
    }
    if mods.contains(Modifiers::SUPER) {
        return Vec::new();
    }

    let display = if mods.contains(Modifiers::SHIFT) && c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    };
    let mut bytes = Vec::new();
    if mods.contains(Modifiers::ALT) {
        bytes.push(0x1b);
    }
    let mut buf = [0u8; 4];
    bytes.extend_from_slice(display.encode_utf8(&mut buf).as_bytes());
    bytes
}

/// Genuine Kitty `CSI u` for a text key once `REPORT_ALL_KEYS_AS_ESCAPE_CODES`
/// is active. Per spec, the `unicode-key-code` is always the base
/// (unshifted) codepoint, regardless of Shift or Ctrl — "the codepoint used
/// is _always_ the lower-case (or more technically, un-shifted) version of
/// the key... If the user presses, for example, ctrl+shift+a the escape
/// code would be `CSI 97;<modifiers>u`. It _must not_ be `CSI
/// 65;<modifiers>u`" — which `c` already is, per this module's `KeyCode::
/// Char` convention (see `encode_text_key`'s doc comment), so it's used
/// directly with no case-folding here (unlike `legacy_text_key`, which
/// still needs the *display* form for its legacy bytes).
///
/// Also emits the "Report alternate keys" (`0b100`) subfield — the shifted
/// codepoint — whenever it's cheaply knowable: exactly the ASCII-letter
/// case termwiz's own `normalize_shift_to_upper_case` handles (`'a'` ->
/// `'A'`). Digits and punctuation have no algorithmic shift (Shift+1 -> '!'
/// depends on keyboard layout, which Horizon doesn't have access to here —
/// see `KITTY_COMPLIANCE`), so no alternate is reported for them.
///
/// Also decorates the modifier field with the "event types" subfield (see
/// `event_type_subfield`) — text keys have no exception analogous to
/// Enter/Tab/Backspace's crash-recovery carve-out, so this applies
/// unconditionally once promoted here.
fn csi_u_text_key(
    c: char,
    mods: Modifiers,
    flags: KittyKeyboardFlags,
    event: KeyEventKind,
    text: Option<&str>,
) -> Vec<u8> {
    let mut mod_bits = u32::from(mods.encode_xterm());
    if mods.contains(Modifiers::SUPER) {
        mod_bits |= 0b1000;
    }
    let mod_value = 1u32 + mod_bits;

    let mut sequence = format!("\x1b[{}", c as u32);
    if flags.contains(KittyKeyboardFlags::REPORT_ALTERNATE_KEYS)
        && mods.contains(Modifiers::SHIFT)
        && c.is_ascii_lowercase()
    {
        sequence.push_str(&format!(":{}", c.to_ascii_uppercase() as u32));
    }
    let event_type = event_type_subfield(event, flags);
    let text = associated_text_subfield(text, flags, event);
    if mod_value != 1 || event_type.is_some() || text.is_some() {
        sequence.push_str(&format!(";{mod_value}"));
        if let Some(event_type) = event_type {
            sequence.push_str(&format!(":{event_type}"));
        }
        if let Some(text) = text {
            sequence.push(';');
            sequence.push_str(&text);
        }
    }
    sequence.push('u');
    sequence.into_bytes()
}

/// `KeyCode::Function(n)` handling, split out of `legacy_bytes` for
/// readability. Ported from termwiz's own table: F1-F4 use SS3 when
/// unmodified and CSI when modified, F5-F24 reuse legacy rxvt-style `CSI
/// n~` numbers (which the Kitty spec's own "Functional key definitions"
/// table also documents as F1-F12's alternate numeric forms). This
/// function is only ever reached with `flags.is_empty()` (see `encode`) or
/// for `n` outside `kitty_override`'s `13..=24` PUA range — see
/// `KITTY_COMPLIANCE`'s "Functional key definitions: F13-F24"/"F25-F35"
/// rows: F13-F24's numbers here are legacy rxvt numbers, not Kitty's PUA
/// codes, which is why `kitty_override` takes over for them once any Kitty
/// flag is active; F25+ has no representation at all, in either place.
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
    /// it. No `KITTY_COMPLIANCE` row currently uses this verdict (the last
    /// one, text keys, was fixed by routing them through
    /// `kitty_keyboard::encode_text_key` — see `terminal_key_from_character`
    /// in `app::keymap`), but the variant stays: it's part of this general
    /// verdict vocabulary, not tied to any one feature, and a future gap
    /// may well be an app-layer bypass again.
    #[allow(dead_code)]
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
             against Claude Code 2.1.201's real negotiation; see kitty_override's doc comment. \
             The same promotion test also gates release/repeat event-type reporting for these \
             keys (see the \"Report event types\" rows below), which is how this deviation \
             reproduces the spec's own \"no release without report-all-keys\" carve-out for the \
             *bare* key without a second, separately-tracked condition.",
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
        key_class: "arrows, Home, End, PageUp, PageDown, Insert, Delete",
        verdict: Verdict::Compliant,
        tests: &["navigation_keys_are_flag_invariant_and_spec_compliant"],
    },
    FeatureEntry {
        feature: "Report event types",
        key_class: "Enter, Tab, Backspace, Escape",
        verdict: Verdict::Compliant,
        tests: &["csi_u_event_type_truth_table"],
    },
    FeatureEntry {
        feature: "Report event types",
        key_class: "text keys (letters, digits, punctuation)",
        verdict: Verdict::Compliant,
        tests: &[
            "release_events_are_unimplemented_regardless_of_flags",
            "csi_u_event_type_truth_table",
        ],
    },
    FeatureEntry {
        feature: "Report event types",
        key_class: "navigation/legacy functional forms (arrows, Home, End, PageUp, PageDown, \
                    Insert, Delete)",
        verdict: Verdict::Compliant,
        tests: &[
            "csi_u_navigation_key_event_type_truth_table",
            "csi_u_event_type_truth_table",
        ],
    },
    FeatureEntry {
        feature: "Functional key definitions: F13-F24",
        key_class: "function keys (high, app-wired)",
        verdict: Verdict::Deviation(
            "reports the dedicated Private-Use-Area CSI u codepoints (57376-57387) once any \
             Kitty flag is negotiated (kitty_override), but — unlike kitty's own reference \
             implementation, which always emits these regardless of flags, even with zero \
             enhancements negotiated — keeps termwiz's legacy rxvt-style numbers when no Kitty \
             flag is active at all, preserving compatibility for old programs that never \
             negotiate the protocol. The spec's own text explicitly permits this choice: \
             \"terminals may instead choose to ignore such keys in legacy mode instead, or have \
             an option to control this behavior.\" `docs/tasks/backlog.md` item 2, resolved: \
             app::keymap now maps NamedKey::F1..F24 to a TermKeyCode, so this path is reachable \
             from the real UI too.",
        ),
        tests: &["high_function_keys_use_legacy_numbers_without_kitty_flags_and_pua_codes_with_them"],
    },
    FeatureEntry {
        feature: "Functional key definitions: F25-F35",
        key_class: "function keys (very high)",
        verdict: Verdict::Unimplemented(
            "no PUA table entry for these in kitty_override (bounded to 13..=24, matching \
             termwiz's own KeyCode::Function doc: \"F1-F24 are possible\"), so they still \
             produce nothing. Also BYPASSED at the app layer: app::keymap has no \
             NamedKey::F25..F35 arm, matching this bug's own explicit scope \
             (`docs/tasks/backlog.md` item 2 title: \"Insert and F1-F24\"). Effort: small — \
             extend kitty_override's Function range to 13..=35 and add the matching app::keymap \
             arms",
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
        key_class: "plain/Shift/Ctrl/Alt letters, digits, punctuation",
        verdict: Verdict::Compliant,
        tests: &[
            "csi_u_text_key_truth_table",
            "shift_letter_produces_csi_u_under_report_all_keys",
        ],
    },
    FeatureEntry {
        feature: "Report all keys as escape codes (text keys): shifted digits/punctuation",
        key_class: "Shift+digit, Shift+punctuation (e.g. Shift+1 -> '!')",
        verdict: Verdict::Deviation(
            "reports the actual shifted codepoint (e.g. 33 for '!') instead of the spec's \
             mandated base/unshifted one (49 for '1'): the base isn't recoverable from GPUI's \
             already-shifted keystroke text without OS keyboard-layout support. Whether it \
             becomes available under today's native GPUI platform layer \
             (`gpui_platform`, `src/keymap.rs`) deserves re-verification rather than \
             being assumed. ASCII letters (Shift+a -> reports 97, not 65) ARE inverted correctly \
             since case-folding needs no layout knowledge — see `encode_text_key`/`csi_u_text_key`.",
        ),
        tests: &["csi_u_text_key_truth_table"],
    },
    FeatureEntry {
        feature: "Report alternate keys",
        key_class: "text keys",
        verdict: Verdict::Deviation(
            "only emits the shifted-key subfield for ASCII letters (Shift+a -> `97:65`), the one \
             case where the shifted codepoint is cheaply derivable (case-folding) without OS \
             keyboard-layout data; digits and punctuation (Shift+1 -> '!') have no algorithmic \
             shift and report no alternate at all — see the sibling \"shifted digits/punctuation\" \
             row above",
        ),
        tests: &["csi_u_text_key_reports_alternate_for_shifted_letter_only"],
    },
    FeatureEntry {
        feature: "Report associated text",
        key_class: "text keys",
        verdict: Verdict::Compliant,
        tests: &["csi_u_associated_text_truth_table"],
    },
    FeatureEntry {
        feature: "Report associated text: keyless IME commit",
        key_class: "text input without a key event",
        verdict: Verdict::Compliant,
        tests: &[
            "text_input_encodes_as_keyless_csi_u",
            "text_input_falls_back_to_raw_utf8_without_flags",
        ],
    },
];

#[cfg(test)]
mod tests;
