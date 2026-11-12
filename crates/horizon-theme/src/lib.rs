//! Horizon's color scheme: derived from the config file's seed
//! (`surface_base`, `accent`, `text_contrast`, the ANSI hues) as packed
//! `0xRRGGBB` colors, with the color math that derivation and its readers
//! share. No UI framework's types appear here; a shell reads [`scheme`] and
//! converts. The design is `docs/theme-design.md`.

pub mod oklab;
pub mod palette;
pub mod save;
pub mod scheme;
pub mod seed;
pub mod spec;
/// Config fixtures for theme tests, here and in the shells.
pub mod test_support;
pub mod warnings;
