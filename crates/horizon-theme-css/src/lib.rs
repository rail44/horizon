//! Horizon's theme and its type scale as CSS
//! variables.
//!
//! The colors are `horizon-theme`'s scheme for a config, and the
//! measurements its `spec`. A view's CSS names the variables; a
//! change of theme or of font size is a new set of them on the element the
//! views are under.

use horizon_config::RawConfig;
use horizon_theme::spec::{
    self, BAND_TINT, CHIP_TINT, DEFAULT_FONT_FAMILY, DEFAULT_FONT_SIZE, SELECTION_TINT,
};
use horizon_theme::{palette, scheme};
use horizon_ui::{El, el};

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

    /// The colors themselves, packed.
    pub fn scheme(&self) -> &scheme::Scheme {
        &self.scheme
    }

    /// The theme with no config file.
    pub fn built_in() -> Self {
        Self::from_config(&RawConfig::default())
    }

    /// A measurement designed at the reference body size, at this size:
    /// `horizon-theme`'s `spec::scaled`.
    pub fn scaled(&self, at_reference: f32) -> f32 {
        spec::scaled(at_reference, self.font_size)
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
            // A row is 18 px at 13.
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
        // (a chip).
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
        // readable on it.
        let track = palette::blend(scheme.surface_chrome, scheme.surface_panel, 0.5);
        color("tab-track", track);
        color("tab-text", palette::contrast_snap(scheme.text_muted, track));
        // What is written on the accent itself: a primary button's label.
        color("on-accent", palette::primary_foreground_for(scheme.accent));

        // The type scale: a size and the line height that goes with it.
        for (name, step) in [
            ("t1", spec::T1),
            ("t2", spec::T2),
            ("body", spec::BODY),
            ("meta", spec::META),
            ("micro", spec::MICRO),
        ] {
            out += &format!(
                "--{name}-size: {}px; --{name}-line: {}px; ",
                self.scaled(step.size),
                self.scaled(step.line_height)
            );
        }
        // Lengths that scale with the type: `--u8` is 8 px at the
        // reference size. The rhythm is 4, 8, 12, 16,
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
