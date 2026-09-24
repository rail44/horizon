//! Preview target selection and activation for CLI and command-model operations.

use super::{CachedPaneLeaf, PaneView, WorkspaceShell};
use crate::preview::{preview_pane_for_path, PreviewTarget};
use gpui::*;
use horizon_workspace::{PaneId, PaneKind, SessionId, SplitAxis, ViewKind};

/// The preview `horizon preview` selects when `--name` is omitted: the name
/// of the preview this repository seeds. A plugin whose previews are named
/// differently needs `--name`, and the pane's status line lists the names it
/// does carry.
const DEFAULT_PREVIEW_NAME: &str = crate::preview::sample::NAME;

impl WorkspaceShell {
    /// `horizon preview <path>`: show a preview plugin in a pane. A path
    /// that already has a pane reloads that pane rather than opening a
    /// second one, so re-running the command after a rebuild is the loop
    /// an agent drives; failing that, an empty preview pane under the
    /// cursor takes the artifact. Only otherwise is a pane opened.
    pub(crate) fn control_plane_open_preview(
        &mut self,
        path: std::path::PathBuf,
        preview_name: Option<String>,
        split: Option<(SessionId, SplitAxis)>,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.workspace_phase.blocks_mutation() {
            return Err("workspace restore is still in progress".to_string());
        }
        let preview_name = preview_name.unwrap_or_else(|| DEFAULT_PREVIEW_NAME.to_string());
        let target = PreviewTarget {
            path: path.clone(),
            preview_name: preview_name.clone(),
        };
        let existing = preview_pane_for_path(&self.preview_targets, &path)
            .or_else(|| self.empty_preview_pane_under_cursor());
        if let Some(pane_id) = existing {
            self.preview_targets.insert(pane_id, target);
            if let Some(PaneView::Cached(CachedPaneLeaf::Preview(view))) =
                self.panes.get(&pane_id).cloned()
            {
                view.update(cx, |pane, cx| pane.retarget(path, preview_name, cx));
            }
            if activate {
                self.activate_preview_pane(pane_id, window, cx);
            }
            return Ok(());
        }
        let pane_id = match split {
            Some((target_session, axis)) => self
                .workspace
                .split_session_with_view(target_session, ViewKind::Preview, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_view_activated(ViewKind::Preview, activate),
        };
        self.preview_targets.insert(pane_id, target);
        self.reconcile(window, cx);
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }

    /// A preview pane under the cursor that has no artifact -- a pane
    /// restored from a previous run (`ViewKindState::Preview`). Without
    /// this, the only way to fill one would be to close it and open another,
    /// since the path lookup can never match a pane that has no path.
    /// Scoped to the cursor pane: with several empty panes, which one a
    /// command filled would otherwise depend on map order.
    fn empty_preview_pane_under_cursor(&self) -> Option<PaneId> {
        let pane_id = self.workspace.cursor_pane_id()?;
        let is_preview = matches!(
            self.workspace.pane_kind(pane_id),
            Some(PaneKind::View(ViewKind::Preview))
        );
        (is_preview && !self.preview_targets.contains_key(&pane_id)).then_some(pane_id)
    }

    fn activate_preview_pane(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((tab_index, pane_index)) = self.workspace.pane_position(pane_id) {
            self.workspace.activate_pane_index(tab_index, pane_index);
            self.focus_active(window, cx);
        }
    }

    /// `CommandId::ReloadPreview`: reload the active pane's plugin, if the
    /// active pane is a preview pane.
    pub(super) fn reload_active_preview(&mut self, cx: &mut Context<Self>) {
        let Some(pane_id) = self.workspace.cursor_pane_id() else {
            return;
        };
        if let Some(PaneView::Cached(CachedPaneLeaf::Preview(view))) =
            self.panes.get(&pane_id).cloned()
        {
            view.update(cx, |pane, cx| pane.reload(cx));
        }
    }
}
