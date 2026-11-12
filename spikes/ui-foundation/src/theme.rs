//! Horizon's theme and its type scale on this stack, as CSS variables.
//!
//! The colors are Horizon's own: the code that derives a scheme from the
//! config file's seed (`src/theme/scheme.rs`, with `palette.rs`, `oklab.rs`
//! and `warnings.rs`) is read as it is. Of gpui it names three color types,
//! which `vendor/gpui-colors` stands in for. The measurements are the
//! table of `src/ui/spec.rs`, written out again here: that file is in
//! gpui's units and cannot be read in.
//!
//! A view's CSS names the variables; a change of theme or of font size is
//! a new set of them on the element the views are under.

use crate::{El, el};

// Horizon's files, under a module that stands where `src/theme/` does so
// that each finds the others (and its own tests' file) where it expects.
// (Not formatted from here: they are Horizon's files, in its edition's
// style.)
#[rustfmt::skip]
#[allow(dead_code, unused_imports)]
#[path = "../../../src/theme"]
mod horizon {
    pub(super) mod oklab;
    pub(super) mod palette;
    pub(super) mod scheme;
    pub(super) mod warnings;
    // This spike's own file, here because what it reads of the scheme is
    // open to the theme's own modules only.
    #[path = "../../spikes/ui-foundation/src/theme_look.rs"]
    pub mod look;
}
pub use horizon::look::Theme;

pub const CSS: &str = "
    .themed { height: 100%; background: var(--background); color: var(--text);
              font-family: var(--font-family); font-size: var(--body-size);
              line-height: var(--body-line); }
";

/// Puts `view` under an element that carries `theme`'s variables, and
/// carries the next ones when `theme` gives another: a change of theme or
/// of font size is this one attribute written again.
pub fn themed(theme: impl Fn() -> Theme + 'static, view: El) -> El {
    el("div")
        .class("themed")
        .attr_dyn("style", move || theme().variables())
        .child(view)
}
