//! The agent pane, drawn over a display model of its own ([`display`]) so
//! the view builds for the preview plugin's target as well. The native-only
//! [`adapter`] builds that model from a live session and [`AgentPane`]
//! feeds it to the view.
//!
//! [`AgentView`] holds a [`Transcript`](display::Transcript) handed in
//! from outside and its own UI state (which receipts and call bodies are
//! open). It never reaches the shell, a runtime, or a session: everything
//! the owner asks for leaves as an [`AgentViewEvent`].
//!
//! The column is a run of units ([`units`]), with no speaker labels:
//!
//! - A message sent into the session sits indented on a grey tint.
//! - An agent turn is one bordered card. Its receipts merge into one line
//!   at its head, followed by its text. While the turn runs, the session's
//!   state opens that line, `Stop` ends it and an accent bar runs down the
//!   card's left edge; once done, the line ends with the turn's elapsed
//!   time. A click on the line opens its calls under a left rule, each row
//!   a status dot and its title, the verb muted and the target in the
//!   monospace family; a click on a row opens its body: a diff with its
//!   `+`/`−` in a gutter cell, or a command's output drawn without a box.
//!   A pending approval is one line under the call that asks for it.
//! - A notice is a plain line.
//!
//! Background tasks and the notice of a session that can no longer run end
//! the column. Below it, the composer is one bordered box holding only the
//! input field, which starts at one row, the model chip, and the send hint.
//! Every fixed label comes from [`chrome`].

#[cfg(not(target_family = "wasm"))]
mod adapter;
pub(crate) mod chrome;
pub(crate) mod display;
#[cfg(not(target_family = "wasm"))]
mod pane;
#[cfg(not(target_family = "wasm"))]
pub(crate) use pane::AgentPane;
pub(crate) mod previews;
mod spec;
mod units;

use std::collections::HashSet;

use gpui::*;
use gpui_component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex, ActiveTheme as _};

use crate::theme;
use crate::ui::parts::key_chip;
use crate::ui::spec::*;
use display::{ComposerMode, Status, Transcript, Unit};
use spec::{thread_gutter, COMPOSER_MAX_ROWS, COMPOSER_MIN_ROWS};

/// What the view asks of whatever holds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentViewEvent {
    Send(String),
    Approve(String),
    Deny(String),
    Stop,
    SwitchModel,
}

/// The view's own state: nothing here comes from the session.
#[derive(Clone, Debug, Default)]
pub(crate) struct UiState {
    /// Receipts opened by a click, by [`display::Receipt::key`]. A running
    /// receipt is open regardless.
    pub(crate) expanded_receipts: HashSet<usize>,
    /// Call bodies opened by a click, by receipt key and call index.
    pub(crate) open_bodies: HashSet<(usize, usize)>,
}

/// The agent pane over a display model.
pub(crate) struct AgentView {
    transcript: Transcript,
    ui: UiState,
    composer: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Whether the column keeps its tail in view as it grows. A wheel
    /// scroll away from the tail drops it; scrolling back to it restores it.
    follow_tail: bool,
    focus_handle: FocusHandle,
    _composer_subscription: Subscription,
}

impl EventEmitter<AgentViewEvent> for AgentView {}

impl AgentView {
    pub(crate) fn new(
        transcript: Transcript,
        ui: UiState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder(transcript.status))
                .auto_grow(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let _composer_subscription = cx.subscribe_in(
            &composer,
            window,
            |view, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.submit(window, cx);
                }
            },
        );
        let focus_handle = cx.focus_handle();
        window.focus(&composer.read(cx).focus_handle(cx), cx);
        Self {
            transcript,
            ui,
            composer,
            scroll: ScrollHandle::new(),
            follow_tail: true,
            focus_handle,
            _composer_subscription,
        }
    }

    /// Replaces what the view shows. The UI state is kept: a receipt that
    /// was open stays open across an update of the same session.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn set_transcript(
        &mut self,
        transcript: Transcript,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = placeholder(transcript.status);
        if text != placeholder(self.transcript.status) {
            self.composer
                .update(cx, |input, cx| input.set_placeholder(text, window, cx));
        }
        self.transcript = transcript;
        cx.notify();
    }

    pub(super) fn toggle_receipt(&mut self, key: usize, cx: &mut Context<Self>) {
        if !self.ui.expanded_receipts.remove(&key) {
            self.ui.expanded_receipts.insert(key);
        }
        cx.notify();
    }

    pub(super) fn toggle_body(&mut self, receipt: usize, call: usize, cx: &mut Context<Self>) {
        if !self.ui.open_bodies.remove(&(receipt, call)) {
            self.ui.open_bodies.insert((receipt, call));
        }
        cx.notify();
    }

    /// Enter in the composer: send what it holds, or, waiting on an
    /// approval with nothing typed, approve.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            if let ComposerMode::Approval { id } = &self.transcript.composer {
                cx.emit(AgentViewEvent::Approve(id.clone()));
            }
            return;
        }
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(AgentViewEvent::Send(text));
    }

    /// Escape while waiting on an approval with nothing typed denies it.
    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.deny_pending(cx) {
            cx.stop_propagation();
        }
    }

    /// The composer field's own Escape, which the field passes on when it
    /// has nothing to do with it.
    fn on_escape(&mut self, _: &Escape, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.deny_pending(cx) {
            cx.propagate();
        }
    }

    /// Denies the pending approval when nothing is typed; whether it did.
    fn deny_pending(&mut self, cx: &mut Context<Self>) -> bool {
        let ComposerMode::Approval { id } = &self.transcript.composer else {
            return false;
        };
        if !self.composer.read(cx).value().trim().is_empty() {
            return false;
        }
        cx.emit(AgentViewEvent::Deny(id.clone()));
        true
    }

    // -- composition --------------------------------------------------------

    /// The scroll column's children.
    fn units(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mono = cx.theme().mono_font_family.clone();
        let status = self.transcript.status;
        let mut children = Vec::new();
        for (index, unit) in self.transcript.units.iter().enumerate() {
            children.push(match unit {
                Unit::User(message) => units::user(index, message).into_any_element(),
                Unit::Notice(notice) => units::notice(notice).into_any_element(),
                Unit::Turn(turn) => units::turn(index, turn, status, &self.ui, &mono, cx),
            });
        }
        if let Some(end) = units::session_end(status) {
            children.push(units::notice(&end).into_any_element());
        }
        children
            .extend(units::task_lines(&self.transcript.tasks).map(IntoElement::into_any_element));
        children
    }

    /// The composer: the field, the model chip, and the send hint.
    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let field = div()
            .w_full()
            .text_size(body().size)
            .child(Textarea::new(&self.composer).appearance(false));
        let hints = self.render_composer_hints(cx);
        v_flex()
            .w_full()
            .flex_none()
            .px(thread_gutter())
            .pb(pad_y())
            .child(
                v_flex()
                    .w_full()
                    .border_1()
                    .border_color(theme::border())
                    .rounded(RADIUS)
                    .px(pad_x())
                    .py(gap_tight())
                    .gap(gap_label())
                    .child(field)
                    .child(hints),
            )
    }

    /// The model chip at the left and the send hint at the right.
    fn render_composer_hints(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .items_center()
            .gap(gap_label())
            .child(units::model_chip(&self.transcript.model, cx))
            .child(div().flex_1().min_w_0())
            .child(key_chip("Enter"))
            .child(
                div()
                    .text_size(micro().size)
                    .line_height(micro().line_height)
                    .text_color(theme::text_muted())
                    .child(chrome::SEND_HINT),
            )
    }

    /// Keeps the tail in view while following it, and resumes following
    /// once the owner scrolls back down to it.
    fn track_tail(&mut self) {
        let at_tail = self.scroll.offset().y <= -self.scroll.max_offset().y + px(1.0);
        if !self.follow_tail && at_tail {
            self.follow_tail = true;
        }
        if self.follow_tail {
            self.scroll.scroll_to_bottom();
        }
    }
}

fn placeholder(status: Status) -> &'static str {
    match status {
        Status::Terminated => chrome::PLACEHOLDER_ENDED,
        Status::Unreachable => chrome::PLACEHOLDER_UNREACHABLE,
        _ => chrome::PLACEHOLDER,
    }
}

impl Focusable for AgentView {
    /// The composer's field, so focusing the pane types into it.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }
}

impl Render for AgentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.track_tail();
        let column = self.units(cx);
        v_flex()
            .id("agent-next")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .on_action(cx.listener(Self::on_escape))
            .size_full()
            .min_w(VIEW_MIN_WIDTH)
            .bg(rgb(theme::background()))
            .text_color(theme::text_primary())
            .child(
                v_flex()
                    .id("agent-next-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_scroll_wheel(cx.listener(|view, _: &ScrollWheelEvent, _, _| {
                        view.follow_tail = false;
                    }))
                    .px(thread_gutter())
                    .pt(gap_section())
                    .pb(gap_composer())
                    .gap(gap_tight())
                    .children(column),
            )
            .child(self.render_composer(cx))
    }
}
