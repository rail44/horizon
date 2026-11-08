//! Opening wiki panes for the CLI (`horizon wiki [page]`), which resolves
//! the caller's project and hands over the page's file, as `horizon
//! markdown` hands over a path.

use super::{CachedPaneLeaf, PaneView, WorkspaceShell};
use gpui::*;
use horizon_workspace::{SessionId, SplitAxis, ViewKind};

/// The overview of the project Horizon was started in, for a wiki pane
/// that names no page (one opened from the view chooser).
pub(super) fn default_page() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    horizon_wiki::Store::for_dir(&cwd)?.page_path("index").ok()
}

impl WorkspaceShell {
    /// Shows the wiki page file `path`. A page that already has a wiki pane
    /// re-reads there; otherwise a tab (or a split of `split`'s session) is
    /// opened on it.
    pub(crate) fn control_plane_open_wiki(
        &mut self,
        path: std::path::PathBuf,
        split: Option<(SessionId, SplitAxis)>,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.workspace_phase.blocks_mutation() {
            return Err("workspace restore is still in progress".to_string());
        }
        if !path.is_file() {
            return Err(format!("no wiki page at {}", path.display()));
        }
        if let Some(pane_id) = self.workspace.document_pane(ViewKind::Wiki, &path) {
            if let Some(PaneView::Cached(CachedPaneLeaf::Wiki(view))) =
                self.panes.get(&pane_id).cloned()
            {
                view.update(cx, |view, cx| view.open(path, cx));
            }
            if activate {
                if let Some((tab_index, pane_index)) = self.workspace.pane_position(pane_id) {
                    self.workspace.activate_pane_index(tab_index, pane_index);
                    self.focus_active(window, cx);
                }
            }
            return Ok(());
        }
        let pane_id = match split {
            Some((target_session, axis)) => self
                .workspace
                .split_session_with_view(target_session, ViewKind::Wiki, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_view_activated(ViewKind::Wiki, activate),
        };
        self.workspace.set_pane_document(pane_id, path);
        self.reconcile(window, cx);
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }
}
