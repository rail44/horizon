//! The seed the theme settings view edits -- exactly the knobs
//! `docs/theme-design.md`'s `scheme_from` derives everything else from:
//! `surface_base`, the six `[theme.ansi]` hues, `accent`, and
//! `text_contrast`. Plain data, with its conversions from and to the seed
//! as written ([`Seed::from_written`], [`Seed::to_written`]).

use crate::palette::{hex, parse_hex};
use crate::scheme::resolve_text_contrast;
#[cfg(test)]
use crate::scheme::{TEXT_CONTRAST_CEIL, TEXT_CONTRAST_FLOOR};
use crate::written::{Written, WrittenAnsi};

/// What [`Seed::from_written`] reads a value that is not written as: the
/// scheme in use, which the caller reads and hands in.
#[derive(Clone, Copy)]
pub struct ResolvedFallback {
    pub surface_base: u32,
    /// Indexed by [`HueSlot::ALL`]'s order.
    pub hues: [u32; 6],
    pub accent: u32,
}

/// One of the six `[theme.ansi]` hue slots that double as the seed's own
/// hue set -- also `accent`'s six recognized slot-name spellings
/// (`resolve_accent` in `src/theme/scheme.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HueSlot {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl HueSlot {
    /// Config-file / display order -- matches `WrittenAnsi`'s field
    /// order and `config.example.toml`'s `[theme.ansi]` table.
    pub const ALL: [HueSlot; 6] = [
        HueSlot::Red,
        HueSlot::Green,
        HueSlot::Yellow,
        HueSlot::Blue,
        HueSlot::Magenta,
        HueSlot::Cyan,
    ];

    /// The `[theme.ansi]` key name -- also a valid `accent` slot-name
    /// spelling.
    pub fn config_key(self) -> &'static str {
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
    pub fn label(self) -> &'static str {
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
    /// spellings `resolve_accent` (`src/theme/scheme.rs`) does for `accent`'s
    /// slot-name form.
    pub fn from_config_key(value: &str) -> Option<HueSlot> {
        HueSlot::ALL
            .into_iter()
            .find(|slot| slot.config_key() == value)
    }
}

/// `accent`'s value: either a reference to one of the six [`HueSlot`]s, or
/// a direct hex color -- the same two spellings `resolve_accent`
/// (`src/theme/scheme.rs`) accepts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AccentValue {
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
/// `scheme_from` (`src/theme/scheme.rs`) derives the rest of the theme from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seed {
    pub surface_base: u32,
    /// Indexed by [`HueSlot`]'s [`HueSlot::ALL`] order.
    pub hues: [u32; 6],
    pub accent: AccentValue,
    /// Clamped to `[TEXT_CONTRAST_FLOOR, TEXT_CONTRAST_CEIL]` by every
    /// constructor below -- never stored out of range.
    pub text_contrast: f64,
}

impl Seed {
    pub fn hue(&self, slot: HueSlot) -> u32 {
        self.hues[HueSlot::ALL.iter().position(|s| *s == slot).unwrap()]
    }

    pub fn with_hue(mut self, slot: HueSlot, value: u32) -> Self {
        let index = HueSlot::ALL.iter().position(|s| *s == slot).unwrap();
        self.hues[index] = value;
        self
    }

    /// Reads the seed a configuration has written, falling
    /// back to `fallback` (the currently-resolved scheme role, read by the
    /// caller) for anything unset -- opening the theme settings pane always
    /// shows exactly what's on screen right now, whether that came from the
    /// config file or a built-in default.
    pub fn from_written(raw: &Written, fallback: ResolvedFallback) -> Seed {
        let surface_base = (raw.surface_base.as_deref())
            .and_then(parse_hex)
            .unwrap_or(fallback.surface_base);

        let ansi_slot = |raw_value: &Option<String>, resolved: u32| {
            raw_value.as_deref().and_then(parse_hex).unwrap_or(resolved)
        };
        let hues = [
            ansi_slot(&raw.ansi.red, fallback.hues[0]),
            ansi_slot(&raw.ansi.green, fallback.hues[1]),
            ansi_slot(&raw.ansi.yellow, fallback.hues[2]),
            ansi_slot(&raw.ansi.blue, fallback.hues[3]),
            ansi_slot(&raw.ansi.magenta, fallback.hues[4]),
            ansi_slot(&raw.ansi.cyan, fallback.hues[5]),
        ];

        let accent = match raw.accent.as_deref().map(str::trim) {
            Some(name) if HueSlot::from_config_key(name).is_some() => {
                AccentValue::Slot(HueSlot::from_config_key(name).unwrap())
            }
            // Unset, or an explicit hex, or an unparsable value that fell
            // back to a built-in default -- in every case, the currently
            // resolved accent is the right thing to preload the custom
            // color picker with.
            _ => AccentValue::Hex(fallback.accent),
        };

        let text_contrast = resolve_text_contrast(raw.text_contrast);

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
    pub fn clamp_contrast(value: f64) -> f64 {
        resolve_text_contrast(Some(value))
    }

    /// This seed with every value written: what `scheme_from` derives the
    /// whole scheme of exactly this seed from.
    pub fn to_written(self) -> Written {
        Written {
            surface_base: Some(hex(self.surface_base)),
            accent: Some(self.accent.config_value()),
            text_contrast: Some(self.text_contrast),
            ansi: WrittenAnsi {
                red: Some(hex(self.hue(HueSlot::Red))),
                green: Some(hex(self.hue(HueSlot::Green))),
                yellow: Some(hex(self.hue(HueSlot::Yellow))),
                blue: Some(hex(self.hue(HueSlot::Blue))),
                magenta: Some(hex(self.hue(HueSlot::Magenta))),
                cyan: Some(hex(self.hue(HueSlot::Cyan))),
            },
        }
    }

    /// The `accent` config value this seed would write/apply, for whoever
    /// writes a configuration file.
    pub fn accent_config_value(&self) -> String {
        self.accent.config_value()
    }
}

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
    fn to_written_carries_every_seed_key() {
        let raw = sample_seed().to_written();
        assert_eq!(raw.surface_base.as_deref(), Some("#f6f6f6"));
        assert_eq!(raw.accent.as_deref(), Some("blue"));
        assert_eq!(raw.text_contrast, Some(5.3));
        assert_eq!(raw.ansi.red.as_deref(), Some("#b03b4c"));
        assert_eq!(raw.ansi.green.as_deref(), Some("#008300"));
        assert_eq!(raw.ansi.yellow.as_deref(), Some("#577c00"));
        assert_eq!(raw.ansi.blue.as_deref(), Some("#0048b3"));
        assert_eq!(raw.ansi.magenta.as_deref(), Some("#643bb0"));
        assert_eq!(raw.ansi.cyan.as_deref(), Some("#007f6e"));
    }

    #[test]
    fn to_written_writes_hex_accent_directly() {
        let seed = Seed {
            accent: AccentValue::Hex(0x84dcc6),
            ..sample_seed()
        };
        let raw = seed.to_written();
        assert_eq!(raw.accent.as_deref(), Some("#84dcc6"));
    }

    #[test]
    fn clamp_contrast_floors_and_ceils() {
        assert_eq!(Seed::clamp_contrast(1.0), TEXT_CONTRAST_FLOOR);
        assert_eq!(Seed::clamp_contrast(100.0), TEXT_CONTRAST_CEIL);
        assert_eq!(Seed::clamp_contrast(10.0), 10.0);
    }

    #[test]
    fn clamp_contrast_falls_back_on_non_finite() {
        assert_eq!(Seed::clamp_contrast(f64::NAN), 15.0);
        assert_eq!(Seed::clamp_contrast(f64::INFINITY), 15.0);
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
    fn from_written_falls_back_to_the_given_fallback_when_seed_unset() {
        let seed = Seed::from_written(&Written::default(), sample_fallback());
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
    fn from_written_reads_explicit_seed_and_slot_accent() {
        let mut raw = Written {
            surface_base: Some("#f6f6f6".to_string()),
            accent: Some("blue".to_string()),
            text_contrast: Some(5.3),
            ..Default::default()
        };
        raw.ansi.blue = Some("#0048b3".to_string());

        // The fallback is deliberately different from the raw values above,
        // so this test can't accidentally pass because the fallback and the
        // explicit seed happen to agree -- every asserted field must come
        // from `raw`, not `fallback`.
        let seed = Seed::from_written(&raw, sample_fallback());
        assert_eq!(seed.surface_base, 0xf6f6f6);
        assert_eq!(seed.hue(HueSlot::Blue), 0x0048b3);
        assert_eq!(seed.accent, AccentValue::Slot(HueSlot::Blue));
        assert_eq!(seed.text_contrast, 5.3);
    }

    #[test]
    fn accent_config_value_matches_to_written() {
        let seed = sample_seed();
        assert_eq!(seed.accent_config_value(), "blue");
        let raw = seed.to_written();
        assert_eq!(raw.accent, Some(seed.accent_config_value()));
    }
}
