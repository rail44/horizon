//! Default RGB resolution for OSC 4/10/11/12 color-query responses
//! (`Event::ColorRequest`, wired in `core/events.rs` and resolved in
//! `TerminalCore::write_vt`).
//!
//! `core::render` has an equivalent table (`resolve_color`/`named_rgb`/
//! `indexed_rgb`) for painting cells, but that one moved to the host
//! (`terminal::view`, `docs/session-daemon-design.md` decision 8) once cell
//! colors stopped needing an immediate RGB answer at all. This module's
//! table stays here because a color *query reply* is a byte sequence sent
//! back to the app right now — it cannot be deferred to a UI paint the way
//! a cell's color can — so it keeps its own copy, resolved against
//! [`TerminalColorScheme`] rather than a live theme this crate has no
//! access to. The live OSC 4/10/11/12 overrides (`overrides` below) also
//! ride the frame as `TerminalFrame::palette_overrides` so the host's own
//! cell-rendering resolution can honor them too — this module's read of
//! `Term::colors()` and that snapshot are two independent consumers of the
//! same underlying state, not a narrowing.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use serde::{Deserialize, Serialize};

/// Crate-local mirror of the host's `ui::theme`-derived color roles
/// (`terminal::config::TerminalColors`). This crate has no dependency on
/// `ui::theme`/floem at all (`docs/session-daemon-design.md` decision 9), so
/// the host converts its live theme into this plain-data struct and pushes
/// it in via `TerminalCore::set_color_scheme` — the same "crate-local
/// newtype / plain-data mirror" pattern `crates/horizon-agent` already uses
/// for `SessionId`.
///
/// [`Default`] duplicates the host's own built-in theme defaults
/// (`ui::theme`'s `ansi` module and `text_primary`/`surface_base`/`accent`
/// roles) so a freshly constructed `TerminalCore` that never receives an
/// explicit scheme (every test in this crate) still answers color queries
/// with the same values Horizon has always shipped. In the running app this
/// default is only ever transiently observed (`TerminalSession::spawn`
/// pushes the live theme immediately after construction).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalColorScheme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
    pub bright_black: Rgb,
    pub bright_red: Rgb,
    pub bright_green: Rgb,
    pub bright_yellow: Rgb,
    pub bright_blue: Rgb,
    pub bright_magenta: Rgb,
    pub bright_cyan: Rgb,
    pub bright_white: Rgb,
}

impl Default for TerminalColorScheme {
    fn default() -> Self {
        const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
            Rgb { r, g, b }
        }
        Self {
            foreground: rgb(233, 236, 242),
            background: rgb(22, 24, 29),
            cursor: rgb(132, 220, 198),
            black: rgb(35, 38, 46),
            red: rgb(224, 108, 117),
            green: rgb(152, 195, 121),
            yellow: rgb(229, 192, 123),
            blue: rgb(97, 175, 239),
            magenta: rgb(198, 120, 221),
            cyan: rgb(86, 182, 194),
            white: rgb(222, 226, 234),
            bright_black: rgb(95, 99, 112),
            bright_red: rgb(255, 123, 127),
            bright_green: rgb(181, 214, 140),
            bright_yellow: rgb(245, 211, 139),
            bright_blue: rgb(120, 194, 255),
            bright_magenta: rgb(218, 140, 255),
            bright_cyan: rgb(103, 205, 216),
            bright_white: rgb(255, 255, 255),
        }
    }
}

/// Resolve the RGB value alacritty_terminal should report for color query
/// `index`: 0..16 base ANSI, 16..232 color cube, 232..256 grayscale ramp,
/// 256/257/258 foreground/background/cursor (see
/// `alacritty_terminal::term::color::Colors` docs — these are the only
/// indices reachable via OSC 4/10/11/12). `overrides` is the live palette
/// (`Term::colors()`): an app that already set this slot via OSC
/// 4/10/11/12 gets that value back; otherwise `scheme`.
pub(super) fn resolve_query_color(
    index: usize,
    overrides: &Colors,
    scheme: &TerminalColorScheme,
) -> Rgb {
    if let Some(rgb) = overrides[index] {
        return rgb;
    }
    default_rgb(index, *scheme)
}

fn default_rgb(index: usize, scheme: TerminalColorScheme) -> Rgb {
    if index < 16 {
        return named_rgb(base_ansi_color(index), scheme);
    }

    if index < 232 {
        let cube = (index - 16) as u8;
        let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
        return Rgb {
            r: component(cube / 36),
            g: component((cube / 6) % 6),
            b: component(cube % 6),
        };
    }

    if index < 256 {
        let gray = 8 + (index - 232) as u8 * 10;
        return Rgb {
            r: gray,
            g: gray,
            b: gray,
        };
    }

    named_rgb(
        match index {
            i if i == NamedColor::Background as usize => NamedColor::Background,
            i if i == NamedColor::Cursor as usize => NamedColor::Cursor,
            _ => NamedColor::Foreground,
        },
        scheme,
    )
}

fn base_ansi_color(index: usize) -> NamedColor {
    match index {
        0 => NamedColor::Black,
        1 => NamedColor::Red,
        2 => NamedColor::Green,
        3 => NamedColor::Yellow,
        4 => NamedColor::Blue,
        5 => NamedColor::Magenta,
        6 => NamedColor::Cyan,
        7 => NamedColor::White,
        8 => NamedColor::BrightBlack,
        9 => NamedColor::BrightRed,
        10 => NamedColor::BrightGreen,
        11 => NamedColor::BrightYellow,
        12 => NamedColor::BrightBlue,
        13 => NamedColor::BrightMagenta,
        14 => NamedColor::BrightCyan,
        _ => NamedColor::BrightWhite,
    }
}

fn named_rgb(color: NamedColor, scheme: TerminalColorScheme) -> Rgb {
    match color {
        NamedColor::Black => scheme.black,
        NamedColor::Red => scheme.red,
        NamedColor::Green => scheme.green,
        NamedColor::Yellow => scheme.yellow,
        NamedColor::Blue => scheme.blue,
        NamedColor::Magenta => scheme.magenta,
        NamedColor::Cyan => scheme.cyan,
        NamedColor::White => scheme.white,
        NamedColor::BrightBlack => scheme.bright_black,
        NamedColor::BrightRed => scheme.bright_red,
        NamedColor::BrightGreen => scheme.bright_green,
        NamedColor::BrightYellow => scheme.bright_yellow,
        NamedColor::BrightBlue => scheme.bright_blue,
        NamedColor::BrightMagenta => scheme.bright_magenta,
        NamedColor::BrightCyan => scheme.bright_cyan,
        NamedColor::BrightWhite => scheme.bright_white,
        NamedColor::Foreground => scheme.foreground,
        NamedColor::Background => scheme.background,
        NamedColor::Cursor => scheme.cursor,
        // Dim*/BrightForeground/DimForeground are not reachable through
        // OSC 4/10/11/12 (index is always < 256, or exactly 256/257/258),
        // so this arm is defensive rather than load-bearing.
        _ => scheme.foreground,
    }
}
