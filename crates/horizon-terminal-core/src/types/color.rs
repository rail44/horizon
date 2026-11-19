use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The theme-independent part of the xterm palette (indices 16..=255).
/// Base ANSI slots return `None`: the caller supplies its current theme.
/// Runtime OSC overrides take precedence and must be checked before this.
pub fn fixed_palette_rgb(index: u8) -> Option<[u8; 3]> {
    match index {
        0..=15 => None,
        16..=231 => {
            let cube = index - 16;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            Some([
                component(cube / 36),
                component((cube / 6) % 6),
                component(cube % 6),
            ])
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            Some([gray, gray, gray])
        }
    }
}

/// A cell's logical color: one of the 16 base ANSI slots (+ their bold/dim
/// promotions), a default-role slot (foreground/background/cursor, also
/// bold/dim-promotable), an xterm 256-color palette index, or a literal
/// 24-bit truecolor value.
///
/// Horizon-owned, so a view matches on a cell's color without the engine.
/// `core::render` is the one place that converts the engine's style colors
/// into this type, as a row is built.
///
/// `docs/session-daemon-design.md` decision 8: a row carries this logical
/// color, not a resolved `[u8; 3]` RGB triple — resolving it against the
/// theme is the view's (`src/terminal/mod.rs`'s `Palette`). A color a
/// program redefined via OSC 4/10/11/12 still affects how cells are drawn:
/// `TerminalFrame::palette_overrides` carries that sparse index→RGB table,
/// and the view consults it before the theme. The engine answers a
/// program's color queries from its own palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalColor {
    Named(NamedColor),
    Indexed(u8),
    Rgb([u8; 3]),
}

/// One of the fixed named color roles a cell can carry: what
/// `core::render` (`SpanStyle::of`, `promoted`) produces from the engine's
/// styles:
///
/// - The 8 base ANSI hues (`Black`..`White`) and their bright forms
///   (`BrightBlack`..`BrightWhite`) reach frames directly from SGR
///   30-37/90-97, or as the bold-promoted form of a base hue.
/// - Dim forms (`DimBlack`..`DimWhite`) are *not* a normal SGR target —
///   they only ever appear as the faint-promoted form of a base hue.
/// - `Foreground`/`Background`/`Cursor` are the default-role slots; only
///   `Foreground` is ever bold/dim-promoted this way too, producing
///   `BrightForeground`/`DimForeground`. Under `INVERSE` a cell's two
///   colors change places after the promotion, so a promoted form can be
///   a cell's background and `Background` its text's color.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// This role's index in `TerminalFrame::palette_overrides`: 0..=15 for
    /// the base ANSI hues (incl. bright), 256/257/258 for the default
    /// foreground/background/cursor roles — the only colors OSC 4/10/11/12
    /// set (see `core::render::palette_overrides`). The bold/dim-promoted roles
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
