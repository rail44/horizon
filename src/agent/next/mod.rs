//! The agent pane redrawn to the board's rules, over a display model of its
//! own ([`display`]) so it builds for the preview plugin's target as well.
//!
//! [`AgentNextView`] holds a [`Transcript`](display::Transcript) handed in
//! from outside, a [`Layout`], and its own UI state (which receipts and
//! call bodies are open, whether the changes overview is). It never reaches
//! the shell, a runtime, or a session: everything the owner asks for leaves
//! as an [`AgentNextEvent`].
//!
//! The three layouts draw the same units ([`units`]):
//!
//! - [`Layout::TurnCards`]: one card per turn; the session's status, model,
//!   changes and stop control in the composer's hint row.
//! - [`Layout::PerItem`]: each turn part as its own unit — text as a card,
//!   a receipt as a bare activity line between cards — with the composer
//!   of [`Layout::TurnCards`].
//! - [`Layout::StateOnCard`]: the turn cards of [`Layout::TurnCards`], the
//!   session's state and stop control on the running turn's card, and the
//!   composer drawn as the next message sent into the session.

pub(crate) mod display;
pub(crate) mod previews;
mod spec;
mod units;

use std::collections::HashSet;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex, ActiveTheme as _};

use crate::theme;
use crate::ui::spec::*;
use display::{ComposerMode, Status, Transcript, TurnPart, Unit};
use spec::{thread_gutter, user_indent, COMPOSER_MAX_ROWS, COMPOSER_MIN_ROWS};

/// Which arrangement of the units the view draws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Layout {
    TurnCards,
    PerItem,
    StateOnCard,
}

impl Layout {
    fn turn_cards(self) -> bool {
        matches!(self, Self::TurnCards | Self::StateOnCard)
    }
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
    pub(crate) overview_open: bool,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder(&transcript.composer))
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
            ui: UiState::default(),
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
        if transcript.composer != self.transcript.composer {
            let text = placeholder(&transcript.composer);
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

    pub(super) fn toggle_overview(&mut self, cx: &mut Context<Self>) {
        self.ui.overview_open = !self.ui.overview_open;
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
        let carried = (self.layout == Layout::StateOnCard).then_some(self.transcript.status);
        let mut children = Vec::new();
        for (index, unit) in self.transcript.units.iter().enumerate() {
            match unit {
                Unit::User(message) => {
                    children.push(units::user(index, message).into_any_element())
                }
                Unit::Notice(notice) => children.push(units::notice(notice).into_any_element()),
                Unit::Turn(turn) if self.layout.turn_cards() => children.push(
                    units::turn_card(index, turn, carried, &self.ui, &mono, cx).into_any_element(),
                ),
                Unit::Turn(turn) => {
                    for (part_index, part) in turn.parts.iter().enumerate() {
                        children.push(match part {
                            TurnPart::Text(text) => {
                                units::text_card(units::part_id(index, part_index), text)
                                    .into_any_element()
                            }
                            TurnPart::Receipt(receipt) => {
                                units::receipt_block(receipt, true, &self.ui, &mono, cx)
                                    .into_any_element()
                            }
                            TurnPart::Thinking => {
                                units::thinking(index).px(pad_x()).into_any_element()
                            }
                        });
                    }
                }
            }
        }
        children
    }

    /// The left half of the hint row: the session's status and controls,
    /// less whatever the running turn's card already carries.
    fn session_group(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let transcript = &self.transcript;
        let on_card = self.layout == Layout::StateOnCard;
        let show_status =
            !on_card || matches!(transcript.status, Status::Terminated | Status::Unreachable);
        h_flex()
            .items_center()
            .gap(gap_tight())
            .when(show_status, |group| {
                group.child(units::status_chip(transcript.status))
            })
            .child(units::model_chip(&transcript.model, cx))
            .children(units::changes_chip(&transcript.changes, cx))
            .when(!on_card && transcript.status.running(), |group| {
                group.child(units::stop_button("agent-next-hint-stop", cx))
            })
    }

    fn field(&self) -> impl IntoElement {
        div()
            .w_full()
            .text_size(body().size)
            .child(Textarea::new(&self.composer).appearance(false))
    }

    fn hint_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .gap(gap_label())
            .child(self.session_group(cx))
            .child(div().flex_1().min_w_0())
            .children(units::key_hints(&self.transcript.composer))
    }

    /// The composer as a strip under the column.
    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .flex_none()
            .px(thread_gutter())
            .py(pad_y())
            .gap(gap_label())
            .border_t_1()
            .border_color(theme::border())
            .when(self.ui.overview_open, |composer| {
                composer.child(units::overview(&self.transcript.changes).mt(gap_tight()))
            })
            .child(self.field())
            .child(self.hint_row(cx))
    }

    /// The composer as the next message sent into the session.
    fn render_next_message(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_none()
            .ml(thread_gutter() + user_indent())
            .mr(thread_gutter())
            .mb(gap_unit())
            .px(pad_x())
            .py(pad_y())
            .rounded(RADIUS)
            .bg(theme::tint_over_background(
                theme::text_primary(),
                BAND_TINT,
            ))
            .gap(gap_label())
            .child(self.field())
            .when(self.ui.overview_open, |composer| {
                composer.child(units::overview(&self.transcript.changes).mt(gap_tight()))
            })
            .child(self.hint_row(cx))
    }

    fn render_tasks(&self) -> impl IntoElement {
        let tasks = v_flex()
            .w_full()
            .flex_none()
            .px(thread_gutter())
            .children(units::task_lines(&self.transcript.tasks));
        if self.layout == Layout::StateOnCard {
            tasks.pb(gap_tight())
        } else {
            tasks
                .py(gap_tight())
                .border_t_1()
                .border_color(theme::border())
        }
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

fn placeholder(mode: &ComposerMode) -> &'static str {
    match mode {
        ComposerMode::Idle => "メッセージを書く",
        ComposerMode::Running => "次のターンで送る",
        ComposerMode::Approval { .. } => "承認を待っています",
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
        let next_message = self.layout == Layout::StateOnCard;
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
                    .pb(if next_message {
                        gap_unit()
                    } else {
                        gap_composer()
                    })
                    .gap(gap_unit())
                    .children(column),
            )
            .when(!self.transcript.tasks.is_empty(), |view| {
                view.child(self.render_tasks())
            })
            .map(|view| {
                if next_message {
                    view.child(self.render_next_message(cx).into_any_element())
                } else {
                    view.child(self.render_composer(cx).into_any_element())
                }
            })
    }
}
