//! Logical-color resolution, mirroring the semantics of the Floem
//! shell's `terminal::view::color::resolve_color` (palette overrides
//! win, then the scheme, with the 256-color cube/grayscale computed).
//! The scheme is loaded from the shared config crate (`[theme]` +
//! `[theme.ansi]` in config.toml) over Horizon's built-in defaults,
//! and `Reload Config` swaps it live via [`reload_from`].

use std::sync::{OnceLock, RwLock};

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use gpui::{rgb, Hsla};
use horizon_config::RawConfig;
use horizon_terminal_core::TerminalColor;

const BACKGROUND_DEFAULT: u32 = 0x16181d; // SURFACE_BASE_DEFAULT
const FOREGROUND_DEFAULT: u32 = 0xe9ecf2; // TEXT_PRIMARY_DEFAULT
const CURSOR_DEFAULT: u32 = 0x84dcc6; // ACCENT_DEFAULT

const ANSI16_DEFAULT: [u32; 16] = [
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

#[derive(Clone, Copy)]
struct Scheme {
    background: u32,
    foreground: u32,
    cursor: u32,
    ansi: [u32; 16],
}

fn scheme_from(raw: &RawConfig) -> Scheme {
    let chrome = |key: &str, fallback_key: Option<&str>, default: u32| {
        raw.theme
            .colors
            .get(key)
            .or_else(|| fallback_key.and_then(|key| raw.theme.colors.get(key)))
            .and_then(|value| parse_hex(value))
            .unwrap_or(default)
    };
    let ansi_slot = |value: &Option<String>, default: u32| {
        value.as_deref().and_then(parse_hex).unwrap_or(default)
    };
    let ansi_raw = &raw.theme.ansi;
    Scheme {
        background: chrome(
            "terminal_background",
            Some("surface_base"),
            BACKGROUND_DEFAULT,
        ),
        foreground: chrome(
            "terminal_foreground",
            Some("text_primary"),
            FOREGROUND_DEFAULT,
        ),
        cursor: chrome("terminal_cursor", Some("accent"), CURSOR_DEFAULT),
        ansi: [
            ansi_slot(&ansi_raw.black, ANSI16_DEFAULT[0]),
            ansi_slot(&ansi_raw.red, ANSI16_DEFAULT[1]),
            ansi_slot(&ansi_raw.green, ANSI16_DEFAULT[2]),
            ansi_slot(&ansi_raw.yellow, ANSI16_DEFAULT[3]),
            ansi_slot(&ansi_raw.blue, ANSI16_DEFAULT[4]),
            ansi_slot(&ansi_raw.magenta, ANSI16_DEFAULT[5]),
            ansi_slot(&ansi_raw.cyan, ANSI16_DEFAULT[6]),
            ansi_slot(&ansi_raw.white, ANSI16_DEFAULT[7]),
            ansi_slot(&ansi_raw.bright_black, ANSI16_DEFAULT[8]),
            ansi_slot(&ansi_raw.bright_red, ANSI16_DEFAULT[9]),
            ansi_slot(&ansi_raw.bright_green, ANSI16_DEFAULT[10]),
            ansi_slot(&ansi_raw.bright_yellow, ANSI16_DEFAULT[11]),
            ansi_slot(&ansi_raw.bright_blue, ANSI16_DEFAULT[12]),
            ansi_slot(&ansi_raw.bright_magenta, ANSI16_DEFAULT[13]),
            ansi_slot(&ansi_raw.bright_cyan, ANSI16_DEFAULT[14]),
            ansi_slot(&ansi_raw.bright_white, ANSI16_DEFAULT[15]),
        ],
    }
}

/// `#rgb` / `#rrggbb` → packed 0xRRGGBB.
fn parse_hex(value: &str) -> Option<u32> {
    let hex = value.trim().strip_prefix('#')?;
    match hex.len() {
        3 => {
            let value = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((value >> 8) & 0xf, (value >> 4) & 0xf, value & 0xf);
            Some((r * 0x11) << 16 | (g * 0x11) << 8 | (b * 0x11))
        }
        6 => u32::from_str_radix(hex, 16).ok(),
        _ => None,
    }
}

fn scheme_store() -> &'static RwLock<Scheme> {
    static STORE: OnceLock<RwLock<Scheme>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(scheme_from(horizon_config::load())))
}

fn scheme() -> Scheme {
    *scheme_store().read().unwrap()
}

/// Applies a re-read config's `[theme]` live — the GPUI half of the
/// `Reload Config` command (the caller refreshes the window after).
pub fn reload_from(raw: &RawConfig) {
    *scheme_store().write().unwrap() = scheme_from(raw);
}

pub fn background() -> u32 {
    scheme().background
}

/// The core-side scheme for OSC 4/10/11/12 query replies, mirrored from
/// the same values the view paints with.
pub fn terminal_color_scheme() -> horizon_terminal_core::TerminalColorScheme {
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
