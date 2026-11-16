//! Seeds as written, for the theme's tests.

use super::scheme::{scheme_from, Scheme};
use super::written::Written;

/// The seed with `colors` written: `surface_base` and `accent`.
pub fn config_with(colors: &[(&str, &str)]) -> Written {
    let mut written = Written::default();
    for (key, value) in colors {
        let value = Some((*value).to_string());
        match *key {
            "surface_base" => written.surface_base = value,
            "accent" => written.accent = value,
            other => panic!("config_with: not a key of the seed: {other}"),
        }
    }
    written
}

/// [`config_with`] plus a `text_contrast`.
pub fn config_with_and_contrast(colors: &[(&str, &str)], text_contrast: Option<f64>) -> Written {
    let mut config = config_with(colors);
    config.text_contrast = text_contrast;
    config
}

/// [`config_with`] plus hues.
pub fn config_with_ansi(colors: &[(&str, &str)], ansi: &[(&str, &str)]) -> Written {
    let mut config = config_with(colors);
    for (slot, value) in ansi {
        let value = Some((*value).to_string());
        match *slot {
            "red" => config.ansi.red = value,
            "green" => config.ansi.green = value,
            "yellow" => config.ansi.yellow = value,
            "blue" => config.ansi.blue = value,
            "magenta" => config.ansi.magenta = value,
            "cyan" => config.ansi.cyan = value,
            other => panic!("config_with_ansi: unsupported test slot {other}"),
        }
    }
    config
}

/// A light seed with every value written, which most of these tests
/// measure against (`docs/theme-design.md`): every role but the seed's own
/// is derived.
pub fn owner_seeded_light_scheme() -> Scheme {
    let mut config = config_with_ansi(
        &[("surface_base", "#f6f6f6"), ("accent", "blue")],
        &[
            ("red", "#b03b4c"),
            ("green", "#00b312"),
            ("yellow", "#87b03b"),
            ("blue", "#0048b3"),
            ("magenta", "#643bb0"),
            ("cyan", "#3bb09e"),
        ],
    );
    config.text_contrast = Some(5.3);
    scheme_from(&config)
}
