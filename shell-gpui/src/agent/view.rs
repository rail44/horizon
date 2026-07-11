//! The agent pane view: transcript over the session entity's live
//! `AgentFrame`, a gpui-component `Input` composer (reuse over port —
//! its IME handling replaces the Floem composer's hand-rolled one), and
//! inline approval buttons. Rendering is deliberately simple — every
//! frame item paints as a block; the virtualized-List + Markdown
//! upgrade is recorded for the M5 polish pass.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use horizon_agent::contract::{MessageRole, SessionState};
use horizon_agent::frame::{pending_approval_call_ids_in, AgentFrameItem};

use super::session::AgentSession;
use crate::theme;

pub struct AgentView {
    session: Entity<AgentSession>,
    composer: Entity<InputState>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl AgentView {
    pub fn new(session: Entity<AgentSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder("Message the agent…"));
        let mut subscriptions = vec![cx.observe(&session, |_, _, cx| cx.notify())];
        subscriptions.push(cx.subscribe_in(
            &composer,
            window,
            |view: &mut AgentView, composer, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    let text = composer.read(cx).value().to_string();
                    if text.trim().is_empty() {
                        return;
                    }
                    view.session.read(cx).send_user_message(text);
                    composer.update(cx, |composer, cx| composer.set_value("", window, cx));
                }
            },
        ));
        let focus_handle = cx.focus_handle();

        Self {
            session,
            composer,
            focus_handle,
            _subscriptions: subscriptions,
        }
    }

    fn render_item(
        &self,
        index: usize,
        item: &AgentFrameItem,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let block = |label: &str, label_color: u32, text: String| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(label_color))
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(0xe9ecf2))
                        .child(text),
                )
                .into_any_element()
        };
        match item {
            AgentFrameItem::Message(message) => Some(match message.role {
                MessageRole::User => block("you", 0x84dcc6, message.text.clone()),
                MessageRole::Assistant => block("agent", 0x61afef, message.text.clone()),
            }),
            AgentFrameItem::AssistantTextDelta(delta) => {
                Some(block("agent…", 0x61afef, delta.text.clone()))
            }
            AgentFrameItem::ReasoningDelta(delta) => {
                Some(block("thinking", 0x5f6370, delta.text.clone()))
            }
            AgentFrameItem::ToolCallRequested(request) => Some(block(
                "tool",
                0xe5c07b,
                format!("{} {}", request.tool_id, request.input),
            )),
            AgentFrameItem::ToolCallFinished(result) => {
                let output = result.output.to_string();
                let clipped = if output.len() > 400 {
                    format!("{}…", &output[..output.floor_char_boundary(400)])
                } else {
                    output
                };
                Some(block("tool result", 0x98c379, clipped))
            }
            AgentFrameItem::ApprovalRequested(request) => {
                let pending = pending_approval_call_ids_in(&self.session.read(cx).frame.items)
                    .contains(&request.call_id);
                let call_id = request.call_id.clone();
                let deny_id = request.call_id.clone();
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(0xe5c07b))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(0xe5c07b))
                                .child(format!("approval requested: {}", request.reason)),
                        )
                        .when(pending, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(
                                        Button::new(("approve", index))
                                            .primary()
                                            .label("Approve")
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.session.read(cx).approve(call_id.clone());
                                            })),
                                    )
                                    .child(
                                        Button::new(("deny", index))
                                            .danger()
                                            .label("Deny")
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.session.read(cx).deny(deny_id.clone());
                                            })),
                                    ),
                            )
                        })
                        .into_any_element(),
                )
            }
            AgentFrameItem::ToolCallPreparing(progress) => Some(block(
                "tool (preparing)",
                0x5f6370,
                format!("{:?}", progress),
            )),
            AgentFrameItem::Error(error) => Some(block("error", 0xe06c75, format!("{error:?}"))),
            AgentFrameItem::Exited(reason) => {
                Some(block("exited", 0x8a90a0, format!("{reason:?}")))
            }
            AgentFrameItem::ToolCallStarted(_) => None,
        }
    }

    fn status_line(&self, cx: &App) -> String {
        match self.session.read(cx).frame.state {
            Some(SessionState::Running) => "running…".to_string(),
            Some(SessionState::ToolRunning) => "tool running…".to_string(),
            Some(SessionState::WaitingForApproval) => "waiting for approval".to_string(),
            Some(SessionState::WaitingForUser) | Some(SessionState::Created) | None => {
                String::new()
            }
            Some(SessionState::Cancelled) => "cancelled".to_string(),
            Some(SessionState::Completed) => "completed".to_string(),
            Some(SessionState::Failed) => "failed".to_string(),
            Some(SessionState::Terminated) => "terminated".to_string(),
        }
    }
}

impl Focusable for AgentView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Focusing the pane focuses the composer — the pane's one text
        // input surface.
        self.composer.read(cx).focus_handle(cx)
    }
}

impl Render for AgentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items: Vec<AnyElement> = {
            let frame_items = self.session.read(cx).frame.items.clone();
            frame_items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| self.render_item(index, item, cx))
                .collect()
        };
        let status = self.status_line(cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BACKGROUND))
            .track_focus(&self.focus_handle)
            .child(
                div()
                    .id("agent-transcript")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(items),
            )
            .when(!status.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .text_size(px(11.0))
                        .text_color(rgb(0x8a90a0))
                        .child(status),
                )
            })
            .child(div().p_2().child(Input::new(&self.composer)))
    }
}
