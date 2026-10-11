//! The host half of the shared vocabulary: the two shell-side entities a
//! preview plugin reaches through its root, and the theme and font-size
//! input they serve.

use embedded_gpui::{shared, Ref};
use gpui::{App, Context, Entity, Global, Subscription};
use horizon_config::RawThemeConfig;

use crate::preview::schema::{PreviewHost, PreviewThemeApi};

/// The `[theme]` section the shell last resolved its scheme from, as the
/// JSON every loaded preview reads. Set by [`publish_theme`] from the one
/// place the shell applies a scheme (`theme::live::apply_scheme`); absent
/// until the first such apply, which is why [`current_theme_json`] falls
/// back to the config file the startup scheme came from.
#[derive(Clone, Default)]
struct PreviewThemeInput {
    json: String,
}

impl Global for PreviewThemeInput {}

pub(crate) fn theme_input_json(theme: &RawThemeConfig) -> String {
    serde_json::to_string(theme).unwrap_or_else(|_| String::from("{}"))
}

/// Publishes a newly applied `[theme]` section to every loaded preview.
pub(crate) fn publish_theme(theme: &RawThemeConfig, cx: &mut App) {
    cx.set_global(PreviewThemeInput {
        json: theme_input_json(theme),
    });
}

/// Marks a change of the shell's live font size (`ui::font`) for every
/// loaded preview; the value itself is read from the live store.
#[derive(Clone, Copy, Default)]
struct PreviewFontSizeInput;

impl Global for PreviewFontSizeInput {}

/// Publishes the shell's live font size to every loaded preview. Called by
/// the font-size commands after they move it.
pub(crate) fn publish_font_size(cx: &mut App) {
    cx.set_global(PreviewFontSizeInput);
}

fn current_theme_json(cx: &App) -> String {
    match cx.try_global::<PreviewThemeInput>() {
        Some(input) => input.json.clone(),
        None => theme_input_json(&horizon_config::load().theme),
    }
}

/// The theme object a guest observes. Its `cx.notify` crosses the boundary
/// as the guest's `Remote::observe` callback.
pub(crate) struct PreviewThemeSource {
    json: String,
    font_size: f32,
    _observers: [Subscription; 2],
}

impl PreviewThemeSource {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let theme = cx.observe_global::<PreviewThemeInput>(|this, cx| {
            this.json = current_theme_json(cx);
            cx.notify();
        });
        let font_size = cx.observe_global::<PreviewFontSizeInput>(|this, cx| {
            this.font_size = crate::ui::font::font_size();
            cx.notify();
        });
        Self {
            json: current_theme_json(cx),
            font_size: crate::ui::font::font_size(),
            _observers: [theme, font_size],
        }
    }
}

#[shared]
impl PreviewThemeApi for PreviewThemeSource {
    fn theme_json(&mut self, _cx: &mut Context<Self>) -> String {
        self.json.clone()
    }

    fn font_size(&mut self, _cx: &mut Context<Self>) -> f32 {
        self.font_size
    }
}

/// The shell's root object for one loaded plugin.
pub(crate) struct PreviewHostRoot {
    /// Held so the shared theme entity outlives the guest's remote.
    _theme: Entity<PreviewThemeSource>,
    theme_ref: Ref<PreviewThemeApi>,
    selected: String,
}

impl PreviewHostRoot {
    pub(crate) fn new(
        theme: Entity<PreviewThemeSource>,
        theme_ref: Ref<PreviewThemeApi>,
        selected: String,
    ) -> Self {
        Self {
            _theme: theme,
            theme_ref,
            selected,
        }
    }
}

#[shared]
impl PreviewHost for PreviewHostRoot {
    fn theme(&mut self, _cx: &mut Context<Self>) -> Ref<PreviewThemeApi> {
        self.theme_ref.clone()
    }

    fn selected_preview(&mut self, _cx: &mut Context<Self>) -> String {
        self.selected.clone()
    }
}
