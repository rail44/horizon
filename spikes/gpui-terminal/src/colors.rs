//! Logical-color resolution for the spike, mirroring the semantics of
//! `horizon::terminal::view::color::resolve_color` (palette overrides win,
//! then the scheme, with the 256-color cube/grayscale computed). The scheme
//! values are Horizon's built-in defaults (`ui::theme` chrome defaults +
//! `ui::theme::ansi`'s 16-slot palette) hardcoded, so the spike looks like
//! Horizon's own terminal with no config file.

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use gpui::{rgb, Hsla};
use horizon_terminal_core::TerminalColor;

pub const BACKGROUND: u32 = 0x16181d; // SURFACE_BASE_DEFAULT
const FOREGROUND: u32 = 0xe9ecf2; // TEXT_PRIMARY_DEFAULT
const CURSOR: u32 = 0x84dcc6; // ACCENT_DEFAULT

const ANSI16: [u32; 16] = [
    0x23262e, // black
    0xe06c75, // red
    0x98c379, // green
    0xe5c07b, // yellow
    0x61afef, // blue
    0xc678dd, // magenta
    0x56b6c2, // cyan
    0xdee2ea, // white
    0x5f6370, // bright black
    0xff7b7f, // bright red
    0xb5d68c, // bright green
    0xf5d38b, // bright yellow
    0x78c2ff, // bright blue
    0xda8cff, // bright magenta
    0x67cdd8, // bright cyan
    0xffffff, // bright white
];

pub fn to_hsla(rgb888: [u8; 3]) -> Hsla {
    rgb(((rgb888[0] as u32) << 16) | ((rgb888[1] as u32) << 8) | rgb888[2] as u32).into()
}

pub fn resolve(color: TerminalColor, overrides: &[(u16, [u8; 3])]) -> [u8; 3] {
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
        TerminalColor::Indexed(index) => indexed_rgb(index),
        TerminalColor::Named(named) => named_rgb(named),
    }
}

fn split(value: u32) -> [u8; 3] {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

fn named_rgb(color: NamedColor) -> [u8; 3] {
    match color {
        NamedColor::Black => split(ANSI16[0]),
        NamedColor::Red => split(ANSI16[1]),
        NamedColor::Green => split(ANSI16[2]),
        NamedColor::Yellow => split(ANSI16[3]),
        NamedColor::Blue => split(ANSI16[4]),
        NamedColor::Magenta => split(ANSI16[5]),
        NamedColor::Cyan => split(ANSI16[6]),
        NamedColor::White => split(ANSI16[7]),
        NamedColor::DimWhite => [170, 176, 190],
        NamedColor::BrightBlack | NamedColor::DimBlack => split(ANSI16[8]),
        NamedColor::BrightRed | NamedColor::DimRed => split(ANSI16[9]),
        NamedColor::BrightGreen | NamedColor::DimGreen => split(ANSI16[10]),
        NamedColor::BrightYellow | NamedColor::DimYellow => split(ANSI16[11]),
        NamedColor::BrightBlue | NamedColor::DimBlue => split(ANSI16[12]),
        NamedColor::BrightMagenta | NamedColor::DimMagenta => split(ANSI16[13]),
        NamedColor::BrightCyan | NamedColor::DimCyan => split(ANSI16[14]),
        NamedColor::BrightWhite => split(ANSI16[15]),
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            split(FOREGROUND)
        }
        NamedColor::Background => split(BACKGROUND),
        NamedColor::Cursor => split(CURSOR),
    }
}

fn indexed_rgb(index: u8) -> [u8; 3] {
    if index < 16 {
        return split(ANSI16[index as usize]);
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
