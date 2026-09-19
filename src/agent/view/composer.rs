//! Composer entity: input ownership, approval mode, session projection, and
//! send interaction. It stays uncached so the auto-growing input participates
//! in the parent flex layout on every intrinsic-height change.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Escape, InputEvent, Textarea, TextareaState};
use horizon_agent::contract::ToolCallId;
use horizon_agent::frame::state_indicates_turn_in_flight;

use super::super::{session::AgentSession, turns};
use super::AgentTranscript;
use crate::theme;
use crate::workspace::RunCommand;
use horizon_workspace::commands::CommandId;

const COMPOSER_MAX_ROWS: usize = 8;

/// Cross-entity projection consumed by transcript rows for the keyboard target
/// annotation. Emitting this explicit event keeps the transcript independent
/// of composer internals and prevents a render-time entity reach-through.
pub(super) enum ComposerEvent {
    ModeChanged(turns::ComposerMode),
}

pub(super) struct AgentComposer {
    session: Entity<AgentSession>,
    input: Entity<TextareaState>,
    transcript: WeakEntity<AgentTranscript>,
    model: Option<String>,
    turn_in_flight: bool,
    mode: turns::ComposerMode,
    dismissed_approval: Option<ToolCallId>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for AgentComposer {}

impl AgentComposer {
    pub(super) fn new(
        session: Entity<AgentSession>,
        transcript: WeakEntity<AgentTranscript>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (turn_in_flight, model, mode) = Self::project_session(&session, cx);
        // Plain Enter submits, Shift+Enter remains a newline, and auto-grow
        // preserves the one-row empty composer while allowing up to the cap.
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(turns::composer_placeholder(turn_in_flight))
                .auto_grow(1, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });

        let mut subscriptions =
            vec![
                cx.observe_in(&session, window, |composer: &mut Self, _, window, cx| {
                    composer.sync_session_projection(window, cx);
                }),
            ];
        subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |composer: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => {
                    if let turns::ComposerMode::Approval { call_id } = composer.mode.clone() {
                        composer.session.read(cx).approve(call_id);
                        return;
                    }
                    composer.send_message(window, cx);
                }
                InputEvent::Change => {
                    if let turns::ComposerMode::Approval { call_id } = composer.mode.clone() {
                        if !input.read(cx).value().is_empty() {
                            composer.dismissed_approval = Some(call_id);
                            composer.sync_mode(cx);
                        }
                    }
                    // The send-button affordance is rendered by this entity,
                    // outside `InputState`, so text transitions notify both.
                    cx.notify();
                }
                _ => {}
            },
        ));

        Self {
            session,
            input,
            transcript,
            model,
            turn_in_flight,
            mode,
            dismissed_approval: None,
            _subscriptions: subscriptions,
        }
    }

    fn project_session(
        session: &Entity<AgentSession>,
        cx: &App,
    ) -> (bool, Option<String>, turns::ComposerMode) {
        let session = session.read(cx);
        let turn_in_flight = state_indicates_turn_in_flight(session.frame.state);
        let model = turns::composer_model_chip(
            session.model.as_deref(),
            turns::latest_turn_model(&session.frame.items),
        )
        .map(str::to_string);
        let mode = turns::next_composer_mode(&session.pending_approval_call_ids(), None);
        (turn_in_flight, model, mode)
    }

    fn sync_session_projection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (turn_in_flight, model, queue) = {
            let session = self.session.read(cx);
            let turn_in_flight = state_indicates_turn_in_flight(session.frame.state);
            let model = turns::composer_model_chip(
                session.model.as_deref(),
                turns::latest_turn_model(&session.frame.items),
            )
            .map(str::to_string);
            (turn_in_flight, model, session.pending_approval_call_ids())
        };

        if self.turn_in_flight != turn_in_flight {
            self.input.update(cx, |input, cx| {
                input.set_placeholder(turns::composer_placeholder(turn_in_flight), window, cx);
            });
            self.turn_in_flight = turn_in_flight;
        }
        let model_changed = self.model != model;
        self.model = model;
        self.set_mode(
            turns::next_composer_mode(&queue, self.dismissed_approval.as_ref()),
            cx,
        );
        if model_changed {
            cx.notify();
        }
    }

    fn sync_mode(&mut self, cx: &mut Context<Self>) {
        let queue = self.session.read(cx).pending_approval_call_ids();
        self.set_mode(
            turns::next_composer_mode(&queue, self.dismissed_approval.as_ref()),
            cx,
        );
    }

    fn set_mode(&mut self, mode: turns::ComposerMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode.clone();
            cx.emit(ComposerEvent::ModeChanged(mode));
        }
    }

    /// `Input` propagates an otherwise-unhandled Escape to this container.
    /// Approval mode consumes it as Deny; normal mode keeps propagating.
    fn on_escape(&mut self, _: &Escape, _window: &mut Window, cx: &mut Context<Self>) {
        if let turns::ComposerMode::Approval { call_id } = self.mode.clone() {
            self.session.read(cx).deny(call_id, None);
        } else {
            cx.propagate();
        }
    }

    /// The shared Enter/button send path. A weak transcript reference preserves
    /// send-to-tail repinning without creating an entity ownership cycle.
    fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.session.read(cx).send_user_message(text);
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let _ = self
            .transcript
            .update(cx, |transcript, cx| transcript.repin_to_tail(cx));
    }

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
            .on_click(cx.listener(|composer, _, window, cx| {
                composer.send_message(window, cx);
            }))
            .into_any_element()
    }
}

impl Focusable for AgentComposer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for AgentComposer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // This entity stays ordinary (uncached): the row's height follows the
        // input entity as it auto-grows, and the model/send controls remain
        // bottom-aligned beside it.
        let has_text = !self.input.read(cx).value().trim().is_empty();
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
                    .child(Textarea::new(&self.input).appearance(false)),
            );
        // The model chip is always present on an agent pane: it doubles as
        // the provider→model picker's entry point (parent task #1's Phase
        // 2), so a session whose model is not (yet) resolved -- a provider
        // whose key is missing, running deterministic fallback -- still gets
        // a switch affordance instead of a silently absent control.
        row = row.child(
            div()
                .pb(px(4.0))
                .child(render_model_chip(self.model.clone())),
        );
        row = row.child(
            div()
                .pb(px(2.0))
                .child(self.render_send_button(has_text, cx)),
        );
        div().px(px(8.0)).pb(px(8.0)).child(row)
    }
}

fn composer_border() -> Hsla {
    theme::text_subtle().alpha(0.4)
}

/// The chip's label: the current model id, or the `Model…` placeholder while
/// no model is resolved yet. Free-standing so the fallback is unit-testable
/// without a GPUI window.
fn model_chip_label(model: Option<&str>) -> String {
    model
        .map(str::to_string)
        .unwrap_or_else(|| "Model…".to_string())
}

/// The model chip: shows the session's current model (or the `Model…`
/// placeholder while none is resolved) and opens the provider→model picker
/// on click. The click dispatches `CommandId::SwitchModel` through the same
/// [`RunCommand`] gpui action the stop/continue buttons use
/// (`src/agent/view/transcript.rs`) -- AGENTS.md's "operations go through
/// the command model" convention -- so palette, `[keybindings]`, and this
/// pointer path all funnel through `WorkspaceShell::execute`, which resolves
/// the active agent session the same way for all of them. Stateful
/// (`.id("composer-model-chip")`) because gpui only attaches click handlers
/// to interactive elements; the id is static for the same reason the send
/// button's is (`composer-send`).
fn render_model_chip(model: Option<String>) -> AnyElement {
    div()
        .id("composer-model-chip")
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
        .cursor_pointer()
        .hover(|style| style.border_color(theme::text_subtle().alpha(0.6)))
        .on_click(|_, window, cx| {
            window.dispatch_action(
                Box::new(RunCommand {
                    id: CommandId::SwitchModel,
                }),
                cx,
            );
        })
        .child(model_chip_label(model.as_deref()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::model_chip_label;

    #[test]
    fn the_chip_label_falls_back_to_a_placeholder_when_no_model_is_resolved() {
        assert_eq!(model_chip_label(Some("m-opus")), "m-opus");
        assert_eq!(model_chip_label(None), "Model…");
    }
}
