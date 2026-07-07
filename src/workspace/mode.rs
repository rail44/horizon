//! Workspace mode's cursor state transitions -- see
//! `docs/workspace-mode-design.md` for the design this implements: a
//! persistent modal state (entered by a single reserved chord) in which
//! `hjkl` moves a **cursor** -- Horizon's operation target -- independently
//! of **focus** (where keyboard input actually flows). Outside the mode the
//! two are the same pane by construction (`Workspace::cursor_visible_index`
//! falls back to `active_visible_index`, never a separately-tracked value),
//! so there is nothing to keep synchronized on every focus-changing
//! operation elsewhere in the codebase.
//!
//! Formerly disambiguated here against `control_surface::ControlMode::
//! Workspace`, a same-named-but-unrelated concept (the palette's
//! Tab-switching "workspace overview" panel); that panel and its `ControlMode`
//! type are gone (`docs/plans/application-ui/01-session-manager.md` --
//! session management moved to its own modal,
//! `control_surface::view::session_manager`), so the disambiguation no
//! longer applies.

use super::types::Workspace;

/// A cursor movement request. `Up`/`Down` are accepted today (the v1 key
/// interpreter's `hjkl` vocabulary needs all four -- see
/// `workspace::mode_input`) but are currently always a no-op: every split
/// today is still requested as `SplitAxis::Horizontal` (see
/// `docs/recursive-layout-design.md`'s slice plan -- the tree itself is
/// N-ary and vertical-capable, but no caller passes `Vertical` yet), so
/// there is no "pane above/below" to move to. Wiring `Up`/`Down` up to real
/// geometric navigation is slice 3 of that plan, not a v1 gap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Workspace {
    /// Whether workspace mode is currently active.
    pub(crate) fn is_workspace_mode_active(&self) -> bool {
        self.workspace_mode_cursor.is_some()
    }

    /// The visible-pane index (within the active tab) that Horizon
    /// operations should act on right now: the free-floating cursor while
    /// workspace mode is active, or simply `active_visible_index()`
    /// (focus) otherwise -- see this module's doc comment for why that
    /// equivalence needs no explicit synchronization. Clamped defensively
    /// against a pane count that shrank out from under a stale cursor
    /// (e.g. a non-creating palette command run from inside the mode
    /// closing panes elsewhere -- see `docs/workspace-mode-design.md`'s
    /// "everything else restores" rule, which leaves the cursor untouched
    /// by such a command).
    pub(crate) fn cursor_visible_index(&self) -> usize {
        match self.workspace_mode_cursor {
            Some(index) => {
                let visible_len = self.visible_pane_ids().len();
                if visible_len == 0 {
                    0
                } else {
                    index.min(visible_len - 1)
                }
            }
            None => self.active_visible_index(),
        }
    }

    /// Enters workspace mode, seeding the cursor at the currently focused
    /// pane. A no-op if already active -- re-pressing the entry chord (or
    /// otherwise re-requesting entry) while already in the mode must not
    /// reset the cursor back to focus, per
    /// `docs/workspace-mode-design.md`'s "re-pressing the entry key while
    /// already in the mode does nothing".
    pub(crate) fn enter_workspace_mode(&mut self) {
        if self.workspace_mode_cursor.is_some() {
            return;
        }
        self.workspace_mode_cursor = Some(self.active_visible_index());
    }

    /// Moves the cursor one step in `direction`. A no-op when the mode
    /// isn't active, and (today) a no-op for `Up`/`Down` -- see
    /// [`Direction`]. Never wraps: hitting either edge of the pane row
    /// simply stops there, matching vim's non-wrapping window navigation
    /// (`ctrl-w h`/`ctrl-w l` at an edge do nothing) rather than the
    /// existing `focus_next`'s wrap-around cycling, which is a different,
    /// older command this deliberately doesn't reuse.
    pub(crate) fn move_cursor(&mut self, direction: Direction) {
        let Some(current) = self.workspace_mode_cursor else {
            return;
        };
        let visible_len = self.visible_pane_ids().len();
        if visible_len == 0 {
            return;
        }
        let current = current.min(visible_len - 1);
        let next = match direction {
            Direction::Left => current.saturating_sub(1),
            Direction::Right => (current + 1).min(visible_len - 1),
            Direction::Up | Direction::Down => current,
        };
        self.workspace_mode_cursor = Some(next);
    }

    /// `Enter`: focus follows the cursor, then the mode ends. A no-op when
    /// the mode isn't active.
    pub(crate) fn commit_workspace_mode(&mut self) {
        let Some(index) = self.workspace_mode_cursor else {
            return;
        };
        self.activate_visible_pane(index);
        self.workspace_mode_cursor = None;
    }

    /// `Esc`: cancels the mode. Focus never moved while the mode was
    /// active, so simply discarding the cursor is enough to "snap it back"
    /// -- there is nothing else to restore.
    pub(crate) fn cancel_workspace_mode(&mut self) {
        self.workspace_mode_cursor = None;
    }

    /// A pane click while workspace mode is active: the design's click
    /// convention (`docs/workspace-mode-design.md`'s mouse/keyboard split)
    /// is that a click always "dives" into the pane clicked, regardless of
    /// where the cursor currently sits -- equivalent to moving the cursor
    /// to `index` and then calling [`commit_workspace_mode`], done in one
    /// step so there's no observable intermediate cursor position. A no-op
    /// when the mode isn't active: the caller's own ordinary
    /// `activate_visible_pane` call already handles a plain click-to-focus
    /// in that case.
    ///
    /// [`commit_workspace_mode`]: Self::commit_workspace_mode
    pub(crate) fn commit_workspace_mode_to(&mut self, index: usize) {
        if self.workspace_mode_cursor.is_none() {
            return;
        }
        self.workspace_mode_cursor = Some(index);
        self.commit_workspace_mode();
    }

    /// Unconditionally leaves workspace mode, regardless of whether it was
    /// active. Used after a "creating" operation (new terminal/agent
    /// session, a split, or reattaching a detached session) runs from
    /// inside the mode -- `docs/workspace-mode-design.md`'s "creating
    /// operations dive" rule: the operation's own focus-follow
    /// (`workspace::request_active_pane_focus`) already moved focus into
    /// the new pane, so the mode's job (letting the cursor roam without
    /// disturbing focus) is done. Also safe to call unconditionally from a
    /// direct keyboard shortcut path, since a keyboard shortcut can never
    /// actually fire while the mode is active in the first place (the mode
    /// swallows every key outside its own `hjkl`/Enter/Esc/`:` vocabulary
    /// -- see `workspace::mode_input`) -- this just makes the helper
    /// reusable across both call shapes without an extra active-check at
    /// each call site.
    pub(crate) fn exit_workspace_mode(&mut self) {
        self.workspace_mode_cursor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::workspace::types::PaneKind;

    #[test]
    fn cursor_follows_focus_outside_the_mode() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(1);

        assert!(!workspace.is_workspace_mode_active());
        assert_eq!(workspace.cursor_visible_index(), 1);
        assert_eq!(
            workspace.cursor_visible_index(),
            workspace.active_visible_index()
        );
    }

    #[test]
    fn entering_seeds_the_cursor_at_the_focused_pane() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(1);

        workspace.enter_workspace_mode();

        assert!(workspace.is_workspace_mode_active());
        assert_eq!(workspace.cursor_visible_index(), 1);
    }

    #[test]
    fn re_entering_while_active_does_not_reset_the_cursor() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);
        assert_eq!(workspace.cursor_visible_index(), 1);

        workspace.enter_workspace_mode();

        assert_eq!(
            workspace.cursor_visible_index(),
            1,
            "re-entering while already active must be a no-op"
        );
    }

    #[test]
    fn moving_the_cursor_leaves_focus_untouched() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        // `split_active` itself focuses the new (second) pane -- reset to
        // the first so this test starts from a known focus position.
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();

        workspace.move_cursor(Direction::Right);

        assert_eq!(workspace.cursor_visible_index(), 1);
        assert_eq!(
            workspace.active_visible_index(),
            0,
            "focus must not move while the mode is active"
        );
    }

    #[test]
    fn cursor_does_not_wrap_past_either_edge() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();

        workspace.move_cursor(Direction::Left);
        assert_eq!(
            workspace.cursor_visible_index(),
            0,
            "already at the left edge"
        );

        workspace.move_cursor(Direction::Right);
        workspace.move_cursor(Direction::Right);
        assert_eq!(
            workspace.cursor_visible_index(),
            1,
            "already at the right edge"
        );
    }

    #[test]
    fn vertical_moves_are_a_no_op_with_only_horizontal_splits() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();

        workspace.move_cursor(Direction::Down);
        assert_eq!(workspace.cursor_visible_index(), 0);

        workspace.move_cursor(Direction::Up);
        assert_eq!(workspace.cursor_visible_index(), 0);
    }

    #[test]
    fn commit_moves_focus_to_the_cursor_and_exits() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);

        workspace.commit_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
        assert_eq!(workspace.active_visible_index(), 1);
    }

    #[test]
    fn a_pane_click_dives_into_the_clicked_pane_and_exits_the_mode() {
        // The design's click convention: a click always dives into the
        // pane clicked, not wherever the cursor currently sits -- so
        // clicking pane 0 here must win even though the cursor moved to
        // pane 1 (see `workspace::view::pane`'s `PointerDown` handler,
        // which calls this ahead of its own `activate_visible_pane`).
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);
        assert_eq!(workspace.cursor_visible_index(), 1);

        workspace.commit_workspace_mode_to(0);

        assert!(!workspace.is_workspace_mode_active());
        assert_eq!(workspace.active_visible_index(), 0);
    }

    #[test]
    fn commit_workspace_mode_to_is_a_no_op_outside_the_mode() {
        // The caller's own `activate_visible_pane` call already handles an
        // ordinary click-to-focus when the mode isn't active -- this must
        // leave focus untouched rather than double-applying it.
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);

        workspace.commit_workspace_mode_to(1);

        assert!(!workspace.is_workspace_mode_active());
        assert_eq!(workspace.active_visible_index(), 0);
    }

    #[test]
    fn cancel_snaps_the_cursor_back_to_the_untouched_focus() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);

        workspace.cancel_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
        assert_eq!(workspace.active_visible_index(), 0);
        assert_eq!(workspace.cursor_visible_index(), 0);
    }

    #[test]
    fn move_and_commit_are_no_ops_outside_the_mode() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);

        workspace.move_cursor(Direction::Right);
        workspace.commit_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
        assert_eq!(workspace.active_visible_index(), 0);
    }

    #[test]
    fn exit_workspace_mode_is_a_safe_no_op_when_inactive() {
        let mut workspace = Workspace::mvp();

        workspace.exit_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
    }
}
