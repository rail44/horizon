//! Shared `#[cfg(test)]` config-fixture builders used across every
//! `src/theme/*.rs` test module -- factored out here rather than
//! duplicated per file since `owner_seeded_light_scheme` in particular
//! encodes the owner's real, current `~/.config/horizon/config.toml`
//! (`docs/theme-design.md`), the primary fixture most of this crate's
//! theme tests measure against.

use std::collections::HashMap;

use horizon_config::{RawConfig, RawThemeConfig};

use super::scheme::{scheme_from, Scheme};

pub(super) fn config_with(colors: &[(&str, &str)]) -> RawConfig {
    RawConfig {
        theme: RawThemeConfig {
            colors: colors
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<HashMap<_, _>>(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// [`config_with`] plus an explicit `text_contrast` (the seed knob,
/// not part of `colors` -- it's `RawThemeConfig`'s own typed field).
pub(super) fn config_with_and_contrast(
    colors: &[(&str, &str)],
    text_contrast: Option<f64>,
) -> RawConfig {
    let mut config = config_with(colors);
    config.theme.text_contrast = text_contrast;
    config
}

/// [`config_with`] plus `[theme.ansi]` hue overrides -- `ansi` is a
/// nested typed struct, not part of the flattened `colors` map, so it
/// needs its own setter rather than a `("ansi.red", ...)` entry.
pub(super) fn config_with_ansi(colors: &[(&str, &str)], ansi: &[(&str, &str)]) -> RawConfig {
    let mut config = config_with(colors);
    for (slot, value) in ansi {
        let value = Some((*value).to_string());
        match *slot {
            "red" => config.theme.ansi.red = value,
            "green" => config.theme.ansi.green = value,
            "yellow" => config.theme.ansi.yellow = value,
            "blue" => config.theme.ansi.blue = value,
            "magenta" => config.theme.ansi.magenta = value,
            "cyan" => config.theme.ansi.cyan = value,
            other => panic!("config_with_ansi: unsupported test slot {other}"),
        }
    }
    config
}

/// The owner's actual, CURRENT `~/.config/horizon/config.toml`
/// `[theme]` (2026-07-16): the seed-only form (`docs/theme-design.md`'s
/// slice B1, made the ONLY form by that doc's 2026-07-16 "config
/// narrowed to the seed" decision). This is the primary owner fixture
/// for this whole test module -- the pre-seed flat-hex fixture that
/// used to sit here (`owner_light_colors`/`owner_light_scheme`,
/// testing role-key override passthrough) was retired alongside the
/// override layer itself; every test that used it now either uses
/// this fixture instead or was retired with it. Every non-seed role
/// (`foreground`, `text_muted`, `text_subtle`, `surface_panel`,
/// `surface_selected`, `border`, `danger`/`warning`/`success`/`info`,
/// ...) is DERIVED here, not set explicitly -- there's no other way
/// to set them any more.
fn owner_seeded_light_colors() -> Vec<(&'static str, &'static str)> {
    vec![("surface_base", "#f6f6f6"), ("accent", "blue")]
}

/// [`owner_seeded_light_colors`] resolved, with `text_contrast = 5.3`
/// and the owner's actual `[theme.ansi]` overrides (`red`/`green`/
/// `yellow`/`blue`/`magenta`/`cyan` -- `config_with_ansi` doesn't cover
/// `black`/`white`/`bright_*`, left unset like the real config).
pub(super) fn owner_seeded_light_scheme() -> Scheme {
    let mut config = config_with_ansi(
        &owner_seeded_light_colors(),
        &[
            ("red", "#b03b4c"),
            ("green", "#00b312"),
            ("yellow", "#87b03b"),
            ("blue", "#0048b3"),
            ("magenta", "#643bb0"),
            ("cyan", "#3bb09e"),
        ],
    );
    config.theme.text_contrast = Some(5.3);
    scheme_from(&config)
}
