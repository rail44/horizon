//! The agent pane is a lightweight composite over three independently owned
//! child entities. The transcript owns the message scroller (gpui-component's
//! tail-following virtual list) and is the only cached child; status and the
//! auto-growing composer retain ordinary GPUI layout so their intrinsic
//! heights can change.

mod composer;
mod rows;
mod scroll;
mod status;
mod tasks;
mod transcript;

use std::collections::HashSet;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_component::StyledExt as _;
use horizon_agent::contract::ToolCallId;

use super::session::AgentSession;
use super::turns;
use composer::{AgentComposer, ComposerEvent};
use scroll::RunningTurnClock;
use status::AgentStatus;
use tasks::BackgroundTasks;
use transcript::{build_transcript_rows, TranscriptRow};

/// The stable, expensive portion of an agent pane. Session updates project
/// into compact row descriptors here; `Render` constructs only visible rows.
pub(super) struct AgentTranscript {
    session: Entity<AgentSession>,
    scroller: Entity<MessageScrollerState>,
    transcript_rows: Vec<TranscriptRow>,
    latest_user_row: Option<usize>,
    session_changes: Vec<turns::FileChange>,
    running_turn_clock: Option<RunningTurnClock>,
    expanded_receipts: HashSet<usize>,
    expanded_rows: HashSet<ToolCallId>,
    changes_expanded: bool,
    /// Explicitly projected from `AgentComposer` events so row keyboard-target
    /// annotation never reaches across into the composer entity during render.
    composer_mode: turns::ComposerMode,
    _subscriptions: Vec<Subscription>,
}

impl AgentTranscript {
    fn new(
        session: Entity<AgentSession>,
        composer_mode: turns::ComposerMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let (transcript_rows, latest_user_row, session_changes) = {
            let session = session.read(cx);
            let (rows, latest_user) = build_transcript_rows(&session.frame.items);
            let calls = turns::build_tool_call_views(&session.frame.items);
            (rows, latest_user, turns::aggregate_changes(&calls))
        };
        let scroller = cx.new(|cx| MessageScrollerState::new(transcript_rows.len(), cx));

        let subscriptions =
            vec![
                cx.observe(&session, |transcript: &mut AgentTranscript, _, cx| {
                    transcript.sync_running_turn_clock(cx);
                    transcript.sync_transcript_rows(cx);
                    cx.notify();
                }),
            ];

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |transcript, cx| {
                if transcript.running_turn_clock.is_some() {
                    cx.notify();
                }
            });
            if alive.is_err() {
                return;
            }
        })
        .detach();

        Self {
            session,
            scroller,
            transcript_rows,
            latest_user_row,
            session_changes,
            running_turn_clock: None,
            expanded_receipts: HashSet::new(),
            expanded_rows: HashSet::new(),
            changes_expanded: false,
            composer_mode,
            _subscriptions: subscriptions,
        }
    }

    fn bind_composer(&mut self, composer: &Entity<AgentComposer>, cx: &mut Context<Self>) {
        self._subscriptions.push(cx.subscribe(
            composer,
            |transcript, _, event: &ComposerEvent, cx| {
                let ComposerEvent::ModeChanged(mode) = event;
                if transcript.composer_mode != *mode {
                    transcript.composer_mode = mode.clone();
                    transcript
                        .scroller
                        .update(cx, |scroller, cx| scroller.remeasure(cx));
                    cx.notify();
                }
            },
        ));
    }

    fn repin_to_tail(&mut self, cx: &mut Context<Self>) {
        // `scroll_to_end` re-engages tail following (message_scroller's own
        // contract), so this is the same explicit re-pin the composer's send
        // path has always performed.
        self.scroller
            .update(cx, |scroller, cx| scroller.scroll_to_end(cx));
        cx.notify();
    }
}

impl Render for AgentTranscript {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let changes_bar = self.render_changes_bar(&self.session_changes, cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // The scroller owns tail-following, the scrollbar, the
                    // jump-to-latest button, and the bottom fade; the
                    // hand-rolled follow pill is gone with them.
                    .child({
                        let transcript_view = cx.entity();
                        MessageScroller::new(
                            "transcript-rows",
                            self.scroller.clone(),
                            move |row_index, window, cx| {
                                transcript_view.update(cx, |transcript, cx| {
                                    transcript.render_transcript_row(row_index, window, cx)
                                })
                            },
                        )
                        .with_content_style(StyleRefinement::default().pt_2())
                        .size_full()
                    }),
            )
            .when_some(changes_bar, |this, bar| this.child(bar))
    }
}

/// The fixed-bounds transcript cache. The backing entity is private so the
/// agent layout can only embed it through the cached conversion below.
struct TranscriptSurface {
    view: Entity<AgentTranscript>,
}

impl TranscriptSurface {
    fn new(view: Entity<AgentTranscript>) -> Self {
        Self { view }
    }

    fn element(&self) -> AnyElement {
        self.view
            .clone()
            .cached(StyleRefinement::default().v_flex().size_full())
            .into_any_element()
    }
}

/// Uncached pane composite. It deliberately owns no session entity or session
/// subscription, so rendering the shell cannot clone/read a live agent frame.
pub(crate) struct AgentView {
    transcript: TranscriptSurface,
    tasks: Entity<BackgroundTasks>,
    status: Entity<AgentStatus>,
    composer: Entity<AgentComposer>,
    focus_handle: FocusHandle,
}

impl AgentView {
    pub(crate) fn new(
        session: Entity<AgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_mode =
            turns::next_composer_mode(&session.read(cx).pending_approval_call_ids(), None);
        let transcript =
            cx.new(|cx| AgentTranscript::new(session.clone(), initial_mode.clone(), cx));
        let composer =
            cx.new(|cx| AgentComposer::new(session.clone(), transcript.downgrade(), window, cx));
        transcript.update(cx, |transcript, cx| {
            transcript.bind_composer(&composer, cx);
        });
        let tasks = cx.new(|cx| BackgroundTasks::new(session.clone(), cx));
        let status = cx.new(|cx| AgentStatus::new(session, cx));
        let focus_handle = composer.read(cx).focus_handle(cx);

        Self {
            transcript: TranscriptSurface::new(transcript),
            tasks,
            status,
            composer,
            focus_handle,
        }
    }
}

impl Focusable for AgentView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AgentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(crate::theme::background()))
            // Apply the config-driven font family (`[ui] font_family`) and
            // size (`[terminal] font_size`) at the pane root so every child
            // -- the Markdown transcript (gpui-component's `TextView`, which
            // reads `window.text_style()`), the composer `Input`, the status
            // line, and the tool-call rows -- inherits the same font as the
            // terminal instead of falling back to gpui-component's built-in
            // default. Children with their own `.text_size()` (labels, tool
            // rows, status) keep their deliberate sub-sizes.
            .font(crate::terminal::resolved_font())
            .text_size(px(crate::terminal::font_size()))
            .track_focus(&self.focus_handle)
            // Transcript presses focus the pressed TextView (gpui-component's
            // window text selection claims focus on mouse-down), so a click
            // that never selected anything -- the plain "click to read"
            // case -- hands the keyboard back to the composer; a real drag
            // selection keeps the transcript focused so cmd-c copies it
            // (the TextView's own Copy action reads the window selection).
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    if gpui_base::TextSelection::selected_text(window, cx)
                        .trim()
                        .is_empty()
                    {
                        window.focus(&this.focus_handle, cx);
                    }
                }),
            )
            // The wrapper gets a definite flex allocation first; the cached
            // transcript then fills those exact bounds. Auto-grow composer and
            // status remain outside the cache and keep intrinsic sizing.
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .child(self.transcript.element()),
            )
            .child(self.tasks.clone())
            .child(self.status.clone())
            .child(self.composer.clone())
    }
}
