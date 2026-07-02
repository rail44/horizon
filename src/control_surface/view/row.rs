use floem::peniko::Color;

use crate::control_surface::{OverviewItem, PaletteItem};
use crate::ui::list_row::ListRow;

use super::super::query::{pane_kind_label, session_kind_label};

pub(super) fn palette_item_row(item: &PaletteItem) -> ListRow {
    ListRow {
        badge: palette_kind_label(item).to_string(),
        badge_color: palette_kind_color(item),
        title: palette_title(item),
        description: palette_description(item),
        enabled: item.enabled(),
    }
}

pub(super) fn overview_item_row(item: &OverviewItem) -> ListRow {
    ListRow {
        badge: overview_kind_label(item).to_string(),
        badge_color: overview_kind_color(item),
        title: overview_title(item),
        description: overview_description(item),
        enabled: true,
    }
}

fn palette_kind_label(item: &PaletteItem) -> &'static str {
    match item {
        PaletteItem::Command(_) => "COMMAND",
        PaletteItem::DetachedSession { .. } => "SESSION",
        PaletteItem::Tab { .. } => "TAB",
    }
}

fn palette_kind_color(item: &PaletteItem) -> Color {
    match item {
        PaletteItem::Command(_) => Color::rgb8(132, 220, 198),
        PaletteItem::DetachedSession { .. } => Color::rgb8(126, 170, 255),
        PaletteItem::Tab { .. } => Color::rgb8(224, 184, 104),
    }
}

fn palette_title(item: &PaletteItem) -> String {
    match item {
        PaletteItem::Command(entry) => entry.spec.title.to_string(),
        PaletteItem::DetachedSession { title, .. } => format!("Attach {title}"),
        PaletteItem::Tab { index, title, .. } => format!("Tab {}: {title}", index + 1),
    }
}

fn palette_description(item: &PaletteItem) -> String {
    match item {
        PaletteItem::Command(entry) => entry.spec.description.to_string(),
        PaletteItem::DetachedSession {
            kind,
            display_number,
            ..
        } => format!(
            "Detached {} session #{}; attach to the active tab as a split.",
            session_kind_label(*kind),
            display_number
        ),
        PaletteItem::Tab {
            pane_count, active, ..
        } => {
            if *active {
                format!("Current tab with {pane_count} pane(s).")
            } else {
                format!("Switch to tab with {pane_count} pane(s).")
            }
        }
    }
}

fn overview_kind_label(item: &OverviewItem) -> &'static str {
    match item {
        OverviewItem::Tab { .. } => "TAB",
        OverviewItem::DetachedSession { .. } => "DETACHED",
        OverviewItem::Pane { .. } => "PANE",
    }
}

fn overview_kind_color(item: &OverviewItem) -> Color {
    match item {
        OverviewItem::Tab { .. } => Color::rgb8(224, 184, 104),
        OverviewItem::DetachedSession { .. } => Color::rgb8(126, 170, 255),
        OverviewItem::Pane { .. } => Color::rgb8(132, 220, 198),
    }
}

fn overview_title(item: &OverviewItem) -> String {
    match item {
        OverviewItem::Tab { index, title, .. } => format!("Tab {}: {title}", index + 1),
        OverviewItem::DetachedSession { title, .. } => format!("Attach {title}"),
        OverviewItem::Pane {
            tab_index,
            pane_index,
            title,
            ..
        } => format!("Tab {} / Pane {}: {title}", tab_index + 1, pane_index + 1),
    }
}

fn overview_description(item: &OverviewItem) -> String {
    match item {
        OverviewItem::Tab {
            pane_count, active, ..
        } => {
            if *active {
                format!("Current tab · {pane_count} pane(s)")
            } else {
                format!("Switch to tab · {pane_count} pane(s)")
            }
        }
        OverviewItem::DetachedSession {
            kind,
            display_number,
            ..
        } => format!(
            "Detached {} session #{} · Enter attaches as split",
            session_kind_label(*kind),
            display_number
        ),
        OverviewItem::Pane {
            kind,
            active,
            tab_active,
            ..
        } => {
            let state = if *tab_active && *active {
                "Active pane"
            } else if *tab_active {
                "Visible pane"
            } else {
                "Pane in inactive tab"
            };
            format!("{state} · {} pane", pane_kind_label(*kind))
        }
    }
}
