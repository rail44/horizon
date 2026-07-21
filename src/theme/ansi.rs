//! Terminal-facing color resolution ([`resolve`], plus the core-side
//! [`terminal_color_scheme`] mirror): the `[theme.ansi]`-derived 16-slot
//! palette, indexed/256-color and OSC 4/10/11/12 lookups, all against the
//! same live [`super::scheme::scheme`].

use alacritty_terminal::vte::ansi::Rgb;
use gpui::{rgb, Hsla};
use horizon_terminal_core::{NamedColor, TerminalColor};

use super::scheme::scheme;

/// The core-side scheme for OSC 4/10/11/12 query replies, mirrored from
/// the same values the view paints with.
pub(crate) fn terminal_color_scheme() -> horizon_terminal_core::TerminalColorScheme {
    let scheme = scheme();
    let rgb = |value: u32| Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    };
    horizon_terminal_core::TerminalColorScheme {
        foreground: rgb(scheme.foreground),
        background: rgb(scheme.background),
        cursor: rgb(scheme.cursor),
        black: rgb(scheme.ansi[0]),
        red: rgb(scheme.ansi[1]),
        green: rgb(scheme.ansi[2]),
        yellow: rgb(scheme.ansi[3]),
        blue: rgb(scheme.ansi[4]),
        magenta: rgb(scheme.ansi[5]),
        cyan: rgb(scheme.ansi[6]),
        white: rgb(scheme.ansi[7]),
        bright_black: rgb(scheme.ansi[8]),
        bright_red: rgb(scheme.ansi[9]),
        bright_green: rgb(scheme.ansi[10]),
        bright_yellow: rgb(scheme.ansi[11]),
        bright_blue: rgb(scheme.ansi[12]),
        bright_magenta: rgb(scheme.ansi[13]),
        bright_cyan: rgb(scheme.ansi[14]),
        bright_white: rgb(scheme.ansi[15]),
    }
}

pub(crate) fn to_hsla(rgb888: [u8; 3]) -> Hsla {
    rgb(((rgb888[0] as u32) << 16) | ((rgb888[1] as u32) << 8) | rgb888[2] as u32).into()
}

pub(crate) fn resolve(color: TerminalColor, overrides: &[(u16, [u8; 3])]) -> [u8; 3] {
    let override_index = match color {
        TerminalColor::Rgb(_) | TerminalColor::Unknown => None,
        TerminalColor::Indexed(index) => Some(index as u16),
        TerminalColor::Named(named) => named.override_index(),
    };
    if let Some(rgb) = override_index
        .and_then(|index| {
            overrides
                .binary_search_by_key(&index, |(index, _)| *index)
                .ok()
        })
        .map(|pos| overrides[pos].1)
    {
        return rgb;
    }

    match color {
        TerminalColor::Rgb(rgb) => rgb,
        TerminalColor::Indexed(index) => indexed_rgb(index),
        TerminalColor::Named(named) => named_rgb(named),
        // Skew catch-all (`TerminalColor::Unknown`'s doc): resolve like
        // the default foreground role -- one cell loses its hue for one
        // frame, nothing else.
        TerminalColor::Unknown => named_rgb(NamedColor::Foreground),
    }
}

fn split(value: u32) -> [u8; 3] {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

fn named_rgb(color: NamedColor) -> [u8; 3] {
    match color {
        NamedColor::Black => split(scheme().ansi[0]),
        NamedColor::Red => split(scheme().ansi[1]),
        NamedColor::Green => split(scheme().ansi[2]),
        NamedColor::Yellow => split(scheme().ansi[3]),
        NamedColor::Blue => split(scheme().ansi[4]),
        NamedColor::Magenta => split(scheme().ansi[5]),
        NamedColor::Cyan => split(scheme().ansi[6]),
        NamedColor::White => split(scheme().ansi[7]),
        NamedColor::DimWhite => [170, 176, 190],
        NamedColor::BrightBlack | NamedColor::DimBlack => split(scheme().ansi[8]),
        NamedColor::BrightRed | NamedColor::DimRed => split(scheme().ansi[9]),
        NamedColor::BrightGreen | NamedColor::DimGreen => split(scheme().ansi[10]),
        NamedColor::BrightYellow | NamedColor::DimYellow => split(scheme().ansi[11]),
        NamedColor::BrightBlue | NamedColor::DimBlue => split(scheme().ansi[12]),
        NamedColor::BrightMagenta | NamedColor::DimMagenta => split(scheme().ansi[13]),
        NamedColor::BrightCyan | NamedColor::DimCyan => split(scheme().ansi[14]),
        NamedColor::BrightWhite => split(scheme().ansi[15]),
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            split(scheme().foreground)
        }
        NamedColor::Background => split(scheme().background),
        NamedColor::Cursor => split(scheme().cursor),
        // Skew catch-all (`NamedColor::Unknown`'s doc): resolved like the
        // default foreground role.
        NamedColor::Unknown => split(scheme().foreground),
    }
}

fn indexed_rgb(index: u8) -> [u8; 3] {
    if index < 16 {
        return split(scheme().ansi[index as usize]);
    }
    if index < 232 {
        let index = index - 16;
        let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
        return [
            component(index / 36),
            component((index / 6) % 6),
            component(index % 6),
        ];
    }
    let gray = 8 + (index - 232) * 10;
    [gray, gray, gray]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::scheme::reload_from;
    use crate::theme::test_support::config_with;

    /// docs/tasks/backlog.md item 25 (since retired): `resolve`/`to_hsla`
    /// read `scheme()` -- a plain `RwLock` read -- with no intermediate
    /// cache of resolved RGB anywhere *in this module*. So a `Reload
    /// Config` (`reload_from` + `window.refresh()`, `src/workspace.rs`)
    /// recolors a static terminal screen with no extra invalidation step:
    /// `window.refresh()` alone is already sufficient, because there is
    /// nothing here to go stale. (The paint path has since grown a
    /// row-keyed `ShapedLine` cache -- `src/terminal/shape_cache.rs` --
    /// that *does* bake resolved colors into its cached runs, but it
    /// fingerprints the theme via `terminal_color_scheme()` and
    /// self-clears on mismatch at the next paint, so this reload contract
    /// still needs no explicit invalidation call.)
    #[test]
    fn resolve_reflects_a_reload_immediately_with_no_separate_cache_to_invalidate() {
        // `terminal_background` was retired as a key (2026-07-16,
        // `docs/theme-design.md`) -- `surface_base` is the only anchor for
        // both chrome and the terminal now.
        reload_from(&config_with(&[("surface_base", "#010203")]));
        assert_eq!(
            resolve(TerminalColor::Named(NamedColor::Background), &[]),
            [0x01, 0x02, 0x03]
        );
        // A second reload -- simulating a static screen that never got a
        // new PTY-driven frame between the two `Reload Config` runs --
        // still picks up the new value on the very next call.
        reload_from(&config_with(&[("surface_base", "#0a0b0c")]));
        assert_eq!(
            resolve(TerminalColor::Named(NamedColor::Background), &[]),
            [0x0a, 0x0b, 0x0c]
        );
    }
}
