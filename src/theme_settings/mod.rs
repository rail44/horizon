//! The theme settings pane: Horizon's first session-less first-party
//! view (`docs/theme-settings-view-design.md`). A directory, mirroring
//! `terminal/`/`agent/`'s per-domain-directory convention:
//!
//! - [`seed`]: the pure seed model (`Seed`/`HueSlot`/`AccentValue`) and its
//!   `RawConfig` conversion -- no GPUI, unit-tested directly.
//! - [`save`]: the explicit-Save `toml_edit` write-back.
//! - [`chips`]: the derived-color swatch chip data (design option b).
//! - This module: the GPUI entity itself -- owns the stock
//!   `ColorPicker`/`Select`/`Slider` widgets, wires their change events to
//!   live-apply (`theme::reload_from` + `apply_gpui_component_theme` +
//!   `window.refresh()`, the same sequence `Reload Config` uses,
//!   `src/workspace.rs`'s `CommandId::ReloadConfig` handling), and renders
//!   the chip preview below.
//!
//! Every knob edits an in-memory [`Seed`] field directly and re-derives the
//! whole scheme from it on every change -- there is no partial-seed state:
//! `scheme_from` (`src/theme.rs`) treats "any seed key present" as "the
//! whole seed is configured" (`seed_is_configured`), matching how a
//! hand-edited config.toml already behaves. Nothing is written to disk
//! until the explicit Save button.

mod chips;
mod save;
mod seed;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{h_flex, v_flex, IndexPath, Sizable as _};

use self::chips::ChipGroup;
use self::seed::{AccentValue, HueSlot, Seed};
use crate::theme;

/// The accent select's items: the six hue-slot names (also valid
/// `[theme.ansi]` keys, `HueSlot::config_key`) plus `"custom"` for a direct
/// hex value. Plain lowercase spellings matching `config.example.toml`
/// exactly, rather than capitalized display labels -- the select doubles
/// as a hint at the config syntax itself.
const ACCENT_OPTIONS: [&str; 7] = [
    "red", "green", "yellow", "blue", "magenta", "cyan", "custom",
];

/// The contrast slider's own UI range ceiling -- a "sensible" range for a
/// drag gesture (`docs/theme-settings-view-design.md`), narrower than the
/// full derivation ceiling `Seed::clamp_contrast` enforces
/// (`theme::TEXT_CONTRAST_CEIL`, 21). A config value above this (unusual --
/// the owner's own real config is 5.3) still loads and applies correctly;
/// only the slider's own visual range doesn't reach it.
const CONTRAST_SLIDER_MAX: f32 = 15.0;

fn accent_option_index(accent: AccentValue) -> usize {
    match accent {
        AccentValue::Slot(slot) => HueSlot::ALL
            .iter()
            .position(|candidate| *candidate == slot)
            .expect("slot is one of HueSlot::ALL"),
        AccentValue::Hex(_) => ACCENT_OPTIONS.len() - 1, // "custom", always last
    }
}

pub(crate) struct ThemeSettingsView {
    focus_handle: FocusHandle,
    scroll: ScrollHandle,

    /// The single source of truth for every control's current value --
    /// each widget's change handler updates this, then [`Self::apply_live`]
    /// derives and applies the whole scheme from it.
    seed: Seed,

    surface_base: Entity<ColorPickerState>,
    /// Indexed by [`HueSlot::ALL`]'s order.
    hue_pickers: [Entity<ColorPickerState>; 6],
    accent_select: Entity<SelectState<Vec<&'static str>>>,
    /// Only rendered/interactive while `seed.accent` is
    /// [`AccentValue::Hex`] (the select's `"custom"` choice) -- always
    /// constructed regardless, so switching back to custom doesn't need to
    /// recreate it.
    accent_custom: Entity<ColorPickerState>,
    contrast_slider: Entity<SliderState>,

    /// Whether any control has changed since the last successful Save
    /// (or since the pane opened, if never saved this session).
    dirty: bool,
    /// The last Save attempt's outcome, shown until the next control
    /// change or Save.
    status: Option<String>,

    _subscriptions: Vec<Subscription>,
    /// A live-reading handle to `WorkspaceShell::sessiond`, so
    /// [`Self::apply_live`] can re-push the live scheme to running
    /// terminal sessions on its own, without routing back through
    /// `WorkspaceShell` -- see `SessiondHandle::
    /// broadcast_terminal_color_scheme`. Deliberately not a cloned
    /// `Option<SessiondHandle>` captured once at construction: this view
    /// can be (re)constructed while `Reload Session Runtime`'s async
    /// drain is still in flight (`WorkspaceShell::sessiond` is `None`
    /// then), and a plain clone would freeze on that `None` forever, since
    /// `reconcile` never rebuilds a pane view that already exists. See
    /// `SessiondSlot`'s doc comment.
    sessiond: crate::sessiond::SessiondSlot,
}

impl ThemeSettingsView {
    pub(crate) fn new(
        sessiond: crate::sessiond::SessiondSlot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The currently-resolved scheme, read once here (not inside
        // `seed::from_current_config` itself -- see `ResolvedFallback`'s
        // doc for why that stays a pure function of its arguments).
        let resolved_terminal = theme::terminal_color_scheme();
        let fallback = self::seed::ResolvedFallback {
            surface_base: theme::background(),
            hues: [
                self::seed::pack_rgb(resolved_terminal.red),
                self::seed::pack_rgb(resolved_terminal.green),
                self::seed::pack_rgb(resolved_terminal.yellow),
                self::seed::pack_rgb(resolved_terminal.blue),
                self::seed::pack_rgb(resolved_terminal.magenta),
                self::seed::pack_rgb(resolved_terminal.cyan),
            ],
            accent: theme::packed_from_hsla(theme::accent()),
        };
        let seed = Seed::from_current_config(horizon_config::load(), fallback);

        let surface_base = cx.new(|cx| {
            ColorPickerState::new(window, cx)
                .default_value(self::seed::u32_to_hsla(seed.surface_base))
        });
        let mut subscriptions = vec![cx.subscribe_in(
            &surface_base,
            window,
            |view, _, event: &ColorPickerEvent, window, cx| {
                let ColorPickerEvent::Change(Some(color)) = event else {
                    return;
                };
                view.seed.surface_base = theme::packed_from_hsla(*color);
                view.apply_live(window, cx);
            },
        )];

        let mut hue_pickers_vec = Vec::with_capacity(HueSlot::ALL.len());
        for slot in HueSlot::ALL {
            let picker = cx.new(|cx| {
                ColorPickerState::new(window, cx)
                    .default_value(self::seed::u32_to_hsla(seed.hue(slot)))
            });
            subscriptions.push(cx.subscribe_in(
                &picker,
                window,
                move |view, _, event: &ColorPickerEvent, window, cx| {
                    let ColorPickerEvent::Change(Some(color)) = event else {
                        return;
                    };
                    view.seed = view.seed.with_hue(slot, theme::packed_from_hsla(*color));
                    view.apply_live(window, cx);
                },
            ));
            hue_pickers_vec.push(picker);
        }
        let hue_pickers: [Entity<ColorPickerState>; 6] = hue_pickers_vec
            .try_into()
            .unwrap_or_else(|_| unreachable!("HueSlot::ALL has exactly 6 entries"));

        let initial_custom_accent = match seed.accent {
            AccentValue::Hex(value) => value,
            AccentValue::Slot(slot) => seed.hue(slot),
        };
        let accent_custom = cx.new(|cx| {
            ColorPickerState::new(window, cx)
                .default_value(self::seed::u32_to_hsla(initial_custom_accent))
        });
        subscriptions.push(cx.subscribe_in(
            &accent_custom,
            window,
            |view, _, event: &ColorPickerEvent, window, cx| {
                let ColorPickerEvent::Change(Some(color)) = event else {
                    return;
                };
                view.seed.accent = AccentValue::Hex(theme::packed_from_hsla(*color));
                view.apply_live(window, cx);
            },
        ));

        let accent_select = cx.new(|cx| {
            SelectState::new(
                ACCENT_OPTIONS.to_vec(),
                Some(IndexPath::new(accent_option_index(seed.accent))),
                window,
                cx,
            )
        });
        subscriptions.push(cx.subscribe_in(
            &accent_select,
            window,
            |view, _, event: &SelectEvent<Vec<&'static str>>, window, cx| {
                let SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                view.on_accent_confirm(value, window, cx);
            },
        ));

        let contrast_slider = cx.new(|_| {
            SliderState::new()
                .min(theme::TEXT_CONTRAST_FLOOR as f32)
                .max(CONTRAST_SLIDER_MAX)
                .step(0.1)
                .default_value(seed.text_contrast as f32)
        });
        subscriptions.push(cx.subscribe_in(
            &contrast_slider,
            window,
            |view, _, event: &SliderEvent, window, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                view.seed.text_contrast = Seed::clamp_contrast(value.start() as f64);
                view.apply_live(window, cx);
            },
        ));

        Self {
            focus_handle: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            seed,
            surface_base,
            hue_pickers,
            accent_select,
            accent_custom,
            contrast_slider,
            dirty: false,
            status: None,
            _subscriptions: subscriptions,
            sessiond,
        }
    }

    /// Derives and applies the whole scheme from the current in-memory
    /// [`Seed`] -- the same live-apply sequence `Reload Config` uses
    /// (`src/workspace.rs`'s `CommandId::ReloadConfig`): swap the resolved
    /// scheme, re-project it onto gpui-component's global theme, refresh
    /// the window so every already-painted pane (terminal ANSI, chrome)
    /// picks it up immediately, then re-push the resolved terminal scheme
    /// to every running terminal session so a subsequent OSC 10/11/12
    /// query reflects it too (`SessiondHandle::
    /// broadcast_terminal_color_scheme`). Pure math plus one global-state
    /// write plus a fire-and-forget send over the already-open sessiond
    /// connection (no reply awaited) -- cheap enough to call on every
    /// slider tick/color-picker drag frame.
    fn apply_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = true;
        self.status = None;
        theme::reload_from(&self.seed.to_raw_config());
        theme::apply_gpui_component_theme(cx);
        window.refresh();
        if let Some(sessiond) = self.sessiond.get() {
            sessiond.broadcast_terminal_color_scheme(theme::terminal_color_scheme());
        }
        cx.notify();
    }

    fn on_accent_confirm(
        &mut self,
        value: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if value == "custom" {
            let packed = self
                .accent_custom
                .read(cx)
                .value()
                .map(theme::packed_from_hsla)
                .unwrap_or(match self.seed.accent {
                    AccentValue::Hex(value) => value,
                    AccentValue::Slot(slot) => self.seed.hue(slot),
                });
            self.seed.accent = AccentValue::Hex(packed);
        } else if let Some(slot) = HueSlot::from_config_key(value) {
            self.seed.accent = AccentValue::Slot(slot);
        } else {
            return;
        }
        self.apply_live(window, cx);
    }

    /// The Save button's handler -- a view-local action (not a `CommandId`
    /// variant): matches the agent composer's send button
    /// (`src/agent/view.rs`'s `render_send_button`), which is likewise a
    /// plain `cx.listener` calling a method on the view directly rather
    /// than riding the command model (that convention is reserved for
    /// operations reachable from the palette/keybindings; Save is only
    /// ever reachable from this pane's own button). Writes only the seed
    /// keys via [`save::save`]; every other section of the config file is
    /// untouched.
    fn save(&mut self, cx: &mut Context<Self>) {
        match save::save(&self.seed) {
            Ok(path) => {
                self.dirty = false;
                self.status = Some(format!("Saved to {}", path.display()));
            }
            Err(error) => {
                self.status = Some(format!("Save failed: {error}"));
            }
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let path_label = horizon_config::resolved_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "no config path resolved (HOME/XDG_CONFIG_HOME unset)".to_string());

        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(15.0))
                            .text_color(theme::text_primary())
                            .child("Theme Settings"),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .when(self.dirty, |this| {
                                this.child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme::text_muted())
                                        .child("● unsaved changes"),
                                )
                            })
                            .child(
                                Button::new("theme-settings-save")
                                    .primary()
                                    .label("Save")
                                    .on_click(cx.listener(|view, _, _, cx| view.save(cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_subtle())
                    .child(path_label),
            )
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(status),
                )
            })
    }

    fn render_seed_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(section_title("Seed"))
            .child(h_flex().gap_4().items_start().child(labeled(
                "Surface Base",
                ColorPicker::new(&self.surface_base).small(),
            )))
            .child(h_flex().gap_4().flex_wrap().items_start().children(
                HueSlot::ALL.iter().enumerate().map(|(index, slot)| {
                    labeled(
                        slot.label(),
                        ColorPicker::new(&self.hue_pickers[index]).small(),
                    )
                }),
            ))
            .child(self.render_accent_row(cx))
            .child(labeled(
                "Text Contrast",
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Slider::new(&self.contrast_slider).w(px(220.0)))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_muted())
                            .child(format!("{:.1}", self.seed.text_contrast)),
                    ),
            ))
    }

    fn render_accent_row(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let is_custom = matches!(self.seed.accent, AccentValue::Hex(_));
        labeled(
            "Accent",
            h_flex()
                .gap_2()
                .items_center()
                .child(Select::new(&self.accent_select).small().w(px(140.0)))
                .when(is_custom, |this| {
                    this.child(ColorPicker::new(&self.accent_custom).small())
                }),
        )
    }

    fn render_preview_section(&self) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(section_title("Preview"))
            .children(chips::chip_groups().into_iter().map(render_chip_group))
    }
}

fn section_title(title: &'static str) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .text_color(theme::text_muted())
        .child(title)
}

/// Wraps `control` with an 11px `text_muted` label above it -- the one
/// label style every seed control uses uniformly (rather than mixing
/// `ColorPicker`'s own built-in `.label()` with hand-rolled labels for
/// `Select`/`Slider`, which don't have one).
fn labeled(label: &'static str, control: impl IntoElement) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child(label),
        )
        .child(control)
}

fn render_chip_group(group: ChipGroup) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(section_title(group.title))
        .children(group.rows.into_iter().map(render_chip_row))
}

fn render_chip_row(row: Vec<chips::Chip>) -> impl IntoElement {
    h_flex()
        .gap_2()
        .flex_wrap()
        .children(row.into_iter().map(render_chip))
}

fn render_chip(chip: chips::Chip) -> impl IntoElement {
    v_flex()
        .gap_1()
        .items_center()
        .child(
            div()
                .w(px(28.0))
                .h(px(20.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(theme::border())
                .bg(chip.color),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child(chip.label),
        )
}

impl Focusable for ThemeSettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ThemeSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("theme-settings")
            .track_focus(&self.focus_handle)
            .track_scroll(&self.scroll)
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .child(
                v_flex()
                    .gap_5()
                    .child(self.render_header(cx))
                    .child(self.render_seed_section(cx))
                    .child(self.render_preview_section()),
            )
    }
}
