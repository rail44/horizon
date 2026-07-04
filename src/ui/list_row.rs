use floem::peniko::Color;
use floem::prelude::*;

use crate::ui::theme;

/// Plain data describing one selectable list row: a colored kind badge next to
/// a title/description pair. Domain views materialize their items into this so
/// the row itself stays domain-neutral.
#[derive(Clone)]
pub(crate) struct ListRow {
    pub(crate) badge: String,
    pub(crate) badge_color: Color,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) enabled: bool,
    /// Marks a destructive command row (see `app::commands::CommandSpec::
    /// destructive`) so it renders with a distinct, danger-colored badge —
    /// `docs/ux-principles.md` requires termination to be "visually
    /// distinct from closing a surface".
    pub(crate) destructive: bool,
}

/// Per-surface sizing so overview and palette can share one row while keeping
/// their own badge width and row height.
#[derive(Clone, Copy)]
pub(crate) struct ListRowStyle {
    pub(crate) badge_width: f64,
    pub(crate) row_height: f64,
    pub(crate) padding_horiz: f64,
}

/// A domain-neutral selectable row.
///
/// `row` supplies the content reactively so the row can update in place as the
/// underlying list is filtered (returning `None` hides the row), `selected`
/// drives the highlight independently of rebuilds, and `on_select` fires on
/// click.
pub(crate) fn list_row(
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
            let badge_color = effective_badge_color(&r);

            s.width(style.badge_width)
                .height(22)
                .items_center()
                .justify_center()
                .font_size(10)
                .border(1.0)
                .border_color(badge_color)
                .color(badge_color)
        }),
        v_stack((
            label(move || row().map(|r| r.title).unwrap_or_default()).style(move |s| {
                let color = row()
                    .filter(|r| r.enabled)
                    .map(|_| theme::text_primary())
                    .unwrap_or_else(theme::text_subtle);

                s.width_full().font_size(13).color(color)
            }),
            label(move || row().map(|r| r.description).unwrap_or_default()).style(move |s| {
                let color = row()
                    .filter(|r| r.enabled)
                    .map(|_| theme::text_muted())
                    .unwrap_or_else(theme::text_subtle);

                s.width_full().font_size(11).color(color)
            }),
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

/// The badge color a row actually renders with: `row.badge_color` (the
/// item-kind color, e.g. teal for a command) unless the row is marked
/// `destructive`, in which case the danger accent overrides it regardless
/// of kind — a destructive command should read as a warning first.
fn effective_badge_color(row: &ListRow) -> Color {
    if row.destructive {
        theme::danger()
    } else {
        row.badge_color
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_badge_color, ListRow};
    use crate::ui::theme;
    use floem::peniko::Color;

    fn row(destructive: bool) -> ListRow {
        ListRow {
            badge: "COMMAND".to_string(),
            badge_color: Color::from_rgb8(132, 220, 198),
            title: "Terminate Active Session".to_string(),
            description: "Terminate the active session and close its panes.".to_string(),
            enabled: true,
            destructive,
        }
    }

    #[test]
    fn destructive_row_overrides_badge_color_with_danger_accent() {
        assert_eq!(effective_badge_color(&row(true)), theme::danger());
    }

    #[test]
    fn non_destructive_row_keeps_its_own_badge_color() {
        let row = row(false);
        assert_eq!(effective_badge_color(&row), row.badge_color);
    }
}
