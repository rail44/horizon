use std::path::PathBuf;

use crate::ui::theme;
use crate::workspace::Workspace;
use floem::prelude::*;

pub(super) fn status_bar(
    workspace: RwSignal<Workspace>,
    agent_state_status: RwSignal<Option<String>>,
    status_dump: Option<PathBuf>,
) -> impl IntoView {
    label(move || {
        let agent_state_status = agent_state_status.get();
        workspace.with(|ws| {
            let status = status_bar_text(ws, agent_state_status.as_deref());
            if let Some(path) = &status_dump {
                let _ = std::fs::write(path, &status);
            }
            status
        })
    })
    .style(|s| {
        s.width_full()
            .height(26)
            .padding_horiz(10)
            .items_center()
            .font_size(12)
            .color(theme::text_muted())
            .background(theme::surface_raised())
    })
}

fn status_bar_text(workspace: &Workspace, agent_state_status: Option<&str>) -> String {
    let base = format!(
        "{} tab(s), {} pane(s), {} detached session(s), active: {}, active pane: {} | Ctrl+Shift+P: control surface",
        workspace.tab_count(),
        workspace.visible_panes().len(),
        workspace.detached_session_count(),
        workspace.active_title(),
        workspace.active_visible_index() + 1
    );
    match agent_state_status {
        Some(status) if !status.is_empty() => format!("{base} | {status}"),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_text_includes_agent_state_diagnostic() {
        let workspace = Workspace::mvp();
        let status = status_bar_text(&workspace, Some("Agent state: /tmp/horizon.duckdb"));

        assert!(status.contains("Ctrl+Shift+P: control surface"));
        assert!(status.contains("Agent state: /tmp/horizon.duckdb"));
    }
}
