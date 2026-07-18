use serde::{Deserialize, Serialize};

/// A cell's logical color: one of the 16 base ANSI slots (+ their bold/dim
/// promotions), a default-role slot (foreground/background/cursor, also
/// bold/dim-promotable), an xterm 256-color palette index, or a literal
/// 24-bit truecolor value.
///
/// Horizon-owned rather than re-exported from `alacritty_terminal` (as it
/// was before this type existed) so UI clients (`src/theme/ansi.rs`,
/// `src/terminal/`) don't need `alacritty_terminal` as a dependency just to
/// match on a cell's color. `core::render::snapshot_frame` is the one place
/// that converts from `alacritty_terminal`'s own `vte::ansi::Color`/
/// `NamedColor` into this type, at frame-build time.
///
/// `docs/session-daemon-design.md` decision 8: this is what actually crosses
/// the `TerminalFrame`/`TerminalSpan` boundary now, instead of a resolved
/// `[u8; 3]` RGB triple — resolving a logical color against a theme (the app
/// default, or in the future a per-client one) is the UI's job
/// (`terminal::view`), not this crate's. A color a terminal app redefined at
/// runtime via OSC 4/10/11/12 (`Term::colors()`'s live per-session
/// overrides) still affects cell *rendering*: `TerminalFrame::palette_overrides`
/// carries that sparse index→RGB table alongside the logical colors, and the
/// UI consults it before falling back to the theme. `TerminalCore`'s own
/// OSC 4/10/11/12 *query replies* (`core::color::resolve_query_color`) use
/// alacritty's own `Colors`/`NamedColor` directly, independent of this
/// boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalColor {
    Named(NamedColor),
    Indexed(u8),
    Rgb([u8; 3]),
}

/// One of the fixed named color roles a cell can carry. This set was
/// derived by reading exactly what `core::render::snapshot_frame` (via
/// `cell_fg`/`cell_bg`) can produce from a live `alacritty_terminal::Term`,
/// not assumed:
///
/// - The 8 base ANSI hues (`Black`..`White`) and their bright forms
///   (`BrightBlack`..`BrightWhite`) reach frames directly from SGR
///   30-37/90-97, or as the bold-promoted form of a base hue (`cell_fg`
///   applies `NamedColor::to_bright` under the `BOLD` flag).
/// - Dim forms (`DimBlack`..`DimWhite`) are *not* a normal SGR target —
///   they only ever appear as the dim-promoted form of a base hue
///   (`cell_fg` applies `NamedColor::to_dim` under the `DIM` flag).
/// - `Foreground`/`Background`/`Cursor` are the default-role slots; only
///   `Foreground` is ever bold/dim-promoted this way too, producing
///   `BrightForeground`/`DimForeground` (`Background`/`Cursor` cells never
///   carry `BOLD`/`DIM` promotion — `cell_bg` only reads the *default*
///   foreground's promoted form when swapping colors under `INVERSE`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    DimBlack,
    DimRed,
    DimGreen,
    DimYellow,
    DimBlue,
    DimMagenta,
    DimCyan,
    DimWhite,
    Foreground,
    Background,
    Cursor,
    BrightForeground,
    DimForeground,
}

impl NamedColor {
    /// This role's index in `TerminalFrame::palette_overrides`, mirroring
    /// `alacritty_terminal`'s OSC-addressable color slots: 0..=15 for the
    /// base ANSI hues (incl. bright), 256/257/258 for the default
    /// foreground/background/cursor roles — the only indices OSC 4/10/11/12
    /// ever write into `Term::colors()` (see
    /// `core::render::palette_overrides`). The bold/dim-promoted roles
    /// (`Dim*`, `BrightForeground`, `DimForeground`) are never set this way
    /// and have no override slot.
    pub fn override_index(self) -> Option<u16> {
        use NamedColor::*;
        Some(match self {
            Black => 0,
            Red => 1,
            Green => 2,
            Yellow => 3,
            Blue => 4,
            Magenta => 5,
            Cyan => 6,
            White => 7,
            BrightBlack => 8,
            BrightRed => 9,
            BrightGreen => 10,
            BrightYellow => 11,
            BrightBlue => 12,
            BrightMagenta => 13,
            BrightCyan => 14,
            BrightWhite => 15,
            Foreground => 256,
            Background => 257,
            Cursor => 258,
            DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan
            | DimWhite | BrightForeground | DimForeground => return None,
        })
    }
}
