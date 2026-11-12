//! What a board row reports about the session bound to it is
//! `horizon-board-view`'s `activity`, shared with the Blitz shell. What is
//! here is the color each state reads in, in gpui's units.

use gpui::Hsla;

pub(crate) use horizon_board_view::activity::*;

use crate::theme;

pub(crate) fn activity_color(activity: BoardSessionActivity) -> Hsla {
    match activity {
        BoardSessionActivity::Failed => theme::danger(),
        BoardSessionActivity::Running
        | BoardSessionActivity::ToolRunning
        | BoardSessionActivity::WaitingForApproval => theme::accent(),
        _ => theme::text_muted(),
    }
}
