//! Composer interaction: the approval-aware Enter/Escape wiring
//! (`sync_composer_mode`, `on_escape`), the one send path
//! (`send_composer_message`), and the composer's own rendering (input
//! row, model chip, send button).

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Escape, Input};

use super::super::follow::FollowState;
use super::super::turns;
use crate::theme;

use super::AgentView;

impl AgentView {
    /// Recomputes [`turns::ComposerMode`] from the session's live
    /// actionable pending-approval queue and this view's own
    /// `dismissed_approval` marker, delegating the actual no-flap
    /// decision to the pure `turns::next_composer_mode` (colocated tests
    /// there) -- this method's only job is wiring the queue and marker
    /// into it. Called from the session-change observer (covers a
    /// new/resolved approval from any of the four paths -- row buttons,
    /// palette, CLI, or a keyboard decision through this same mode) and
    /// from the composer's own `InputEvent::Change` handler (typing past
    /// a shown approval).
    pub(super) fn sync_composer_mode(&mut self, cx: &mut Context<Self>) {
        let queue = self.session.read(cx).pending_approval_call_ids();
        self.composer_mode = turns::next_composer_mode(&queue, self.dismissed_approval.as_ref());
    }

    /// The approval-mode composer's Deny binding (decision 4: "Deny
    /// esc"). Wired as an `on_action` on the composer's own container
    /// div rather than through `InputState` directly: gpui-component's
    /// `Input` consumes `Escape` for its own concerns (inline-completion
    /// dismissal, IME-mark clearing) but otherwise calls `cx.propagate()`
    /// (`crates/ui/src/input/state.rs`'s `InputState::escape`, verified
    /// against the vendored gpui-component source at the pinned rev --
    /// Horizon never opts the composer into `clean_on_escape`, the one
    /// case that would swallow it instead), so the already-resolved
    /// `Escape` action keeps bubbling up the element tree to this
    /// container's own handler exactly the way gpui-component's own
    /// `SearchPanel` catches it (`crates/ui/src/input/search.rs`). No
    /// `AgentPaneFocus`-style key context exists in the GPUI shell (per
    /// the amendment's current-state note) -- this handler lives on the
    /// composer's own container instead, scoped to just its mode.
    fn on_escape(&mut self, _: &Escape, _window: &mut Window, cx: &mut Context<Self>) {
        if let turns::ComposerMode::Approval { call_id } = self.composer_mode.clone() {
            self.session.read(cx).deny(call_id);
        } else {
            cx.propagate();
        }
    }

    /// The composer's one send implementation: trims/empty-guards, sends
    /// through `AgentSession::send_user_message`, clears the composer, and
    /// re-pins the transcript to the bottom (sending always re-pins,
    /// wherever the user had scrolled to -- requirement 4 of decision 7's
    /// GPUI port: composer send always re-enters `Sticky`, explicitly,
    /// not by way of `follow::on_wheel_scroll`). Shared by the
    /// `PressEnter` subscription above and the composer's send button
    /// (`render_send_button`, the mock's circular `↑`) -- exactly one send
    /// path, so an empty-composer Enter and an empty-composer button click
    /// both no-op identically rather than each carrying its own guard.
    pub(super) fn send_composer_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.session.read(cx).send_user_message(text);
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        self.follow = FollowState::Sticky;
        self.transcript_scroll.scroll_to_bottom();
    }

    /// The composer area: Horizon's own bordered, rounded container
    /// (`composer_border`'s stronger-than-subtle border, see that
    /// function's doc comment) holding a chromeless auto-grow `Input`
    /// (`Input::appearance(false)` -- gpui-component's own no-border/
    /// no-background switch, so the container supplies all the chrome)
    /// with the read-only model-id pill ([`turns::composer_model_chip`]
    /// -- known from session start via `AgentSession::model`; see that
    /// function's doc comment for the precedence against
    /// [`turns::latest_turn_model`] -- no `▾` since no switcher is
    /// wired) and the circular accent send button inline on the right,
    /// bottom-anchored so they stay put while the input grows.
    ///
    /// Layout revised 2026-07-16 (owner feedback): the 2026-07-13 mock's
    /// stacked arrangement -- input row above a separate accessory row --
    /// reserved several text rows of height even when the composer was
    /// empty, and its 20px/18px outer margins read too wide against the
    /// transcript's 8px padding. The accessory row is folded inline
    /// (single row when empty, one text row tall) and the outer margins
    /// now match the transcript's rhythm. Still wrapped so
    /// [`Self::on_escape`] catches the `Escape` action `Input`'s own
    /// handler propagates (see that method's doc comment) --
    /// `composer_mode`'s Enter/Esc keyboard capture is unchanged, only
    /// the rendering around it.
    pub(super) fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let model = {
            let session = self.session.read(cx);
            turns::composer_model_chip(
                session.model.as_deref(),
                turns::latest_turn_model(&session.frame.items),
            )
            .map(str::to_string)
        };
        let has_text = !self.composer.read(cx).value().trim().is_empty();

        let mut row = div()
            .flex()
            .flex_row()
            .items_end()
            .gap_2()
            .px(px(6.0))
            .py(px(4.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(composer_border())
            .on_action(cx.listener(Self::on_escape))
            .child(
                div()
                    .flex_1()
                    .child(Input::new(&self.composer).appearance(false)),
            );
        if let Some(model) = model {
            // The pill rides the send button's baseline; pb centers it
            // against the button's 26px height on the shared bottom edge.
            row = row.child(div().pb(px(4.0)).child(render_model_chip(model)));
        }
        row = row.child(
            div()
                .pb(px(2.0))
                .child(self.render_send_button(has_text, cx)),
        );

        div().px(px(8.0)).pb(px(8.0)).child(row).into_any_element()
    }

    /// The composer's send button (mock: a circular accent `↑` at the
    /// accessory row's right): dispatches
    /// [`Self::send_composer_message`], the exact same path `PressEnter`
    /// uses. `has_text` only mutes the button's color when the composer is
    /// empty -- `send_composer_message` already no-ops on an empty/
    /// whitespace value, so the click handler itself needs no separate
    /// guard (one send implementation, not two).
    fn render_send_button(&self, has_text: bool, cx: &mut Context<Self>) -> AnyElement {
        let (bg, fg) = if has_text {
            (theme::accent(), theme::on_accent())
        } else {
            (theme::text_subtle().alpha(0.3), theme::text_subtle())
        };
        div()
            .id("composer-send")
            .flex()
            .items_center()
            .justify_center()
            .w(px(26.0))
            .h(px(26.0))
            .rounded_full()
            .bg(bg)
            .when(has_text, |this| this.cursor_pointer())
            .child(div().text_size(px(13.0)).text_color(fg).child("↑"))
            .on_click(cx.listener(|view, _, window, cx| {
                view.send_composer_message(window, cx);
            }))
            .into_any_element()
    }
}

/// The composer's own border (`render_composer`): a step stronger than
/// the receipt rows' subtle border (`theme::text_subtle().alpha(0.25)`,
/// matching the mock's `#e4e4e7`), mirroring the mock's own composer
/// chrome, which borders with the visibly darker `#d4d4d8` on the same
/// white canvas its rows use `#e4e4e7` on -- the composer reads as the
/// pane's one persistent, more-present surface, not just another muted
/// row.
fn composer_border() -> Hsla {
    theme::text_subtle().alpha(0.4)
}

/// The composer's read-only model-id pill (mock: `claude-sonnet-4`, no
/// `▾` -- no switcher is wired, see `render_composer`'s doc comment):
/// reuses the same subtle border role the receipt rows use, matching the
/// mock's own chip, which borders with the same `#e4e4e7` its rows do.
fn render_model_chip(model: String) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .rounded(px(5.0))
        .border_1()
        .border_color(theme::text_subtle().alpha(0.25))
        .px(px(8.0))
        .py(px(3.0))
        .text_size(px(11.0))
        .text_color(theme::text_muted())
        .child(model)
        .into_any_element()
}
