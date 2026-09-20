//! The one seeded preview: a small gallery of gpui-component widgets drawn
//! with Horizon's theme roles.
//!
//! It is the template a new preview copies — a `NAME`, a `build` function
//! that returns the view with sample data, and an entry in
//! [`crate::preview::registry`] — and the fixture
//! `scripts/check-preview-plugin.sh` asserts against. The same source is
//! compiled natively by the shell and into the wasm guest.

use gpui::prelude::*;
use gpui::{
    div, px, rgb, AnyView, App, Context, Entity, IntoElement, Render, SharedString, Task, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, v_flex, Icon, IconName, IndexPath, Sizable as _};

// Bundles the icon SVGs into the plugin. `gpui-kit-assets`' default
// `AssetSource` fetches them over HTTP on `target_family = "wasm"`, which a
// WASI guest cannot do; this macro embeds them instead. The guest hands the
// generated type to its runtime as the plugin's `AssetSource`.
gpui_kit_assets::icon_assets!(pub PreviewIcons, [Search, Check, Inbox]);

/// The name the host selects this preview by.
pub const NAME: &str = "sample";

/// The heading this preview paints. A headless check reads it out of the
/// display list to tell "drew something" from "drew this".
pub const LABEL: &str = "Horizon preview sample";

/// The accent role's current value as `#rrggbb`. Painted as text so a theme
/// change is visible in the glyph stream as well as in the painted colors —
/// the host's `scene_summary()` counts primitives and glyphs, not colors.
pub fn accent_swatch_text() -> String {
    format!(
        "accent {}",
        crate::theme::hex(crate::theme::packed_from_hsla(crate::theme::accent()))
    )
}

pub fn build(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| SamplePreview::new(window, cx)).into()
}

struct SampleRows {
    rows: Vec<SharedString>,
    selected: Option<IndexPath>,
}

impl ListDelegate for SampleRows {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let row = self.rows.get(ix.row)?.clone();
        Some(ListItem::new(ix.row).child(div().child(row)))
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.rows.retain(|row| row.contains(query));
        Task::ready(())
    }
}

struct SamplePreview {
    input: Entity<InputState>,
    list: Entity<ListState<SampleRows>>,
}

impl SamplePreview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("type here"));
        let list = cx.new(|cx| {
            ListState::new(
                SampleRows {
                    rows: vec!["alpha".into(), "beta".into(), "gamma".into()],
                    selected: None,
                },
                window,
                cx,
            )
        });
        Self { input, list }
    }
}

impl Render for SamplePreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .gap(px(8.))
            .p(px(12.))
            .bg(rgb(crate::theme::background()))
            .text_color(crate::theme::text_primary())
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(Icon::new(IconName::Search))
                    .child(div().child(LABEL)),
            )
            .child(Button::new("sample-button").primary().label("Primary"))
            .child(Input::new(&self.input).small())
            .child(List::new(&self.list).h(px(80.)))
            .child(
                div()
                    .text_color(crate::theme::accent())
                    .child(accent_swatch_text()),
            )
    }
}
