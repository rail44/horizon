//! Horizon's color scheme: derived from a seed (`surface_base`, `accent`,
//! `text_contrast`, the ANSI hues) as packed `0xRRGGBB` colors, with the
//! color math that derivation and its readers share. The seed is taken as
//! a value ([`written::Written`]): this crate reads no configuration file
//! and writes none. No UI framework's types appear here. The design is
//! `docs/theme-design.md`.

pub mod oklab;
pub mod palette;
pub mod scheme;
pub mod seed;
pub mod spec;
#[cfg(test)]
mod test_support;
pub mod warnings;
pub mod written;
