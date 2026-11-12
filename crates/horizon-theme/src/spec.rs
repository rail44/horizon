//! The measurements every view draws to, as plain numbers: the type
//! scale, the tints, and the font's defaults and bounds. Each shell puts
//! them in its own units.
//!
//! The table is designed at a body size of [`REFERENCE_BODY`] px and
//! scaled from the font size in use, each scaled value rounded to a whole
//! pixel. At the default size of 13 the body is 13px and the other steps
//! round from the 14-based ratios: t1 16/24, t2 14/22, body 13/24, meta
//! 11/17, micro 10/15.

/// The body size the table is designed at.
pub const REFERENCE_BODY: f32 = 14.0;

/// A measurement designed at the reference body size, at `font_size`.
pub fn scaled(at_reference: f32, font_size: f32) -> f32 {
    (at_reference * font_size / REFERENCE_BODY).round()
}

/// One step of the type scale: a size and the line height that goes with
/// it, at the reference body size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub size: f32,
    pub line_height: f32,
}

/// A title: the task title on the board.
pub const T1: Step = Step {
    size: 17.0,
    line_height: 26.0,
};

/// A heading inside running text.
pub const T2: Step = Step {
    size: 15.0,
    line_height: 24.0,
};

/// Running text, and a list row's title.
pub const BODY: Step = Step {
    size: REFERENCE_BODY,
    line_height: 26.0,
};

/// Times, counters, and secondary labels.
pub const META: Step = Step {
    size: 12.0,
    line_height: 18.0,
};

/// Chips and the smallest counters.
pub const MICRO: Step = Step {
    size: 11.0,
    line_height: 16.0,
};

/// The tint a header band and an owner reply sit on.
pub const BAND_TINT: f32 = 0.03;

/// A selected row's full-bleed tint.
pub const SELECTION_TINT: f32 = 0.07;

/// A status chip's tinted background.
pub const CHIP_TINT: f32 = 0.14;

/// The font when the config names none.
#[cfg(target_os = "macos")]
pub const DEFAULT_FONT_FAMILY: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// The built-in default for `[terminal] font_size`, which
/// `config.example.toml` documents.
pub const DEFAULT_FONT_SIZE: f32 = 13.0;

/// The px step `Increase Font Size`/`Decrease Font Size` move the size by.
pub const FONT_SIZE_STEP: f32 = 1.0;

/// The smallest size the font-size commands can reach, in px: below this
/// the terminal grid is unreadable rather than merely small.
pub const MIN_FONT_SIZE: f32 = 5.0;

/// The largest size the font-size commands can reach, in px.
pub const MAX_FONT_SIZE: f32 = 40.0;
