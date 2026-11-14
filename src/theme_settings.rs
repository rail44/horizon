//! The theme settings: the seed the theme is derived from, each part of
//! it on a control, applied as it is changed and written to the config
//! file only by Save. Below, the colors derived from it.
//!
//! The seed and the writing of it are `horizon-theme`'s, as the GPUI
//! shell's view uses them (`src/theme_settings/`).

use std::path::PathBuf;
use std::rc::Rc;

use horizon_theme::scheme::{Scheme, TEXT_CONTRAST_FLOOR};
use horizon_theme::seed::{AccentValue, HueSlot, ResolvedFallback, Seed};
use horizon_theme_css::Theme;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Callback, Component, El, el};

use crate::parts::{ColorPicker, Drags, Range, Select, Slider};
use crate::plugin::{Command, Mounted, Plugin};

mod styles {
    horizon_ui::css_module! {"
    .theme-settings { height: 100%; overflow-y: scroll; outline: none; box-sizing: border-box;
                      display: flex; flex-direction: column; gap: var(--u24);
                      padding: var(--u16); background: var(--background);
                      color: var(--text); font-family: var(--font-family);
                      font-size: var(--body-size); line-height: var(--body-line);
        .header { display: flex; flex-direction: column; gap: var(--u4); }
        .top { display: flex; align-items: center; justify-content: space-between; }
        .heading { font-size: var(--t2-size); line-height: var(--t2-line); }
        .right { display: flex; align-items: center; gap: var(--u8); }
        .save { padding: 0 var(--u12); border-radius: var(--radius); line-height: var(--u28);
                font-size: var(--meta-size); cursor: pointer;
                background: var(--accent); color: var(--on-accent); }
        .small { font-size: var(--micro-size); line-height: var(--micro-line);
                 color: var(--text-muted); }
        .path { color: var(--text-subtle); }
        .section { display: flex; flex-direction: column; gap: var(--u12); }
        .section-title { font-size: var(--meta-size); line-height: var(--meta-line);
                         color: var(--text-muted); }
        .row { display: flex; flex-wrap: wrap; align-items: flex-start; gap: var(--u16); }
        .labeled { display: flex; flex-direction: column; gap: var(--u4); }
        .inline { display: flex; align-items: center; gap: var(--u8); }
        .accent .select { width: 140px; }
        .contrast .slider { width: 220px; }
        .group { display: flex; flex-direction: column; gap: var(--u4); }
        .chips { display: flex; flex-wrap: wrap; gap: var(--u8); }
        .chip { display: flex; flex-direction: column; align-items: center; gap: var(--u4); }
        .chip .color-of { width: 28px; height: 20px; box-sizing: border-box;
                          border-radius: 3px; border: var(--line) solid var(--border); }
    }
"}
}

/// The accent's choices: the six hues by name, then a color of its own.
const ACCENT_OPTIONS: [&str; 7] = [
    "red", "green", "yellow", "blue", "magenta", "cyan", "custom",
];
/// The slider's top: past it the text hardly changes.
const CONTRAST_MAX: f64 = 15.;

/// Writes a seed to the config file, and says where.
pub type Save = Box<dyn Fn(&Seed) -> Result<PathBuf, String>>;

/// A group of the derived colors: its title, and rows of named colors.
type Swatches = (&'static str, Vec<Vec<(&'static str, u32)>>);

/// What is outside the view.
pub struct Host {
    /// Draws everything in the theme this seed makes.
    pub apply: Box<dyn Fn(&Seed)>,
    /// Writes the seed to the config file, and says where.
    pub save: Save,
    /// The config file's path, as the header shows it.
    pub path: Option<PathBuf>,
}

fn rgb(packed: u32) -> [u8; 3] {
    [(packed >> 16) as u8, (packed >> 8) as u8, packed as u8]
}

fn packed([r, g, b]: [u8; 3]) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// The seed a theme that came from no seed would have had: what the
/// controls start on for the keys the configuration leaves out.
pub fn fallback(scheme: &Scheme) -> ResolvedFallback {
    ResolvedFallback {
        surface_base: scheme.background,
        // Red, green, yellow, blue, magenta, cyan.
        hues: std::array::from_fn(|slot| scheme.ansi[slot + 1]),
        accent: scheme.accent,
    }
}

/// The derived colors, in groups of rows of named swatches.
fn swatches(scheme: &Scheme) -> Vec<Swatches> {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    const BRIGHT: [&str; 8] = [
        "bright black",
        "bright red",
        "bright green",
        "bright yellow",
        "bright blue",
        "bright magenta",
        "bright cyan",
        "bright white",
    ];
    let ansi = |names: [&'static str; 8], from: usize| {
        (names.into_iter().enumerate())
            .map(|(index, name)| (name, scheme.ansi[from + index]))
            .collect::<Vec<_>>()
    };
    vec![
        ("ANSI 16", vec![ansi(NAMES, 0), ansi(BRIGHT, 8)]),
        (
            "Text",
            vec![vec![
                ("primary", scheme.foreground),
                ("muted", scheme.text_muted),
                ("subtle", scheme.text_subtle),
            ]],
        ),
        (
            "Surfaces + borders",
            vec![vec![
                ("panel", scheme.surface_panel),
                ("chrome", scheme.surface_chrome),
                ("selected", scheme.surface_selected),
                ("raised", scheme.surface_raised),
                ("border", scheme.border),
            ]],
        ),
        (
            "Semantic",
            vec![vec![
                ("danger", scheme.danger),
                ("warning", scheme.warning),
                ("success", scheme.success),
                ("info", scheme.info),
            ]],
        ),
        (
            "Diff",
            vec![vec![
                ("added bg", scheme.diff_added_surface),
                ("added text", scheme.diff_added_text),
                ("removed bg", scheme.diff_removed_surface),
                ("removed text", scheme.diff_removed_text),
            ]],
        ),
    ]
}

fn labeled(label: &str, control: El) -> El {
    el("div")
        .class(styles::LABELED)
        .child(el("div").class(styles::SMALL).text(label))
        .child(control)
}

/// Builds the view over `seed`, the theme's seed as it is now; `theme` is
/// the theme drawn, which the swatches follow.
pub fn view(seed: Seed, theme: RwSignal<Theme>, host: Host) -> El {
    let host = Rc::new(host);
    let seed = RwSignal::new(seed);
    let dirty = RwSignal::new(false);
    let status = RwSignal::new(None::<String>);
    let drags = Drags::default();
    // A change of any control: the seed changed, and the theme of it
    // applied.
    let changing = host.clone();
    let change = move |change: &dyn Fn(&mut Seed)| {
        seed.update(|seed| change(seed));
        dirty.set(true);
        status.set(None);
        (changing.apply)(&seed.get_untracked());
    };
    let change = Rc::new(change);

    let surface = {
        let value = RwSignal::new(rgb(seed.get_untracked().surface_base));
        let change = change.clone();
        ColorPicker {
            value,
            drags: &drags,
            on_change: Callback::new(move |color| {
                change(&|seed| seed.surface_base = packed(color));
            }),
        }
        .view()
    };
    let hues = el("div").class(styles::ROW);
    for slot in HueSlot::ALL {
        let value = RwSignal::new(rgb(seed.get_untracked().hue(slot)));
        let change = change.clone();
        let control = ColorPicker {
            value,
            drags: &drags,
            on_change: Callback::new(move |color| {
                change(&|seed| *seed = seed.with_hue(slot, packed(color)));
            }),
        }
        .view();
        hues.child(labeled(slot.label(), control));
    }

    // The accent: one of the hues by name, or a color of its own, which
    // has its picker only while it is the choice.
    let initial = seed.get_untracked();
    let accent_at = match initial.accent {
        AccentValue::Slot(slot) => {
            (HueSlot::ALL.iter().position(|other| *other == slot)).unwrap_or(0)
        }
        AccentValue::Hex(_) => ACCENT_OPTIONS.len() - 1,
    };
    let chosen = RwSignal::new(accent_at);
    let custom = RwSignal::new(rgb(match initial.accent {
        AccentValue::Hex(color) => color,
        AccentValue::Slot(slot) => initial.hue(slot),
    }));
    let options = ACCENT_OPTIONS.map(String::from).to_vec();
    let by_name = change.clone();
    let accent_select = Select {
        options,
        chosen,
        on_change: Callback::new(move |index: usize| {
            let accent = match HueSlot::ALL.get(index) {
                Some(slot) => AccentValue::Slot(*slot),
                None => AccentValue::Hex(packed(custom.get_untracked())),
            };
            by_name(&|seed| seed.accent = accent);
        }),
    };
    let by_color = change.clone();
    let custom_drags = drags.clone();
    let accent = el("div")
        .class((styles::INLINE, styles::ACCENT))
        .child(accent_select)
        .when(
            move || chosen.get() == ACCENT_OPTIONS.len() - 1,
            move || {
                let change = by_color.clone();
                ColorPicker {
                    value: custom,
                    drags: &custom_drags,
                    on_change: Callback::new(move |color| {
                        change(&|seed| seed.accent = AccentValue::Hex(packed(color)));
                    }),
                }
                .view()
            },
        );

    let contrast = RwSignal::new(initial.text_contrast);
    let range = Range {
        min: TEXT_CONTRAST_FLOOR,
        max: CONTRAST_MAX,
        step: 0.1,
    };
    let by_contrast = change.clone();
    let contrast_slider = Slider {
        value: contrast,
        range,
        drags: &drags,
        on_change: Callback::new(move |value| {
            by_contrast(&|seed| seed.text_contrast = Seed::clamp_contrast(value));
        }),
    };

    let saving = host.clone();
    let path = (host.path.as_ref())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "no config path resolved (HOME/XDG_CONFIG_HOME unset)".to_string());
    let header =
        el("div")
            .class(styles::HEADER)
            .child(
                el("div")
                    .class(styles::TOP)
                    .child(el("div").class(styles::HEADING).text("Theme Settings"))
                    .child(
                        el("div")
                            .class(styles::RIGHT)
                            .when(
                                move || dirty.get(),
                                || {
                                    el("div")
                                        .class((styles::SMALL, "unsaved"))
                                        .text("● unsaved changes")
                                },
                            )
                            .child(el("div").class(styles::SAVE).text("Save").on(
                                "click",
                                move |_| match (saving.save)(&seed.get_untracked()) {
                                    Ok(path) => {
                                        dirty.set(false);
                                        status.set(Some(format!("Saved to {}", path.display())));
                                    }
                                    Err(error) => status.set(Some(format!("Save failed: {error}"))),
                                },
                            )),
                    ),
            )
            .child(el("div").class((styles::SMALL, styles::PATH)).text(&path))
            .when(
                move || status.get().is_some(),
                move || {
                    el("div")
                        .class((styles::SMALL, "status"))
                        .text_dyn(move || status.get().unwrap_or_default())
                },
            );

    let seed_section = el("div")
        .class(styles::SECTION)
        .child(el("div").class(styles::SECTION_TITLE).text("Seed"))
        .child(
            el("div")
                .class(styles::ROW)
                .child(labeled("Surface Base", surface)),
        )
        .child(hues)
        .child(labeled("Accent", accent))
        .child(labeled(
            "Text Contrast",
            el("div")
                .class((styles::INLINE, styles::CONTRAST))
                .child(contrast_slider)
                .child(
                    el("div")
                        .class(styles::SMALL)
                        .text_dyn(move || format!("{:.1}", contrast.get())),
                ),
        ));

    // The derived colors follow the theme as it is drawn.
    let preview = el("div")
        .class(styles::SECTION)
        .child(el("div").class(styles::SECTION_TITLE).text("Preview"))
        .each(
            move || swatches(theme.get().scheme()),
            |(title, rows)| (*title, rows.clone()),
            |(title, rows)| {
                let chip = |(name, color): &(&str, u32)| {
                    let color = format!("background: #{color:06x}");
                    el("div")
                        .class(styles::CHIP)
                        .child(el("div").class(styles::COLOR_OF).attr("style", &color))
                        .child(el("div").class(styles::SMALL).text(name))
                };
                el("div")
                    .class(styles::GROUP)
                    .child(el("div").class(styles::SECTION_TITLE).text(title))
                    .children((rows.iter()).map(|row| {
                        el("div")
                            .class(styles::CHIPS)
                            .children(row.iter().map(chip))
                    }))
            },
        );

    let root = el("div")
        .class(styles::THEME_SETTINGS)
        .attr("tabindex", "0")
        .child(header)
        .child(seed_section)
        .child(preview);
    drags.wire(root)
}

/// The theme settings as a plugin. They keep nothing of their own: the
/// seed is the configuration's.
pub struct ThemeSettingsPlugin {
    pub seed: Seed,
    pub theme: RwSignal<Theme>,
    pub host: Host,
}

impl Plugin for ThemeSettingsPlugin {
    const NAME: &'static str = "theme-settings";
    const TITLE: &'static str = "Theme Settings";
    type State = ();
    type Command = std::convert::Infallible;
    type Asked = std::convert::Infallible;

    fn commands() -> Vec<Command<std::convert::Infallible>> {
        Vec::new()
    }

    fn mount(
        self,
        _: (),
        _: Callback<std::convert::Infallible>,
    ) -> Mounted<(), std::convert::Infallible> {
        Mounted {
            view: view(self.seed, self.theme, self.host),
            run: Callback::new(|never| match never {}),
            state: Signal::stored_local(()),
        }
    }
}
