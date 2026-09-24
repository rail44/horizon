//! The agent pane is a lightweight composite over independently owned
//! child entities. The transcript owns the message scroller (gpui-component's
//! tail-following virtual list) and is the only cached child; status and the
//! auto-growing composer retain ordinary GPUI layout so their intrinsic
//! heights can change.

mod composer;
mod status;
mod tasks;
mod transcript;

use gpui::*;
use gpui_component::StyledExt as _;

use super::session::AgentSession;
use super::turns;
use composer::AgentComposer;
use status::AgentStatus;
use tasks::BackgroundTasks;
use transcript::AgentTranscript;

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
