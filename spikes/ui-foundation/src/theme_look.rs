//! The theme as this spike uses it: Horizon's scheme for a config, the
//! font and its size, and all of it as CSS variables. A module of
//! `crate::theme`'s `horizon`, beside the files of `src/theme/` it reads.

use horizon_config::RawConfig;

use super::{palette, scheme};

/// `src/ui/spec.rs`: the body size its table is designed at.
const REFERENCE_BODY: f32 = 14.;
/// `src/ui/font.rs`: the size when the config sets none.
const DEFAULT_FONT_SIZE: f32 = 13.;
const DEFAULT_FONT_FAMILY: &str = "DejaVu Sans Mono";
/// `src/ui/spec.rs`: the tints of a band, a selected row and a chip.
const BAND_TINT: f32 = 0.03;
const SELECTION_TINT: f32 = 0.07;
const CHIP_TINT: f32 = 0.14;

/// A terminal's colors, font and row height.
#[derive(Clone, Debug)]
pub struct TerminalLook {
    /// The sixteen colors a program names by number.
    pub ansi: [[u8; 3]; 16],
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
    pub selection: [u8; 3],
    pub font_family: String,
    pub font_size: f32,
    pub line_height: f64,
}

#[derive(Clone)]
pub struct Theme {
    scheme: scheme::Scheme,
    pub font_size: f32,
    pub font_family: String,
}

impl Theme {
    pub fn from_config(raw: &RawConfig) -> Self {
        Self {
            scheme: scheme::scheme_from(raw),
            font_size: raw.terminal.font_size.unwrap_or(DEFAULT_FONT_SIZE),
            font_family: (raw.ui.font_family.clone())
                .unwrap_or_else(|| DEFAULT_FONT_FAMILY.to_string()),
        }
    }

    /// The theme of the config file on this machine.
    pub fn of_this_machine() -> Self {
        Self::from_config(horizon_config::load())
    }

    /// The theme with no config file.
    pub fn built_in() -> Self {
        Self::from_config(&RawConfig::default())
    }

    /// A measurement designed at the reference body size, at this size:
    /// `src/ui/spec.rs`'s `scaled`.
    pub fn scaled(&self, at_reference: f32) -> f32 {
        (at_reference * self.font_size / REFERENCE_BODY).round()
    }

    /// What a terminal takes of the theme.
    pub fn terminal(&self) -> TerminalLook {
        let rgb = |color: u32| [(color >> 16) as u8, (color >> 8) as u8, color as u8];
        TerminalLook {
            ansi: self.scheme.ansi.map(rgb),
            foreground: rgb(self.scheme.foreground),
            background: rgb(self.scheme.background),
            cursor: rgb(self.scheme.cursor),
            // `theme::terminal_selection` is the accent; it is drawn here
            // as a tint of it, the text over it staying as it is.
            selection: rgb(palette::blend(
                self.scheme.background,
                self.scheme.accent,
                0.3,
            )),
            font_family: self.font_family.clone(),
            font_size: self.font_size,
            // `src/terminal/font.rs`: a row is 18 px at 13.
            line_height: (self.font_size * 18. / 13.).round() as f64,
        }
    }

    /// Every variable, as the declarations of a `style` attribute.
    pub fn variables(&self) -> String {
        let scheme = &self.scheme;
        let mut out = String::new();
        let mut color = |name: &str, value: u32| {
            out += &format!("--{name}: {}; ", palette::hex(value));
        };
        let tint = |tone: u32, alpha: f32| palette::blend(scheme.background, tone, alpha);
        color("background", scheme.background);
        color("text", scheme.foreground);
        color("text-muted", scheme.text_muted);
        color("text-subtle", scheme.text_subtle);
        color("accent", scheme.accent);
        color("danger", scheme.danger);
        color("warning", scheme.warning);
        color("success", scheme.success);
        color("info", scheme.info);
        color("border", scheme.border);
        color("surface-panel", scheme.surface_panel);
        color("surface-raised", scheme.surface_raised);
        color("surface-chrome", scheme.surface_chrome);
        color("surface-selected", scheme.surface_selected);
        color("diff-added-surface", scheme.diff_added_surface);
        color("diff-added-text", scheme.diff_added_text);
        color("diff-removed-surface", scheme.diff_removed_surface);
        color("diff-removed-text", scheme.diff_removed_text);
        // A band (a message sent in), and a selected row.
        color("band", tint(scheme.foreground, BAND_TINT));
        color("selection", tint(scheme.accent, SELECTION_TINT));
        // A chip in each tone: its tint, and the tone made readable on it
        // (`src/ui/parts.rs`'s `chip`).
        for (name, tone) in [
            ("muted", scheme.text_muted),
            ("accent", scheme.accent),
            ("danger", scheme.danger),
            ("warning", scheme.warning),
            ("success", scheme.success),
            ("info", scheme.info),
        ] {
            let surface = tint(tone, CHIP_TINT);
            color(&format!("chip-{name}-surface"), surface);
            color(
                &format!("chip-{name}-text"),
                palette::contrast_snap(tone, surface),
            );
        }
        // The tab strip's track, and an unselected tab's label made
        // readable on it (`src/theme/gpui_component.rs`).
        let track = palette::blend(scheme.surface_chrome, scheme.surface_panel, 0.5);
        color("tab-track", track);
        color("tab-text", palette::contrast_snap(scheme.text_muted, track));
        // What is written on the accent itself: a primary button's label.
        color("on-accent", palette::primary_foreground_for(scheme.accent));

        // The type scale: a size and the line height that goes with it.
        for (name, size, line) in [
            ("t1", 17., 26.),
            ("t2", 15., 24.),
            ("body", REFERENCE_BODY, 26.),
            ("meta", 12., 18.),
            ("micro", 11., 16.),
        ] {
            out += &format!(
                "--{name}-size: {}px; --{name}-line: {}px; ",
                self.scaled(size),
                self.scaled(line)
            );
        }
        // Lengths that scale with the type: `--u8` is 8 px at the
        // reference size. The rhythm of `src/ui/spec.rs` is 4, 8, 12, 16,
        // 24 and 32 of these.
        for length in [4., 8., 12., 16., 24., 28., 32.] {
            out += &format!("--u{length}: {}px; ", self.scaled(length));
        }
        // The font stack, each family named as CSS wants it.
        let families: Vec<String> = (self.font_family.split(','))
            .map(str::trim)
            .filter(|family| !family.is_empty())
            .map(|family| match family {
                "monospace" | "sans-serif" | "serif" => family.to_string(),
                family => format!("\"{family}\""),
            })
            .collect();
        out += &format!("--font-family: {}; ", families.join(", "));
        out
    }
}
