//! Default RGB resolution for OSC 4/10/11/12 color-query responses
//! (`Event::ColorRequest`, wired in `core/events.rs` and resolved in
//! `TerminalCore::write_vt`).
//!
//! `core::render` has an equivalent table (`resolve_color`/`named_rgb`/
//! `indexed_rgb`) for painting cells, but it is private to that module and
//! out of scope to touch here, so the mapping is duplicated for the
//! query-response path. If ownership ever allows touching `render.rs`,
//! promoting that table to `pub(super)` and reusing it here would remove
//! the duplication.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

use crate::terminal::config::{resolved_colors, TerminalColors};

/// Resolve the RGB value alacritty_terminal should report for color query
/// `index`: 0..16 base ANSI, 16..232 color cube, 232..256 grayscale ramp,
/// 256/257/258 foreground/background/cursor (see
/// `alacritty_terminal::term::color::Colors` docs — these are the only
/// indices reachable via OSC 4/10/11/12). `overrides` is the live palette
/// (`Term::colors()`): an app that already set this slot via OSC
/// 4/10/11/12 gets that value back; otherwise Horizon's configured theme.
pub(super) fn resolve_query_color(index: usize, overrides: &Colors) -> Rgb {
    if let Some(rgb) = overrides[index] {
        return rgb;
    }
    default_rgb(index, resolved_colors())
}

fn default_rgb(index: usize, scheme: &TerminalColors) -> Rgb {
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

fn named_rgb(color: NamedColor, scheme: &TerminalColors) -> Rgb {
    let [r, g, b] = match color {
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
    };
    Rgb { r, g, b }
}
