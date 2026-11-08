//! Opening Markdown panes for the CLI, and keeping the model's record of
//! the file each one shows.

use super::{CachedPaneLeaf, PaneView, WorkspaceShell};
use gpui::*;
use horizon_workspace::{PaneId, SessionId, SplitAxis, ViewKind};

impl WorkspaceShell {
    /// `horizon markdown <path>`: show a Markdown file in a pane. A file
    /// that already has a pane re-reads in that pane rather than opening a
    /// second one; otherwise a tab (or a split of `split`'s session) is
    /// opened on it.
    pub(crate) fn control_plane_open_markdown(
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
            return Err(format!("not a file: {}", path.display()));
        }
        if let Some(pane_id) = self.workspace.document_pane(ViewKind::Markdown, &path) {
            if let Some(PaneView::Cached(CachedPaneLeaf::Markdown(view))) =
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
                .split_session_with_view(target_session, ViewKind::Markdown, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_view_activated(ViewKind::Markdown, activate),
        };
        self.workspace.set_pane_document(pane_id, path);
        self.reconcile(window, cx);
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }

    /// A Markdown view moved to another file (a followed link, the back
    /// key): the model records it, so the tab title and a restart follow.
    pub(super) fn record_markdown_document(
        &mut self,
        pane_id: PaneId,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.pane_document(pane_id) == Some(path.as_path()) {
            return;
        }
        if self.workspace.set_pane_document(pane_id, path) {
            self.persist_workspace();
            cx.notify();
        }
    }
}
