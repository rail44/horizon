//! The theme's seed as a configuration has it written: each value there or
//! not, and not yet read. Whether anything is written at all is part of
//! what a scheme is derived from, so this is what the derivation takes;
//! whoever reads a configuration file fills it in.

/// The seed as written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Written {
    /// A hex color.
    pub surface_base: Option<String>,
    /// A hex color, or the name of one of the six hues.
    pub accent: Option<String>,
    pub text_contrast: Option<f64>,
    pub ansi: WrittenAnsi,
}

/// The six hues as written, each a hex color.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WrittenAnsi {
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub blue: Option<String>,
    pub magenta: Option<String>,
    pub cyan: Option<String>,
}
