//! The seed the theme settings view edits -- exactly the knobs
//! `docs/theme-design.md`'s `scheme_from` derives everything else from:
//! `surface_base`, the six `[theme.ansi]` hues, `accent`, and
//! `text_contrast`. Pure data plus the one conversion the live-apply path
//! needs ([`Seed::to_raw_config`]) -- no GPUI here, so this is unit-tested
//! directly in this module's own `#[cfg(test)] mod tests` below.

use std::collections::HashMap;

use alacritty_terminal::vte::ansi::Rgb;
use gpui::Hsla;
use horizon_config::{RawConfig, RawThemeAnsiConfig, RawThemeConfig};

use crate::theme::{self, hex, parse_hex, TEXT_CONTRAST_CEIL, TEXT_CONTRAST_FLOOR};

/// `Rgb` (three `u8` channels) -> packed `0xRRGGBB`, matching every other
/// packed-color representation this module and `theme.rs` share. `pub(crate)`
/// so the view (`super::mod`) can pack `terminal_color_scheme()`'s `Rgb`
/// fields into a [`ResolvedFallback`].
pub(crate) fn pack_rgb(rgb: Rgb) -> u32 {
    ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32
}

/// The currently-resolved values [`Seed::from_current_config`] falls back
/// to for any seed key the raw config leaves unset. Read from the live
/// `theme::` scheme by the caller (the view, at pane-construction time) and
/// passed in explicitly -- rather than this module reaching into
/// `theme::background()`/`theme::accent()`/`theme::terminal_color_scheme()`
/// itself -- so `from_current_config` stays a pure function of its two
/// arguments and is fully unit-testable without touching process-global
/// theme state (which, notably, is *not* reliably `RawConfig::default()`
/// even under `#[cfg(test)]`: `horizon_config::load()`'s own test-mode gate
/// only fires when *that crate* is compiled under test, not when it's a
/// plain dependency of this crate's test binary -- see `theme.rs`'s own
/// tests, which all call `scheme_from(&RawConfig::default())` directly for
/// exactly this reason).
#[derive(Clone, Copy)]
pub(crate) struct ResolvedFallback {
    pub(crate) surface_base: u32,
    /// Indexed by [`HueSlot::ALL`]'s order.
    pub(crate) hues: [u32; 6],
    pub(crate) accent: u32,
}

/// Packed `0xRRGGBB` -> `Hsla`, the exact inverse of `theme::packed_from_hsla`
/// -- used by the view (`super::mod`) to seed each `ColorPickerState`'s
/// `default_value` from this module's packed `u32` representation.
pub(crate) fn u32_to_hsla(value: u32) -> Hsla {
    theme::to_hsla([(value >> 16) as u8, (value >> 8) as u8, value as u8])
}

/// One of the six `[theme.ansi]` hue slots that double as the seed's own
/// hue set -- also `accent`'s six recognized slot-name spellings
/// (`resolve_accent` in `src/theme.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HueSlot {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl HueSlot {
    /// Config-file / display order -- matches `RawThemeAnsiConfig`'s field
    /// order and `config.example.toml`'s `[theme.ansi]` table.
    pub(crate) const ALL: [HueSlot; 6] = [
        HueSlot::Red,
        HueSlot::Green,
        HueSlot::Yellow,
        HueSlot::Blue,
        HueSlot::Magenta,
        HueSlot::Cyan,
    ];

    /// The `[theme.ansi]` key name -- also a valid `accent` slot-name
    /// spelling.
    pub(crate) fn config_key(self) -> &'static str {
        match self {
            HueSlot::Red => "red",
            HueSlot::Green => "green",
            HueSlot::Yellow => "yellow",
            HueSlot::Blue => "blue",
            HueSlot::Magenta => "magenta",
            HueSlot::Cyan => "cyan",
        }
    }

    /// A capitalized label for the picker/select UI.
    pub(crate) fn label(self) -> &'static str {
        match self {
            HueSlot::Red => "Red",
            HueSlot::Green => "Green",
            HueSlot::Yellow => "Yellow",
            HueSlot::Blue => "Blue",
            HueSlot::Magenta => "Magenta",
            HueSlot::Cyan => "Cyan",
        }
    }

    /// The inverse of [`HueSlot::config_key`] -- accepts exactly the same
    /// spellings `resolve_accent` (`src/theme.rs`) does for `accent`'s
    /// slot-name form.
    pub(crate) fn from_config_key(value: &str) -> Option<HueSlot> {
        HueSlot::ALL
            .into_iter()
            .find(|slot| slot.config_key() == value)
    }
}

/// `accent`'s value: either a reference to one of the six [`HueSlot`]s, or
/// a direct hex color -- the same two spellings `resolve_accent`
/// (`src/theme.rs`) accepts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AccentValue {
    Slot(HueSlot),
    Hex(u32),
}

impl AccentValue {
    /// The `[theme]` `accent` value this resolves to, as written to
    /// config: the slot name for [`AccentValue::Slot`], a `#rrggbb` string
    /// for [`AccentValue::Hex`].
    fn config_value(self) -> String {
        match self {
            AccentValue::Slot(slot) => slot.config_key().to_string(),
            AccentValue::Hex(value) => hex(value),
        }
    }
}

/// The full seed: every knob the theme settings view exposes, and the one
/// `scheme_from` (`src/theme.rs`) derives the rest of the theme from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Seed {
    pub(crate) surface_base: u32,
    /// Indexed by [`HueSlot`]'s [`HueSlot::ALL`] order.
    pub(crate) hues: [u32; 6],
    pub(crate) accent: AccentValue,
    /// Clamped to `[TEXT_CONTRAST_FLOOR, TEXT_CONTRAST_CEIL]` by every
    /// constructor below -- never stored out of range.
    pub(crate) text_contrast: f64,
}

impl Seed {
    pub(crate) fn hue(&self, slot: HueSlot) -> u32 {
        self.hues[HueSlot::ALL.iter().position(|s| *s == slot).unwrap()]
    }

    pub(crate) fn with_hue(mut self, slot: HueSlot, value: u32) -> Self {
        let index = HueSlot::ALL.iter().position(|s| *s == slot).unwrap();
        self.hues[index] = value;
        self
    }

    /// Reads the current seed from the owner's loaded config
    /// (`horizon_config::load()`'s `[theme]`/`[theme.ansi]` fields), falling
    /// back to `fallback` (the currently-resolved scheme role, read by the
    /// caller) for anything unset -- opening the theme settings pane always
    /// shows exactly what's on screen right now, whether that came from the
    /// config file or a built-in default.
    pub(crate) fn from_current_config(raw: &RawConfig, fallback: ResolvedFallback) -> Seed {
        let surface_base = raw
            .theme
            .colors
            .get("surface_base")
            .and_then(|value| parse_hex(value))
            .unwrap_or(fallback.surface_base);

        let ansi_slot = |raw_value: &Option<String>, resolved: u32| {
            raw_value.as_deref().and_then(parse_hex).unwrap_or(resolved)
        };
        let hues = [
            ansi_slot(&raw.theme.ansi.red, fallback.hues[0]),
            ansi_slot(&raw.theme.ansi.green, fallback.hues[1]),
            ansi_slot(&raw.theme.ansi.yellow, fallback.hues[2]),
            ansi_slot(&raw.theme.ansi.blue, fallback.hues[3]),
            ansi_slot(&raw.theme.ansi.magenta, fallback.hues[4]),
            ansi_slot(&raw.theme.ansi.cyan, fallback.hues[5]),
        ];

        let accent = match raw.theme.colors.get("accent").map(|value| value.trim()) {
            Some(name) if HueSlot::from_config_key(name).is_some() => {
                AccentValue::Slot(HueSlot::from_config_key(name).unwrap())
            }
            // Unset, or an explicit hex, or an unparsable value that fell
            // back to a built-in default -- in every case, the currently
            // resolved accent is the right thing to preload the custom
            // color picker with.
            _ => AccentValue::Hex(fallback.accent),
        };

        let text_contrast = raw
            .theme
            .text_contrast
            .filter(|value| value.is_finite())
            .unwrap_or(TEXT_CONTRAST_DEFAULT_FOR_SEED)
            .clamp(TEXT_CONTRAST_FLOOR, TEXT_CONTRAST_CEIL);

        Seed {
            surface_base,
            hues,
            accent,
            text_contrast,
        }
    }

    /// Clamps `value` into `[TEXT_CONTRAST_FLOOR, TEXT_CONTRAST_CEIL]` --
    /// the contrast slider's own change handler (`super::mod`) uses this so
    /// a slider drag can never push the stored seed out of range, even
    /// though the slider's own UI range (a "sensible" ~4.5-15, narrower
    /// than the full derivation ceiling of 21) never actually reaches the
    /// upper bound itself.
    pub(crate) fn clamp_contrast(value: f64) -> f64 {
        if value.is_finite() {
            value.clamp(TEXT_CONTRAST_FLOOR, TEXT_CONTRAST_CEIL)
        } else {
            TEXT_CONTRAST_DEFAULT_FOR_SEED
        }
    }

    /// Builds a `RawConfig`-shaped value that, fed through
    /// `theme::reload_from`, derives the whole scheme from exactly this
    /// seed: the same shape `scheme_from` reads (`raw.theme`), with every
    /// other `RawConfig` section left at its default -- nothing else in the
    /// seed's derivation reads them.
    pub(crate) fn to_raw_config(self) -> RawConfig {
        let mut colors = HashMap::new();
        colors.insert("surface_base".to_string(), hex(self.surface_base));
        colors.insert("accent".to_string(), self.accent.config_value());

        RawConfig {
            theme: RawThemeConfig {
                ansi: RawThemeAnsiConfig {
                    red: Some(hex(self.hue(HueSlot::Red))),
                    green: Some(hex(self.hue(HueSlot::Green))),
                    yellow: Some(hex(self.hue(HueSlot::Yellow))),
                    blue: Some(hex(self.hue(HueSlot::Blue))),
                    magenta: Some(hex(self.hue(HueSlot::Magenta))),
                    cyan: Some(hex(self.hue(HueSlot::Cyan))),
                    ..Default::default()
                },
                text_contrast: Some(self.text_contrast),
                colors,
            },
            ..Default::default()
        }
    }

    /// The `accent` config value this seed would write/apply -- exposed for
    /// [`super::save`] so it doesn't need to know about [`AccentValue`]'s
    /// internals.
    pub(crate) fn accent_config_value(&self) -> String {
        self.accent.config_value()
    }
}

/// Duplicated from `theme::TEXT_CONTRAST_DEFAULT` rather than importing it:
/// that constant is `pub(crate)` for its floor/ceiling siblings' sake, but
/// importing the default too would blur which module owns "what happens
/// when unset" -- here, unset always means "whatever's currently resolved
/// and on screen" ([`Seed::from_current_config`]), never this literal
/// number; it only matters as [`Seed::clamp_contrast`]'s fallback for a
/// non-finite drag value, which should never happen via the stock
/// `Slider`'s own min/max clamp in the first place.
const TEXT_CONTRAST_DEFAULT_FOR_SEED: f64 = 15.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_seed() -> Seed {
        Seed {
            surface_base: 0xf6f6f6,
            hues: [0xb03b4c, 0x008300, 0x577c00, 0x0048b3, 0x643bb0, 0x007f6e],
            accent: AccentValue::Slot(HueSlot::Blue),
            text_contrast: 5.3,
        }
    }

    #[test]
    fn to_raw_config_carries_every_seed_key() {
        let raw = sample_seed().to_raw_config();
        assert_eq!(raw.theme.colors.get("surface_base").unwrap(), "#f6f6f6");
        assert_eq!(raw.theme.colors.get("accent").unwrap(), "blue");
        assert_eq!(raw.theme.text_contrast, Some(5.3));
        assert_eq!(raw.theme.ansi.red.as_deref(), Some("#b03b4c"));
        assert_eq!(raw.theme.ansi.green.as_deref(), Some("#008300"));
        assert_eq!(raw.theme.ansi.yellow.as_deref(), Some("#577c00"));
        assert_eq!(raw.theme.ansi.blue.as_deref(), Some("#0048b3"));
        assert_eq!(raw.theme.ansi.magenta.as_deref(), Some("#643bb0"));
        assert_eq!(raw.theme.ansi.cyan.as_deref(), Some("#007f6e"));
        // Every other RawConfig section stays at its default -- nothing in
        // the seed's derivation reads them.
        assert_eq!(raw.provider, Default::default());
        assert_eq!(raw.terminal, Default::default());
        assert_eq!(raw.ui, Default::default());
        assert!(raw.keybindings.is_empty());
    }

    #[test]
    fn to_raw_config_writes_hex_accent_directly() {
        let seed = Seed {
            accent: AccentValue::Hex(0x84dcc6),
            ..sample_seed()
        };
        let raw = seed.to_raw_config();
        assert_eq!(raw.theme.colors.get("accent").unwrap(), "#84dcc6");
    }

    #[test]
    fn clamp_contrast_floors_and_ceils() {
        assert_eq!(Seed::clamp_contrast(1.0), TEXT_CONTRAST_FLOOR);
        assert_eq!(Seed::clamp_contrast(100.0), TEXT_CONTRAST_CEIL);
        assert_eq!(Seed::clamp_contrast(10.0), 10.0);
    }

    #[test]
    fn clamp_contrast_falls_back_on_non_finite() {
        assert_eq!(
            Seed::clamp_contrast(f64::NAN),
            TEXT_CONTRAST_DEFAULT_FOR_SEED
        );
        assert_eq!(
            Seed::clamp_contrast(f64::INFINITY),
            TEXT_CONTRAST_DEFAULT_FOR_SEED
        );
    }

    #[test]
    fn hue_slot_config_key_round_trips() {
        for slot in HueSlot::ALL {
            assert_eq!(HueSlot::from_config_key(slot.config_key()), Some(slot));
        }
        assert_eq!(HueSlot::from_config_key("not-a-slot"), None);
    }

    #[test]
    fn with_hue_updates_only_the_named_slot() {
        let seed = sample_seed().with_hue(HueSlot::Green, 0x123456);
        assert_eq!(seed.hue(HueSlot::Green), 0x123456);
        assert_eq!(seed.hue(HueSlot::Red), sample_seed().hue(HueSlot::Red));
    }

    fn sample_fallback() -> ResolvedFallback {
        ResolvedFallback {
            surface_base: 0x16181d,
            hues: [0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2],
            accent: 0x84dcc6,
        }
    }

    #[test]
    fn from_current_config_falls_back_to_the_given_fallback_when_seed_unset() {
        let seed = Seed::from_current_config(&RawConfig::default(), sample_fallback());
        assert_eq!(seed.surface_base, 0x16181d);
        assert_eq!(seed.hue(HueSlot::Red), 0xe06c75);
        assert_eq!(seed.hue(HueSlot::Green), 0x98c379);
        assert_eq!(seed.hue(HueSlot::Yellow), 0xe5c07b);
        assert_eq!(seed.hue(HueSlot::Blue), 0x61afef);
        assert_eq!(seed.hue(HueSlot::Magenta), 0xc678dd);
        assert_eq!(seed.hue(HueSlot::Cyan), 0x56b6c2);
        assert_eq!(seed.accent, AccentValue::Hex(0x84dcc6));
        assert_eq!(seed.text_contrast, 15.0);
    }

    #[test]
    fn from_current_config_reads_explicit_seed_and_slot_accent() {
        let mut raw = RawConfig::default();
        raw.theme
            .colors
            .insert("surface_base".to_string(), "#f6f6f6".to_string());
        raw.theme
            .colors
            .insert("accent".to_string(), "blue".to_string());
        raw.theme.ansi.blue = Some("#0048b3".to_string());
        raw.theme.text_contrast = Some(5.3);

        // The fallback is deliberately different from the raw values above,
        // so this test can't accidentally pass because the fallback and the
        // explicit seed happen to agree -- every asserted field must come
        // from `raw`, not `fallback`.
        let seed = Seed::from_current_config(&raw, sample_fallback());
        assert_eq!(seed.surface_base, 0xf6f6f6);
        assert_eq!(seed.hue(HueSlot::Blue), 0x0048b3);
        assert_eq!(seed.accent, AccentValue::Slot(HueSlot::Blue));
        assert_eq!(seed.text_contrast, 5.3);
    }

    #[test]
    fn accent_config_value_matches_to_raw_config() {
        let seed = sample_seed();
        assert_eq!(seed.accent_config_value(), "blue");
        let raw = seed.to_raw_config();
        assert_eq!(
            raw.theme.colors.get("accent").unwrap(),
            &seed.accent_config_value()
        );
    }
}
