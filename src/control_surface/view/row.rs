use floem::peniko::Color;

use crate::control_surface::{PaletteItem, PaletteRow, ViewChooserRow};
use crate::ui::list_row::ListRow;
use crate::ui::theme;
use crate::workspace::PaneKind;

/// Dispatches a `PaletteRow` (whichever stage produced it) to its
/// `ListRow` rendering -- the palette view's one row-conversion entry point
/// (`view::palette::command_palette`).
pub(super) fn palette_row_view(row: &PaletteRow) -> ListRow {
    match row {
        PaletteRow::Catalog(item) => palette_item_row(item),
        PaletteRow::Chooser(row) => chooser_row_view(row),
    }
}

fn chooser_row_view(row: &ViewChooserRow) -> ListRow {
    ListRow {
        badge: chooser_kind_label(row).to_string(),
        badge_color: chooser_kind_color(row),
        title: row.title.clone(),
        description: chooser_description(row),
        enabled: true,
        destructive: false,
    }
}

fn chooser_kind_label(row: &ViewChooserRow) -> &'static str {
    match row.kind {
        PaneKind::Terminal => "TERMINAL",
        PaneKind::Agent => "AGENT",
    }
}

fn chooser_kind_color(row: &ViewChooserRow) -> Color {
    match row.kind {
        PaneKind::Terminal => Color::from_rgb8(126, 170, 255),
        PaneKind::Agent => Color::from_rgb8(132, 220, 198),
    }
}

fn chooser_description(row: &ViewChooserRow) -> String {
    match &row.role_id {
        Some(_) => format!("Open a new agent session as {}.", row.title),
        None => match row.kind {
            PaneKind::Terminal => "Open a new terminal session.".to_string(),
            PaneKind::Agent => "Open a new agent session.".to_string(),
        },
    }
}

fn palette_item_row(item: &PaletteItem) -> ListRow {
    ListRow {
        badge: palette_kind_label(item).to_string(),
        badge_color: palette_kind_color(item),
        title: palette_title(item),
        description: palette_description(item),
        enabled: item.enabled(),
        destructive: palette_is_destructive(item),
    }
}

/// A palette row is destructive exactly when it wraps a command marked so
/// on its `CommandSpec` (see `app::commands::CommandSpec::destructive`) —
/// never matched off the title, so future destructive commands inherit the
/// styling automatically.
fn palette_is_destructive(item: &PaletteItem) -> bool {
    match item {
        PaletteItem::Command(entry) => entry.spec.destructive,
        PaletteItem::DetachedSession { .. } | PaletteItem::Tab { .. } => false,
        // Same "destructive" treatment as `Command`'s
        // `CommandSpec::destructive`-marked rows (see
        // `docs/ux-principles.md`'s termination-must-be-visually-distinct
        // requirement) — this row isn't backed by a `CommandSpec` since
        // it's parameterized per session rather than catalog-based, but it
        // ends a session just the same, so it gets the same styling.
        PaletteItem::TerminateSession { .. } => true,
        // Backed by `CommandId::TerminateAllDetachedSessions`, whose
        // `CommandSpec::destructive` is `true` — hardcoded here to match
        // since this row bypasses `Command(CommandEntry)` for its dynamic
        // count title (see `PaletteItem::TerminateAllDetached`'s doc comment).
        PaletteItem::TerminateAllDetached { .. } => true,
    }
}

fn palette_kind_label(item: &PaletteItem) -> &'static str {
    match item {
        PaletteItem::Command(_) => "COMMAND",
        PaletteItem::DetachedSession { .. } => "SESSION",
        PaletteItem::Tab { .. } => "TAB",
        PaletteItem::TerminateSession { .. } | PaletteItem::TerminateAllDetached { .. } => {
            "TERMINATE"
        }
    }
}

fn palette_kind_color(item: &PaletteItem) -> Color {
    match item {
        PaletteItem::Command(_) => Color::from_rgb8(132, 220, 198),
        PaletteItem::DetachedSession { .. } => Color::from_rgb8(126, 170, 255),
        PaletteItem::Tab { .. } => Color::from_rgb8(224, 184, 104),
        // Overridden by `effective_badge_color` since this row is always
        // destructive, but still a real value in case that changes.
        PaletteItem::TerminateSession { .. } | PaletteItem::TerminateAllDetached { .. } => {
            theme::danger()
        }
    }
}

fn palette_title(item: &PaletteItem) -> String {
    match item {
        PaletteItem::Command(entry) => entry.spec.title.to_string(),
        PaletteItem::DetachedSession { title, .. } => format!("Attach {title}"),
        PaletteItem::Tab { index, title, .. } => format!("Tab {}: {title}", index + 1),
        PaletteItem::TerminateSession { title, .. } => format!("Terminate {title}"),
        PaletteItem::TerminateAllDetached { count } => {
            format!("Terminate {count} detached session(s)")
        }
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
            kind.label(),
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
        PaletteItem::TerminateSession {
            kind,
            display_number,
            ..
        } => format!(
            "End the {} session #{} — this stops the session, not just a pane.",
            kind.label(),
            display_number
        ),
        PaletteItem::TerminateAllDetached { count } => {
            format!("End all {count} detached session(s) — this stops each one, not just its pane.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::commands::{CommandCategory, CommandEntry, CommandId, CommandSpec};

    fn command_item(destructive: bool) -> PaletteItem {
        PaletteItem::Command(CommandEntry {
            spec: CommandSpec {
                id: CommandId::TerminateActiveSession,
                title: "Terminate Active Session",
                category: CommandCategory::Workspace,
                description: "Terminate the active session and close its panes.",
                destructive,
            },
            enabled: true,
        })
    }

    #[test]
    fn palette_row_for_destructive_command_is_marked_destructive() {
        assert!(palette_item_row(&command_item(true)).destructive);
    }

    #[test]
    fn palette_row_for_non_destructive_command_is_not_marked_destructive() {
        assert!(!palette_item_row(&command_item(false)).destructive);
    }

    #[test]
    fn palette_rows_for_sessions_and_tabs_are_never_destructive() {
        let session = PaletteItem::DetachedSession {
            session_id: crate::session::SessionId::new(),
            kind: crate::workspace::SessionKind::Terminal,
            display_number: 1,
            title: "Terminal #1".to_string(),
        };
        let tab = PaletteItem::Tab {
            index: 0,
            title: "Terminal #1".to_string(),
            pane_count: 1,
            active: true,
        };

        assert!(!palette_item_row(&session).destructive);
        assert!(!palette_item_row(&tab).destructive);
    }

    #[test]
    fn palette_row_for_terminate_session_is_destructive() {
        let terminate = PaletteItem::TerminateSession {
            session_id: crate::session::SessionId::new(),
            kind: crate::workspace::SessionKind::Terminal,
            display_number: 2,
            title: "Terminal #2".to_string(),
        };

        assert!(palette_item_row(&terminate).destructive);
    }

    #[test]
    fn chooser_row_for_a_kind_with_no_role_is_never_destructive_and_always_enabled() {
        let terminal = ViewChooserRow {
            kind: PaneKind::Terminal,
            role_id: None,
            title: "Terminal".to_string(),
        };

        let row = chooser_row_view(&terminal);
        assert!(!row.destructive);
        assert!(row.enabled);
        assert_eq!(row.badge, "TERMINAL");
        assert_eq!(row.title, "Terminal");
    }

    #[test]
    fn chooser_row_for_a_role_mentions_the_role_title_in_its_description() {
        let config_role = ViewChooserRow {
            kind: PaneKind::Agent,
            role_id: Some(horizon_agent::roles::RoleId("config".to_string())),
            title: "Configuration Agent".to_string(),
        };

        let row = chooser_row_view(&config_role);
        assert_eq!(row.badge, "AGENT");
        assert!(row.description.contains("Configuration Agent"));
    }

    #[test]
    fn palette_row_view_dispatches_to_the_right_conversion() {
        let command_row = PaletteRow::Catalog(command_item(false));
        let chooser_row = PaletteRow::Chooser(ViewChooserRow {
            kind: PaneKind::Terminal,
            role_id: None,
            title: "Terminal".to_string(),
        });

        assert_eq!(palette_row_view(&command_row).badge, "COMMAND");
        assert_eq!(palette_row_view(&chooser_row).badge, "TERMINAL");
    }
}
