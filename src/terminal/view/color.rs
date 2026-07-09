//! Resolves a cell's logical color (`horizon_terminal_core::TerminalColor`,
//! an ANSI/256 index, a named role, or a literal truecolor value --
//! `docs/session-daemon-design.md` decision 8) against Horizon's live theme
//! (`terminal::config::resolved_colors`) into the `[u8; 3]` RGB triple
//! `terminal::view::layout`/`render` paint with.
//!
//! This is the host-side half of decision 8's color cut: `TerminalCore`
//! (now in `horizon-terminal-core`) used to resolve this itself, reading
//! `ui::theme::terminal_colors()` -- its one cross-crate dependency, and the
//! thing blocking the extraction. The resolution table itself
//! (`resolve_color`/`named_rgb`/`indexed_rgb`) moved here unchanged. The
//! per-session live OSC 4/10/11/12 palette overrides
//! (`alacritty_terminal::term::color::Colors`) ride the frame as
//! `TerminalFrame::palette_overrides`, a sparse index->RGB table this module
//! checks before falling back to the theme -- an app that redefines a
//! palette color at runtime does affect cell rendering, and a literal
//! override always wins over the (possibly per-client, in the future) theme
//! for that slot.

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use horizon_terminal_core::TerminalColor;

use crate::terminal::config::TerminalColors;

/// `overrides` is `TerminalFrame::palette_overrides`: sorted ascending by
/// index, so lookups are a binary search rather than a linear scan.
pub(super) fn resolve_color(
    color: TerminalColor,
    scheme: TerminalColors,
    overrides: &[(u16, [u8; 3])],
) -> [u8; 3] {
    // Bold/dim promote a named color to its Bright*/Dim* variant
    // (`core::render::cell_fg`), and those variants sit past index 258 with
    // no OSC slot of their own (only the base role at 256/257/258 is
    // settable via OSC 10/11/12) -- e.g. a bold default-fg cell carries
    // `BrightForeground`, which never matches an override and always falls
    // through to the theme. Documented minor edge, not worth promoting the
    // lookup to the base role since that would apply an OSC-10 fg override
    // to text that intentionally isn't the plain default foreground.
    let override_index = match color {
        TerminalColor::Spec(_) => None,
        TerminalColor::Indexed(index) => Some(index as u16),
        TerminalColor::Named(named) => Some(named as usize as u16),
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
        TerminalColor::Spec(Rgb { r, g, b }) => [r, g, b],
        TerminalColor::Indexed(index) => indexed_rgb(index, scheme),
        TerminalColor::Named(named) => named_rgb(named, scheme),
    }
}

/// Maps an `alacritty_terminal` named color to RGB, sourcing the 16 base
/// ANSI slots plus foreground/background/cursor from the app theme
/// (`scheme`, `terminal::config::resolved_colors`). `DimWhite` is the one
/// exception: alacritty gives "dim white" its own distinct shade (unlike
/// the other colors, whose `Dim*` variant just reuses the `Bright*` value),
/// and there is no theme slot for it, so it stays hardcoded.
fn named_rgb(color: NamedColor, scheme: TerminalColors) -> [u8; 3] {
    match color {
        NamedColor::Black => scheme.black,
        NamedColor::Red => scheme.red,
        NamedColor::Green => scheme.green,
        NamedColor::Yellow => scheme.yellow,
        NamedColor::Blue => scheme.blue,
        NamedColor::Magenta => scheme.magenta,
        NamedColor::Cyan => scheme.cyan,
        NamedColor::White => scheme.white,
        NamedColor::DimWhite => [170, 176, 190],
        NamedColor::BrightBlack | NamedColor::DimBlack => scheme.bright_black,
        NamedColor::BrightRed | NamedColor::DimRed => scheme.bright_red,
        NamedColor::BrightGreen | NamedColor::DimGreen => scheme.bright_green,
        NamedColor::BrightYellow | NamedColor::DimYellow => scheme.bright_yellow,
        NamedColor::BrightBlue | NamedColor::DimBlue => scheme.bright_blue,
        NamedColor::BrightMagenta | NamedColor::DimMagenta => scheme.bright_magenta,
        NamedColor::BrightCyan | NamedColor::DimCyan => scheme.bright_cyan,
        NamedColor::BrightWhite => scheme.bright_white,
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            scheme.foreground
        }
        NamedColor::Background => scheme.background,
        NamedColor::Cursor => scheme.cursor,
    }
}

fn indexed_rgb(index: u8, scheme: TerminalColors) -> [u8; 3] {
    if index < 16 {
        return named_rgb(
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
            },
            scheme,
        );
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
    use horizon_terminal_core::{TerminalCore, TerminalSize};

    /// Color golden test (`docs/session-daemon-design.md` decision 9's
    /// done-definition): a known byte sequence run through the extracted
    /// `TerminalCore` produces a *logical*-color frame, and resolving it
    /// here with the default theme reproduces the exact RGB values the
    /// pre-extraction `TerminalCore` used to bake in directly (see the
    /// crate's own `vt_stream_preserves_ansi_foreground_color`, which
    /// checks the logical-color half of this same byte sequence) --
    /// proving the color cut is visually neutral for the default theme.
    #[test]
    fn known_bytes_resolve_to_the_pre_cut_rgb_values_under_the_default_theme() {
        let mut core = TerminalCore::new(TerminalSize::new(20, 4));
        core.write_vt(b"\x1b[31mred\x1b[0m plain");

        let frame = core.snapshot_frame();
        let scheme = crate::terminal::config::resolved_colors();
        let first_line = &frame.lines[0];

        let red_span = first_line
            .spans
            .iter()
            .find(|span| span.text == "r")
            .expect("a red-colored span should exist");
        assert_eq!(resolve_color(red_span.fg, scheme, &[]), [224, 108, 117]);

        let plain_span = first_line
            .spans
            .iter()
            .find(|span| span.text == "p")
            .expect("a default-colored span should exist");
        assert_eq!(resolve_color(plain_span.fg, scheme, &[]), scheme.foreground);
    }

    /// An OSC 4/10/11/12 palette override for a cell's index wins over the
    /// theme (`docs/session-daemon-design.md` decision 8's restored
    /// rendering path): a literal override always beats the per-client
    /// theme for that slot.
    #[test]
    fn indexed_color_with_a_matching_override_returns_the_override_rgb() {
        let scheme = crate::terminal::config::resolved_colors();
        let overrides = [(1u16, [9, 8, 7])];

        assert_eq!(
            resolve_color(TerminalColor::Indexed(1), scheme, &overrides),
            [9, 8, 7]
        );
    }

    /// An index with no matching override falls through to the theme,
    /// unaffected by unrelated overrides in the table.
    #[test]
    fn indexed_color_without_a_matching_override_falls_back_to_the_theme() {
        let scheme = crate::terminal::config::resolved_colors();
        let overrides = [(1u16, [9, 8, 7])];

        assert_eq!(
            resolve_color(TerminalColor::Indexed(2), scheme, &overrides),
            indexed_rgb(2, scheme)
        );
    }

    /// OSC 10 (set foreground) writes `NamedColor::Foreground as usize ==
    /// 256`; a `Named(Foreground)` cell picks that override up.
    #[test]
    fn named_foreground_with_a_matching_override_returns_the_override_rgb() {
        let scheme = crate::terminal::config::resolved_colors();
        let overrides = [(256u16, [1, 2, 3])];

        assert_eq!(
            resolve_color(
                TerminalColor::Named(NamedColor::Foreground),
                scheme,
                &overrides
            ),
            [1, 2, 3]
        );
    }
}
