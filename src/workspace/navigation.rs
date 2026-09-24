//! Workspace-mode and tab/pane navigation with focus and persistence effects.

use super::WorkspaceShell;
use gpui::*;
use horizon_workspace::{Direction, PaneId};

pub(super) fn workspace_mode_blocked_by_restore(restoring: bool, failed: bool) -> bool {
    restoring && !failed
}

/// Index arithmetic behind `next_tab`/`prev_tab`: `delta` steps around the
/// tab strip, wrapping at both ends (`rem_euclid` keeps negative sums in
/// range, so Shift+Tab from the first tab lands on the last one). Callers
/// guard the `count <= 1` no-op case; this only does the math. Pure so
/// it's unit-testable without a window.
fn cycle_tab_index(active: usize, count: usize, delta: isize) -> usize {
    (active as isize + delta).rem_euclid(count as isize) as usize
}

impl WorkspaceShell {
    pub(super) fn toggle_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        if self.workspace.is_workspace_mode_active() {
            self.workspace.cancel_workspace_mode();
            self.focus_active(window, cx);
        } else {
            self.workspace.enter_workspace_mode();
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    pub(super) fn mode_move(&mut self, direction: Direction, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        self.workspace.move_cursor(direction);
        cx.notify();
    }

    pub(super) fn mode_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        self.workspace.commit_workspace_mode();
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn mode_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        self.workspace.cancel_workspace_mode();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cycles tabs by `delta` steps: `+1` is `NextTab` (Tab), `-1` is
    /// `PrevTab` (Shift+Tab) -- both bound only in [`MODE_CONTEXT`]. While
    /// workspace mode is active the shell root keeps focus -- the mode's
    /// dispatch home. Handing focus to the newly active pane here would
    /// put the focused node under the `Terminal` context, whose deeper
    /// `tab`/`shift-tab` → `NoAction` bindings (`bindings::derive_bindings`,
    /// board #31) shadow `WorkspaceMode`'s at resolution, so every Tab or
    /// Shift+Tab after the first would fall through to the pane's
    /// `on_key_down` and reach the PTY as `0x09` instead of cycling. The
    /// dive into the pane is `mode_commit`/`mode_cancel`'s job when the
    /// mode ends; PTY-level focus still follows the model's (new) active
    /// pane via `sync_terminal_focus`. The non-mode branch is defensive:
    /// neither action is bound outside [`MODE_CONTEXT`], so neither can
    /// normally fire while the mode is off.
    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        let count = self.workspace.tab_count();
        if count > 1 {
            let next = cycle_tab_index(self.workspace.active_tab_index(), count, delta);
            self.workspace.activate_tab_index(next);
            if self.workspace.is_workspace_mode_active() {
                window.focus(&self.focus_handle, cx);
                self.sync_terminal_focus(window, cx);
                self.persist_workspace();
            } else {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(1, window, cx);
    }

    /// Shift+Tab: cycle to the previous tab (wrapping from the first tab
    /// to the last), the reverse direction of `next_tab`.
    pub(super) fn prev_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(-1, window, cx);
    }

    pub(super) fn activate_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.exit_workspace_mode();
        self.workspace.activate_tab_index(index);
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn activate_pane(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.activate_pane(pane_id);
        self.sync_terminal_focus(window, cx);
        self.persist_workspace();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{cycle_tab_index, workspace_mode_blocked_by_restore};
    #[test]
    fn failed_restore_allows_workspace_mode_to_reach_the_reload_command() {
        assert!(workspace_mode_blocked_by_restore(true, false));
        assert!(!workspace_mode_blocked_by_restore(true, true));
        assert!(!workspace_mode_blocked_by_restore(false, false));
    }

    #[test]
    fn cycle_tab_index_steps_forward_wrapping_at_the_end() {
        assert_eq!(cycle_tab_index(1, 3, 1), 2);
        assert_eq!(cycle_tab_index(2, 3, 1), 0);
    }

    #[test]
    fn cycle_tab_index_steps_backward_wrapping_at_the_start() {
        // Shift+Tab from the first tab lands on the last one; `rem_euclid`
        // keeps negative sums in range (and copes with multi-step deltas).
        assert_eq!(cycle_tab_index(1, 3, -1), 0);
        assert_eq!(cycle_tab_index(0, 3, -1), 2);
        assert_eq!(cycle_tab_index(0, 3, -4), 2);
    }

    #[test]
    fn cycle_tab_index_swaps_for_a_two_tab_strip() {
        assert_eq!(cycle_tab_index(0, 2, 1), 1);
        assert_eq!(cycle_tab_index(0, 2, -1), 1);
        assert_eq!(cycle_tab_index(1, 2, 1), 0);
        assert_eq!(cycle_tab_index(1, 2, -1), 0);
    }
}
