//! Default RGB resolution for OSC 4/10/11/12 color-query responses
//! (`Event::ColorRequest`, wired in `core/events.rs` and resolved in
//! `TerminalCore::write_vt`).
//!
//! `core::render` had an equivalent table (`resolve_color`/`named_rgb`/
//! `indexed_rgb`) for painting cells, but that one moved to the host
//! (now `theme::resolve` in `src/theme/ansi.rs`;
//! `docs/session-daemon-design.md` decision 8) once cell colors stopped
//! needing an immediate RGB answer at all. This module's
//! table stays here because a color *query reply* is a byte sequence sent
//! back to the app right now — it cannot be deferred to a UI paint the way
//! a cell's color can — so it keeps its own copy, resolved against
//! [`TerminalColorScheme`] rather than a live theme this crate has no
//! access to. The live OSC 4/10/11/12 overrides (`overrides` below) also
//! ride the frame as `TerminalFrame::palette_overrides` so the host's own
//! cell-rendering resolution can honor them too — this module's read of
//! `Term::colors()` and that snapshot are two independent consumers of the
//! same underlying state, not a narrowing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Crate-local mirror of the host's theme color roles (the shell's
/// `src/theme/`). This crate has no dependency on the shell's theme or its
/// UI toolkit at all (`docs/session-daemon-design.md` decision 9), so
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalColorScheme {
    #[schemars(with = "RgbSchema")]
    pub foreground: Rgb,
    #[schemars(with = "RgbSchema")]
    pub background: Rgb,
    #[schemars(with = "RgbSchema")]
    pub cursor: Rgb,
    #[schemars(with = "RgbSchema")]
    pub black: Rgb,
    #[schemars(with = "RgbSchema")]
    pub red: Rgb,
    #[schemars(with = "RgbSchema")]
    pub green: Rgb,
    #[schemars(with = "RgbSchema")]
    pub yellow: Rgb,
    #[schemars(with = "RgbSchema")]
    pub blue: Rgb,
    #[schemars(with = "RgbSchema")]
    pub magenta: Rgb,
    #[schemars(with = "RgbSchema")]
    pub cyan: Rgb,
    #[schemars(with = "RgbSchema")]
    pub white: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_black: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_red: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_green: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_yellow: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_blue: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_magenta: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_cyan: Rgb,
    #[schemars(with = "RgbSchema")]
    pub bright_white: Rgb,
}

/// The wire schema's [`Rgb`]: a plain `{r, g, b}` u8 struct, kept in sync
/// with its serde shape.
#[derive(JsonSchema)]
#[schemars(rename = "Rgb")]
#[expect(dead_code)]
struct RgbSchema {
    r: u8,
    g: u8,
    b: u8,
}

impl TerminalColorScheme {
    /// The scheme of a theme given as plain colors (red, green, blue): the
    /// foreground, the background, the cursor and the sixteen colors a
    /// program names by number.
    pub fn from_rgb(
        foreground: [u8; 3],
        background: [u8; 3],
        cursor: [u8; 3],
        ansi: [[u8; 3]; 16],
    ) -> Self {
        let rgb = |[r, g, b]: [u8; 3]| Rgb { r, g, b };
        Self {
            foreground: rgb(foreground),
            background: rgb(background),
            cursor: rgb(cursor),
            black: rgb(ansi[0]),
            red: rgb(ansi[1]),
            green: rgb(ansi[2]),
            yellow: rgb(ansi[3]),
            blue: rgb(ansi[4]),
            magenta: rgb(ansi[5]),
            cyan: rgb(ansi[6]),
            white: rgb(ansi[7]),
            bright_black: rgb(ansi[8]),
            bright_red: rgb(ansi[9]),
            bright_green: rgb(ansi[10]),
            bright_yellow: rgb(ansi[11]),
            bright_blue: rgb(ansi[12]),
            bright_magenta: rgb(ansi[13]),
            bright_cyan: rgb(ansi[14]),
            bright_white: rgb(ansi[15]),
        }
    }
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

impl TerminalColorScheme {
    /// The sixteen colors of the palette, in its order.
    pub(super) fn ansi(&self) -> [Rgb; 16] {
        [
            self.black,
            self.red,
            self.green,
            self.yellow,
            self.blue,
            self.magenta,
            self.cyan,
            self.white,
            self.bright_black,
            self.bright_red,
            self.bright_green,
            self.bright_yellow,
            self.bright_blue,
            self.bright_magenta,
            self.bright_cyan,
            self.bright_white,
        ]
    }
}

/// A color of the scheme.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
