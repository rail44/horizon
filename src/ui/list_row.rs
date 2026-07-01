use floem::peniko::Color;
use floem::prelude::*;

use crate::ui::theme;

/// Plain data describing one selectable list row: a colored kind badge next to
/// a title/description pair. Domain views materialize their items into this so
/// the row itself stays domain-neutral.
#[derive(Clone)]
pub struct ListRow {
    pub badge: String,
    pub badge_color: Color,
    pub title: String,
    pub description: String,
    pub enabled: bool,
}

/// Per-surface sizing so overview and palette can share one row while keeping
/// their own badge width and row height.
#[derive(Clone, Copy)]
pub struct ListRowStyle {
    pub badge_width: f64,
    pub row_height: f64,
    pub padding_horiz: f64,
}

/// A domain-neutral selectable row.
///
/// `row` supplies the content reactively so the row can update in place as the
/// underlying list is filtered (returning `None` hides the row), `selected`
/// drives the highlight independently of rebuilds, and `on_select` fires on
/// click.
pub fn list_row(
    row: impl Fn() -> Option<ListRow> + Copy + 'static,
    selected: impl Fn() -> bool + 'static,
    style: ListRowStyle,
    on_select: impl Fn() + 'static,
) -> impl IntoView {
    h_stack((
        label(move || row().map(|r| r.badge).unwrap_or_default()).style(move |s| {
            let Some(r) = row() else {
                return s.hide();
            };

            s.width(style.badge_width)
                .height(22)
                .items_center()
                .justify_center()
                .font_size(10)
                .border(1.0)
                .border_color(r.badge_color)
                .color(r.badge_color)
        }),
        v_stack((
            label(move || row().map(|r| r.title).unwrap_or_default())
                .style(|s| s.width_full().font_size(13).color(theme::text_primary())),
            label(move || row().map(|r| r.description).unwrap_or_default())
                .style(|s| s.width_full().font_size(11).color(theme::text_muted())),
        ))
        .style(|s| {
            s.flex()
                .flex_col()
                .min_width(0.0)
                .flex_basis(0.0)
                .flex_grow(1.0)
        }),
    ))
    .on_click_stop(move |_| on_select())
    .style(move |s| {
        let Some(r) = row() else {
            return s.hide();
        };

        let background = if selected() {
            theme::surface_selected()
        } else {
            theme::surface_base()
        };
        let text_color = if r.enabled {
            theme::text_primary()
        } else {
            theme::text_subtle()
        };

        s.width_full()
            .height(style.row_height)
            .items_center()
            .gap(10)
            .padding_horiz(style.padding_horiz)
            .padding_vert(6)
            .background(background)
            .color(text_color)
    })
}
