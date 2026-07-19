//! Workspace mode's cursor state transitions -- see
//! `docs/workspace-mode-design.md` for the design this implements: a
//! persistent modal state (entered by a single reserved chord) in which
//! `hjkl` moves a **cursor** -- Horizon's operation target -- independently
//! of **focus** (where keyboard input actually flows). Outside the mode the
//! two are the same pane by construction (`Workspace::cursor_pane_id`
//! falls back to the tab's own `active` pane, never a separately-tracked
//! value), so there is nothing to keep synchronized on every focus-changing
//! operation elsewhere in the codebase.
//!
//! Formerly disambiguated here against `control_surface::ControlMode::
//! Workspace`, a same-named-but-unrelated concept (the palette's
//! Tab-switching "workspace overview" panel); that panel and its `ControlMode`
//! type are gone (`docs/plans/application-ui/01-session-manager.md` --
//! session management moved to its own modal, `src/session_manager.rs`),
//! so the disambiguation no longer applies.

use super::nav::{nearest_in_direction, pane_rects};
use super::types::{PaneId, Workspace};

/// A cursor movement request, resolved geometrically (bspwm style) rather
/// than by tree structure (`docs/recursive-layout-design.md`'s Focus
/// navigation decision) -- see `workspace::nav::nearest_in_direction`, the
/// pure function [`Workspace::move_cursor`] delegates to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Workspace {
    /// Whether workspace mode is currently active. `true` either because
    /// it was explicitly toggled on ([`Self::workspace_mode_active`], the
    /// raw "entered via the reserved chord" bookkeeping [`enter_workspace_
    /// mode`]/[`cancel_workspace_mode`]/[`commit_workspace_mode`]/
    /// [`exit_workspace_mode`] read and write) *or*, unconditionally,
    /// because the workspace has zero tabs.
    ///
    /// The zero-tab bypass is the owner's 2026-07-19 clarification of the
    /// mode's own purpose: workspace mode exists to separate "keys go to
    /// the focused pane" from "keys command the workspace"
    /// (`docs/workspace-mode-design.md`), and with no panes at all there
    /// is no pane input left to protect -- so an empty workspace is
    /// implicitly *always* a command surface, and requiring the entry
    /// chord first is meaningless. This getter is the single place that
    /// encodes that: every mode-resident key binding (`:` opening the
    /// palette foremost -- the only reachable path back to `New Tab…`
    /// once every pane is gone) becomes reachable the instant the
    /// workspace empties, no entry chord needed, and reverts the instant
    /// a tab exists again, purely because the bypass stops applying --
    /// airtight in both directions by construction, since nothing needs
    /// to remember to flip a flag on the way in or out. The entry chord
    /// itself becomes a harmless no-op while empty as a direct
    /// consequence: `toggle_mode` (`src/workspace/render.rs`) always sees
    /// this method return `true` while empty and takes its "cancel"
    /// branch, which is already idempotent when [`Self::
    /// workspace_mode_cursor`] is `None` (see [`cancel_workspace_mode`]).
    ///
    /// The raw field alone remains the exact answer once a tab exists --
    /// the bypass only ever adds, never removes, activeness, so a
    /// non-empty workspace's behavior is untouched.
    ///
    /// The GPUI shell additionally suppresses this (for its key-context
    /// decision only, not this method's own answer) while a
    /// control-surface modal is open -- see `render::
    /// mode_key_context_active`'s doc comment for why: the mode's own
    /// hjkl/Enter/Escape bindings must not compete with a modal's typed
    /// search keys, the same hazard `effective_scrim_pattern` already
    /// guards against on the scrim/border side.
    ///
    /// [`enter_workspace_mode`]: Self::enter_workspace_mode
    /// [`cancel_workspace_mode`]: Self::cancel_workspace_mode
    /// [`commit_workspace_mode`]: Self::commit_workspace_mode
    /// [`exit_workspace_mode`]: Self::exit_workspace_mode
    pub fn is_workspace_mode_active(&self) -> bool {
        self.workspace_mode_active || self.tab_count() == 0
    }

    /// Enters workspace mode, seeding the cursor at the currently focused
    /// pane (or `None`, for a zero-tab workspace with nothing to focus). A
    /// no-op if already active -- re-pressing the entry chord (or
    /// otherwise re-requesting entry) while already in the mode must not
    /// reset the cursor back to focus, per
    /// `docs/workspace-mode-design.md`'s "re-pressing the entry key while
    /// already in the mode does nothing".
    pub fn enter_workspace_mode(&mut self) {
        if self.workspace_mode_active {
            return;
        }
        self.workspace_mode_active = true;
        self.workspace_mode_cursor = self.active_tab().map(|tab| tab.active);
    }

    /// Moves the cursor one step in `direction`, resolved geometrically
    /// (`workspace::nav::nearest_in_direction`) against the active tab's
    /// pane rectangles: the nearest pane whose rectangle lies in
    /// `direction` from the cursor's, by relative position and overlap --
    /// not a flat left/right index walk, so `Up`/`Down` reach panes across
    /// a vertical split exactly like `Left`/`Right` do across a horizontal
    /// one (`docs/recursive-layout-design.md`'s slice 4). A no-op when the
    /// mode isn't active, or when nothing qualifies in `direction` (the
    /// cursor is already at that edge) -- hitting an edge simply stops
    /// there, matching vim's non-wrapping window navigation (`ctrl-w h`/
    /// `ctrl-w l` at an edge do nothing) rather than the existing
    /// `focus_next`'s wrap-around cycling, which is a different, older
    /// command this deliberately doesn't reuse.
    pub fn move_cursor(&mut self, direction: Direction) {
        let Some(current) = self.workspace_mode_cursor else {
            return;
        };
        let Some(tab) = self.active_tab() else {
            return;
        };
        let rects = pane_rects(&tab.root);
        if let Some(next) = nearest_in_direction(&rects, current, direction) {
            self.workspace_mode_cursor = Some(next);
        }
    }

    /// `Enter`: focus follows the cursor, then the mode ends. Exits the
    /// mode unconditionally (a no-op if it wasn't active); if there was no
    /// cursor to commit to -- a zero-tab workspace, which entered the mode
    /// with nothing to focus -- there is simply nothing else to do.
    pub fn commit_workspace_mode(&mut self) {
        self.workspace_mode_active = false;
        if let Some(pane_id) = self.workspace_mode_cursor.take() {
            self.activate_pane(pane_id);
        }
    }

    /// `Esc`: cancels the mode. Focus never moved while the mode was
    /// active, so simply discarding the cursor is enough to "snap it back"
    /// -- there is nothing else to restore.
    pub fn cancel_workspace_mode(&mut self) {
        self.workspace_mode_active = false;
        self.workspace_mode_cursor = None;
    }

    /// A pane click while workspace mode is active: the design's click
    /// convention (`docs/workspace-mode-design.md`'s mouse/keyboard split)
    /// is that a click always "dives" into the pane clicked, regardless of
    /// where the cursor currently sits -- equivalent to moving the cursor
    /// to `pane_id` and then calling [`commit_workspace_mode`], done in one
    /// step so there's no observable intermediate cursor position. A no-op
    /// when the mode isn't active: the caller's own ordinary
    /// `activate_pane` call already handles a plain click-to-focus in that
    /// case.
    ///
    /// [`commit_workspace_mode`]: Self::commit_workspace_mode
    pub fn commit_workspace_mode_to(&mut self, pane_id: PaneId) {
        if !self.workspace_mode_active {
            return;
        }
        self.workspace_mode_cursor = Some(pane_id);
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
    pub fn exit_workspace_mode(&mut self) {
        self.workspace_mode_active = false;
        self.workspace_mode_cursor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PaneKind, SplitAxis};
    use crate::SessionId;

    #[test]
    fn cursor_follows_focus_outside_the_mode() {
        let mut workspace = Workspace::mvp();
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

        assert!(!workspace.is_workspace_mode_active());
        assert_eq!(workspace.cursor_pane_id(), Some(second));
        assert!(workspace.is_active_pane(second));
    }

    #[test]
    fn entering_seeds_the_cursor_at_the_focused_pane() {
        let mut workspace = Workspace::mvp();
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

        workspace.enter_workspace_mode();

        assert!(workspace.is_workspace_mode_active());
        assert_eq!(workspace.cursor_pane_id(), Some(second));
    }

    #[test]
    fn re_entering_while_active_does_not_reset_the_cursor() {
        let mut workspace = Workspace::mvp();
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);
        assert_eq!(workspace.cursor_pane_id(), Some(second));

        workspace.enter_workspace_mode();

        assert_eq!(
            workspace.cursor_pane_id(),
            Some(second),
            "re-entering while already active must be a no-op"
        );
    }

    #[test]
    fn moving_the_cursor_leaves_focus_untouched() {
        let mut workspace = Workspace::mvp();
        let first = workspace.visible_pane_id(0).expect("first pane");
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        // `split_active` itself focuses the new (second) pane -- reset to
        // the first so this test starts from a known focus position.
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();

        workspace.move_cursor(Direction::Right);

        assert_eq!(workspace.cursor_pane_id(), Some(second));
        assert!(
            workspace.is_active_pane(first),
            "focus must not move while the mode is active"
        );
    }

    #[test]
    fn cursor_does_not_move_past_either_edge_of_a_horizontal_split() {
        let mut workspace = Workspace::mvp();
        let first = workspace.visible_pane_id(0).expect("first pane");
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();

        workspace.move_cursor(Direction::Left);
        assert_eq!(
            workspace.cursor_pane_id(),
            Some(first),
            "already at the left edge"
        );

        workspace.move_cursor(Direction::Right);
        workspace.move_cursor(Direction::Right);
        assert_eq!(
            workspace.cursor_pane_id(),
            Some(second),
            "already at the right edge"
        );
    }

    #[test]
    fn vertical_moves_navigate_a_vertical_split() {
        let mut workspace = Workspace::mvp();
        let top = workspace.visible_pane_id(0).expect("first pane");
        let target_session = workspace
            .active_terminal_session_id()
            .expect("mvp() starts with a terminal session");
        workspace.split_session_with_new_session(
            target_session,
            PaneKind::Terminal,
            SplitAxis::Vertical,
            true,
        );
        let bottom = workspace
            .visible_pane_id(1)
            .expect("the vertical split created a second pane");
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();

        workspace.move_cursor(Direction::Down);
        assert_eq!(workspace.cursor_pane_id(), Some(bottom));

        workspace.move_cursor(Direction::Up);
        assert_eq!(workspace.cursor_pane_id(), Some(top));
    }

    #[test]
    fn commit_moves_focus_to_the_cursor_and_exits() {
        let mut workspace = Workspace::mvp();
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);

        workspace.commit_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
        assert!(workspace.is_active_pane(second));
    }

    #[test]
    fn a_pane_click_dives_into_the_clicked_pane_and_exits_the_mode() {
        // The design's click convention: a click always dives into the
        // pane clicked, not wherever the cursor currently sits -- so
        // clicking the first pane here must win even though the cursor
        // moved to the second. Exercises `commit_workspace_mode_to`
        // directly at the model level; no production caller drives it
        // from a real click today (the GPUI shell's pane click handler
        // in `src/workspace.rs` calls `activate_pane` only).
        let mut workspace = Workspace::mvp();
        let first = workspace.visible_pane_id(0).expect("first pane");
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);
        assert_eq!(workspace.cursor_pane_id(), Some(second));

        workspace.commit_workspace_mode_to(first);

        assert!(!workspace.is_workspace_mode_active());
        assert!(workspace.is_active_pane(first));
    }

    #[test]
    fn commit_workspace_mode_to_is_a_no_op_outside_the_mode() {
        // The caller's own `activate_pane` call already handles an
        // ordinary click-to-focus when the mode isn't active -- this must
        // leave focus untouched rather than double-applying it.
        let mut workspace = Workspace::mvp();
        let first = workspace.visible_pane_id(0).expect("first pane");
        let second = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);

        workspace.commit_workspace_mode_to(second);

        assert!(!workspace.is_workspace_mode_active());
        assert!(workspace.is_active_pane(first));
    }

    #[test]
    fn cancel_snaps_the_cursor_back_to_the_untouched_focus() {
        let mut workspace = Workspace::mvp();
        let first = workspace.visible_pane_id(0).expect("first pane");
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);
        workspace.enter_workspace_mode();
        workspace.move_cursor(Direction::Right);

        workspace.cancel_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
        assert!(workspace.is_active_pane(first));
        assert_eq!(workspace.cursor_pane_id(), Some(first));
    }

    #[test]
    fn move_and_commit_are_no_ops_outside_the_mode() {
        let mut workspace = Workspace::mvp();
        let first = workspace.visible_pane_id(0).expect("first pane");
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_visible_pane(0);

        workspace.move_cursor(Direction::Right);
        workspace.commit_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
        assert!(workspace.is_active_pane(first));
    }

    #[test]
    fn exit_workspace_mode_is_a_safe_no_op_when_inactive() {
        let mut workspace = Workspace::mvp();

        workspace.exit_workspace_mode();

        assert!(!workspace.is_workspace_mode_active());
    }

    #[test]
    fn zero_tab_workspace_is_implicitly_in_workspace_mode() {
        // 2026-07-19 owner clarification (superseding the prior "enterable
        // with zero tabs" framing): with no panes at all there is no pane
        // input left to protect, so an empty workspace is *always* a
        // command surface -- `is_workspace_mode_active()` must already
        // report `true` the instant the last tab closes, with no explicit
        // `enter_workspace_mode()` call needed first (that's the whole
        // point: `:` opening the palette, the only reachable path back to
        // `New Tab…`, must not require the entry chord when there is
        // nothing left for it to protect).
        let mut workspace = Workspace::mvp();
        assert!(!workspace.is_workspace_mode_active());

        workspace.terminate_active_session();

        assert_eq!(workspace.tab_count(), 0);
        assert!(workspace.is_workspace_mode_active());
        assert_eq!(workspace.cursor_pane_id(), None);
        // The raw "explicitly entered" bookkeeping never had to flip --
        // the getter's zero-tab bypass is doing all the work.
        assert!(!workspace.workspace_mode_active);
    }

    #[test]
    fn creating_the_first_tab_exits_the_implicit_empty_mode() {
        // The reverse transition must be just as immediate: the moment a
        // tab exists again, the zero-tab bypass stops applying and normal
        // (raw-field) gating resumes -- no call site needs to remember to
        // flip anything off.
        let mut workspace = Workspace::mvp();
        workspace.terminate_active_session();
        assert!(workspace.is_workspace_mode_active());

        workspace.open_tab_with_new_session_activated(PaneKind::Terminal, true);

        assert!(!workspace.is_workspace_mode_active());
    }

    #[test]
    fn toggling_off_is_a_harmless_no_op_on_an_empty_workspace() {
        // Mirrors what `toggle_mode` (`src/workspace/render.rs`) actually
        // does when the entry chord fires on an empty workspace:
        // `is_workspace_mode_active()` is unconditionally `true` there, so
        // the toggle always takes the "cancel" branch -- which must be a
        // true no-op (nothing observable changes, and the workspace stays
        // exactly as implicitly active as before).
        let mut workspace = Workspace::mvp();
        workspace.terminate_active_session();

        workspace.cancel_workspace_mode();

        assert!(workspace.is_workspace_mode_active());
        assert_eq!(workspace.cursor_pane_id(), None);
    }

    #[test]
    fn pane_dependent_mode_keys_are_inert_on_an_empty_workspace() {
        // hjkl navigation, `Enter` (commit), and `Esc` (cancel) are
        // pane-dependent (`docs/workspace-mode-design.md`'s v1 keyset) --
        // with no panes at all, moving/committing/canceling must have
        // nothing to act on, even though the mode reads as active
        // throughout (the empty-workspace bypass, not a real cursor).
        let mut workspace = Workspace::mvp();
        workspace.terminate_active_session();
        assert!(workspace.is_workspace_mode_active());

        workspace.move_cursor(Direction::Right);
        assert_eq!(workspace.cursor_pane_id(), None);

        workspace.commit_workspace_mode();
        assert!(workspace.is_workspace_mode_active());
        assert_eq!(workspace.tab_count(), 0);

        workspace.cancel_workspace_mode();
        assert!(workspace.is_workspace_mode_active());
        assert_eq!(workspace.tab_count(), 0);
    }

    #[test]
    fn committing_with_no_cursor_still_exits_the_mode() {
        // `Enter` with nothing to commit to (zero tabs) must still clear
        // the raw "explicitly entered" bookkeeping -- even though
        // `is_workspace_mode_active()` keeps reporting `true` afterward
        // regardless, since the workspace is still empty (the zero-tab
        // bypass), so the getter itself can no longer distinguish
        // "committed" from "never entered" the way it could before this
        // bypass existed.
        let mut workspace = Workspace::mvp();
        workspace.terminate_active_session();
        workspace.enter_workspace_mode();

        workspace.commit_workspace_mode();

        assert!(!workspace.workspace_mode_active);
        assert!(workspace.is_workspace_mode_active());
    }
}
