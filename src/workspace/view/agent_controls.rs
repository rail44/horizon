use std::time::Duration;

use super::composer_text::composer_text_view;
use crate::agent::contract::SessionState;
use crate::ui::spacing;
use crate::ui::theme;
use crate::workspace::AgentDraft;
use floem::prelude::*;
use floem::{
    action::set_ime_cursor_area,
    peniko::kurbo::{Point, Size},
};

/// Floor on the composer's height -- it grows past this for wrapped/
/// multi-line drafts (see `composer_text::ComposerTextView`), but never
/// shrinks below the room a single short line needs.
const COMPOSER_MIN_HEIGHT: f64 = 34.0;

/// The message-box composer: a single wrapped, multi-line `TextLayout`
/// (`composer_text::composer_text_view`) with a caret drawn at
/// `draft.cursor`, so the cursor's position is both visible and -- via
/// `AgentDraftAction::MoveLeft`/`MoveRight` (`app::keymap`) moving
/// `draft.cursor` -- reachable with the arrow keys. IME preedit is spliced
/// in right at the cursor too, matching where a composing IME actually
/// inserts once committed. See `docs/agent-composer-cursor-design.md`.
pub(super) fn agent_composer(
    visible: impl Fn() -> bool + 'static + Copy,
    active: impl Fn() -> bool + 'static + Copy,
    draft: RwSignal<AgentDraft>,
    preedit: impl Fn() -> Option<String> + 'static + Copy,
    ime_cursor_area: RwSignal<(Point, Size)>,
) -> impl IntoView {
    container(composer_text_view(draft, preedit, active).style(|s| s.width_full()))
        .style(move |s| {
            if !visible() {
                return s.hide();
            }

            let border = if active() {
                theme::accent()
            } else {
                theme::border_default()
            };

            s.width_full()
                .min_height(COMPOSER_MIN_HEIGHT)
                .items_center()
                .padding_horiz(spacing::SPACING_SM)
                .margin_horiz(spacing::SPACING_XS)
                .margin_bottom(spacing::SPACING_XS)
                .background(theme::surface_base())
                .border(1.0)
                .border_color(border)
        })
        .on_move(move |origin| {
            if active() && visible() {
                let position = origin + Point::new(10.0, 6.0).to_vec2();
                let size = Size::new(8.0, 18.0);
                ime_cursor_area.set((position, size));
                set_ime_cursor_area(position, size);
            }
        })
}

/// Compact pane-header label for an agent session's current state, e.g.
/// `"considering · 12s"` while a turn is in flight or plain `"cancelled"`
/// once it's settled. `elapsed` is `Some` only while ticking makes sense
/// (see the pane's `turn_in_flight`-gated timer) — otherwise the state name
/// is shown on its own. Satisfies `docs/ux-principles.md`'s Persistent UI
/// Requirement that the pane header show pane state.
pub(super) fn agent_pane_status_label(state: SessionState, elapsed: Option<Duration>) -> String {
    let label = agent_state_label(state);
    match elapsed {
        Some(elapsed) => format!("{label} · {}s", elapsed.as_secs()),
        None => label.to_string(),
    }
}

fn agent_state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Created => "starting",
        SessionState::Running => "considering",
        SessionState::WaitingForUser => "idle",
        SessionState::WaitingForApproval => "waiting for approval",
        SessionState::ToolRunning => "running tool",
        SessionState::Cancelled => "cancelled",
        SessionState::Completed => "done",
        SessionState::Failed => "failed",
        SessionState::Terminated => "terminated",
    }
}

#[cfg(test)]
mod tests {
    use super::agent_pane_status_label;
    use crate::agent::contract::SessionState;
    use std::time::Duration;

    #[test]
    fn agent_pane_status_label_appends_elapsed_when_present() {
        assert_eq!(
            agent_pane_status_label(SessionState::Running, Some(Duration::from_secs(12))),
            "considering · 12s"
        );
    }

    #[test]
    fn agent_pane_status_label_omits_elapsed_when_absent() {
        assert_eq!(
            agent_pane_status_label(SessionState::Cancelled, None),
            "cancelled"
        );
    }
}
