//! The agent pane redrawn to the board's rules, over a display model of its
//! own ([`display`]) so it builds for the preview plugin's target as well.
//!
//! [`AgentNextView`] holds a [`Transcript`](display::Transcript) handed in
//! from outside, a [`Layout`], and its own UI state (which receipts and
//! call bodies are open). It never reaches the shell, a runtime, or a
//! session: everything the owner asks for leaves as an [`AgentNextEvent`].
//!
//! Every layout puts the same things in the same places: the composer
//! holds the input, the model chip and the send hint only; the session's
//! state and 「停止」 ride on the running turn; an approval sits under the
//! call that asks for it; background tasks and a session that can no
//! longer run end the column. Neither speaker is labeled, a turn's
//! receipts merge into one line, and notices are plain lines. The layouts
//! differ in one area each ([`units`]):
//!
//! - [`Layout::Baseline`]: one card per turn, the receipt line first; while
//!   the turn runs, the session's state opens that line and 「停止」 ends it.
//! - [`Layout::BoxedComposer`]: [`Layout::Baseline`] with the composer's
//!   field and hint row in one bordered box instead of a bordered strip.
//! - [`Layout::BorderlessCalls`]: [`Layout::Baseline`] with the expanded
//!   call rows under a left rule rather than in a bordered box.
//! - [`Layout::ElapsedOnly`]: [`Layout::Baseline`] with a finished turn's
//!   right end showing its elapsed time without the model.

pub(crate) mod display;
pub(crate) mod previews;
mod spec;
mod units;

use std::collections::HashSet;

use gpui::*;
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex, ActiveTheme as _};

use crate::theme;
use crate::ui::parts::key_chip;
use crate::ui::spec::*;
use display::{ComposerMode, Status, Transcript, Unit};
use spec::{thread_gutter, COMPOSER_MAX_ROWS, COMPOSER_MIN_ROWS};

/// Which arrangement of the units the view draws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Layout {
    Baseline,
    BoxedComposer,
    BorderlessCalls,
    ElapsedOnly,
}

/// What the view asks of whatever holds it.
// Only a shell subscriber would read the payloads, and none is wired yet.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentNextEvent {
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
pub(crate) struct AgentNextView {
    transcript: Transcript,
    layout: Layout,
    ui: UiState,
    composer: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Whether the column keeps its tail in view as it grows. A wheel
    /// scroll away from the tail drops it; scrolling back to it restores it.
    follow_tail: bool,
    focus_handle: FocusHandle,
    _composer_subscription: Subscription,
}

impl EventEmitter<AgentNextEvent> for AgentNextView {}

impl AgentNextView {
    pub(crate) fn new(
        transcript: Transcript,
        layout: Layout,
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
        window.focus(&focus_handle, cx);
        Self {
            transcript,
            layout,
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
    #[allow(dead_code)]
    pub(crate) fn set_transcript(
        &mut self,
        transcript: Transcript,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if placeholder(transcript.status) != placeholder(self.transcript.status) {
            let text = placeholder(transcript.status);
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
                cx.emit(AgentNextEvent::Approve(id.clone()));
            }
            return;
        }
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(AgentNextEvent::Send(text));
    }

    /// Escape while waiting on an approval with nothing typed denies it.
    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        let ComposerMode::Approval { id } = &self.transcript.composer else {
            return;
        };
        if self.composer.read(cx).value().trim().is_empty() {
            cx.emit(AgentNextEvent::Deny(id.clone()));
            cx.stop_propagation();
        }
    }

    // -- composition --------------------------------------------------------

    /// The scroll column's children for the current layout.
    fn units(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mono = cx.theme().mono_font_family.clone();
        let status = self.transcript.status;
        let mut children = Vec::new();
        for (index, unit) in self.transcript.units.iter().enumerate() {
            children.push(match unit {
                Unit::User(message) => units::user(index, message).into_any_element(),
                Unit::Notice(notice) => units::notice(notice).into_any_element(),
                Unit::Turn(turn) => {
                    units::turn(index, turn, status, self.layout, &self.ui, &mono, cx)
                }
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
        let strip = v_flex().w_full().flex_none().px(thread_gutter());
        if self.layout == Layout::BoxedComposer {
            return strip.pb(pad_y()).child(
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
            );
        }
        strip
            .py(pad_y())
            .gap(gap_label())
            .border_t_1()
            .border_color(theme::border())
            .child(field)
            .child(hints)
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
                    .child("で送信"),
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
        Status::Terminated => "このセッションは終了しています",
        Status::Unreachable => "ランタイムに到達できません",
        _ => "メッセージを書く",
    }
}

impl Focusable for AgentNextView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AgentNextView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.track_tail();
        let column = self.units(cx);
        v_flex()
            .id("agent-next")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
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
                    .gap(gap_unit())
                    .children(column),
            )
            .child(self.render_composer(cx))
    }
}
