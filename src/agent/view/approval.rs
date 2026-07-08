//! Inline tool-call approval (`docs/agent-output-ui-design.md` decision 8):
//! the pane-internal focus/optimistic-answer plumbing moved here verbatim
//! from the pre-slice-4 approval banner (`workspace::view::agent_controls`),
//! plus the new [`approval_control_row`] view rendered at the bottom of the
//! tool block that requested the approval instead of a separate banner row.

use std::rc::Rc;

use floem::prelude::*;

use crate::agent::contract::ToolCallId;
use crate::ui::fonts::font_family;
use crate::ui::hint_chip::key_hint;
use crate::ui::spacing;
use crate::ui::theme;

use super::transcript::ToolBlock;

/// Which of an agent pane's two pane-internal focus targets currently owns
/// the keyboard: the message-box composer, or the inline approval control
/// row of the oldest pending tool call (see [`approval_control_row`]). Only
/// meaningful for agent panes -- terminal panes have no approval row and
/// their pane never leaves `MessageBox`. Named `Banner` pre-slice-4, when
/// this was a pane-bottom banner rather than inlined into the tool block.
///
/// Design: the tool-approval interaction takes its "every interaction
/// request visibly explains which key does what" cue from crush
/// (charmbracelet's TUI) -- see `ui::hint_chip::key_hint` for the inline
/// `[y] approve` bindings this drives, and `workspace::input::
/// handle_agent_banner_key` for the key routing this focus state gates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AgentPaneFocus {
    #[default]
    MessageBox,
    Approval,
}

/// Computes the pane's next approval focus, if it should change, after the
/// oldest pending tool-call approval (`AgentFrame::pending_approval_call_id`,
/// gated by `gate_pending_approval`) goes from `previous_pending` to
/// `pending`.
///
/// Returns `None` when the pending call id didn't actually change -- an
/// unrelated frame update, or the same call still pending after the user
/// pressed `Esc` to leave the control row early (a no-op refresh must not
/// undo that). Otherwise: a *new* call becoming the oldest pending one (the
/// session's first pending approval, or the next one in a queue once the
/// previous oldest resolved) grabs focus; no call pending any more releases
/// it back to the message box. This covers both halves of the design -- the
/// approval row "takes... focus" the instant a request arrives, and "focus
/// returns to the message box" the instant it resolves -- regardless of how
/// the resolution happened, since a key, a click, and a palette action all
/// converge on the same `pending_approval` signal.
pub(crate) fn next_agent_pane_focus(
    previous_pending: Option<ToolCallId>,
    pending: Option<ToolCallId>,
) -> Option<AgentPaneFocus> {
    if previous_pending == pending {
        return None;
    }
    Some(if pending.is_some() {
        AgentPaneFocus::Approval
    } else {
        AgentPaneFocus::MessageBox
    })
}

/// The call id the approval row should currently treat as awaiting an
/// answer: `pending` itself, unless it's already been answered locally
/// (`answered` -- see the module doc below on the optimistic-state fix), in
/// which case `None`. Backs both the render-time enablement of the y/n
/// buttons/`esc` hint and the dispatch guard in `pane_view`'s `KeyDown`
/// handler, so a key or click can never re-dispatch for a call that's
/// already been locked in -- however long the resolution takes to round
/// trip back. See the module doc on the 2026-07 repeated-approval OOM
/// incident this closes the client-side half of (the authoritative,
/// server-side half is `agent::tools::approval`'s idempotence guard).
pub(crate) fn awaiting_call(
    pending: Option<ToolCallId>,
    answered: Option<ToolCallId>,
) -> Option<ToolCallId> {
    match pending {
        Some(call_id) if answered.as_ref() != Some(&call_id) => Some(call_id),
        _ => None,
    }
}

/// Whether the oldest pending call has already been answered locally and is
/// now just waiting for the resolution to round-trip back -- the "answered
/// -- running…" state that replaces the interactive y/n buttons.
fn is_awaiting_resolution(pending: Option<ToolCallId>, answered: Option<ToolCallId>) -> bool {
    match pending {
        Some(call_id) => answered == Some(call_id),
        None => false,
    }
}

/// Computes the pane's next locally-recorded "answered" marker (the
/// optimistic-state fix's memory of "a keypress/click already locked this
/// call in") after the oldest pending approval goes from `previous_pending`
/// to `pending`. Returns `None` when the pending call id is unchanged (a
/// no-op refresh must leave a still-pending answered marker alone) --
/// otherwise always clears it to `Some(None)`: the call either resolved (no
/// marker needed any more) or a new one just became pending (which must
/// start in the normal, unanswered state). Mirrors `next_agent_pane_focus`'s
/// exact shape, driven by the same `pending_approval` transitions --
/// `pane_view` folds both into the one effect.
pub(crate) fn next_answered_call(
    previous_pending: Option<ToolCallId>,
    pending: Option<ToolCallId>,
) -> Option<Option<ToolCallId>> {
    if previous_pending == pending {
        None
    } else {
        Some(None)
    }
}

/// Gates the pane's pending approval behind its "cancel requested" latch.
///
/// Once the user clicks Cancel, approve/deny must go dead immediately — not
/// after the provider's cancel events round-trip. In that window the call
/// still looks pending in the frame, and an Approve click would really
/// execute the tool while history records the call as cancelled. The latch
/// is set the instant the cancel action fires and cleared when the frame's
/// session state next changes (by then the cancelled call has resolved, or
/// the cancel was ignored and the approval is genuinely still pending).
pub(crate) fn gate_pending_approval(
    cancel_requested: bool,
    pending: Option<ToolCallId>,
) -> Option<ToolCallId> {
    if cancel_requested {
        None
    } else {
        pending
    }
}

/// Bundles the pane-level approval plumbing the inline control row
/// (`approval_control_row`) needs: which of the session's pending tool
/// calls is the oldest (only it gets an actionable row -- targeting
/// discipline, the same rule the pre-slice-4 banner followed), how many
/// more are queued behind it, the local "already answered" marker, and the
/// approve/deny dispatchers themselves.
///
/// A plain struct of `Rc<dyn Fn>` rather than more generic parameters
/// threaded through every nested view constructor (`agent_frame_view` ->
/// `transcript_block_view` -> `transcript_body_view` ->
/// `tool_view::tool_body_view`) -- `pane_view` builds one of these per agent
/// pane and clones it cheaply (`Rc::clone` per field) into each tool
/// block's view, the same way `agent_frame_view` already clones its
/// `block_view_ids: Rc<RefCell<..>>` per block.
#[derive(Clone)]
pub(crate) struct ApprovalController {
    pending_call_id: Rc<dyn Fn() -> Option<ToolCallId>>,
    extra_pending: Rc<dyn Fn() -> usize>,
    answered: Rc<dyn Fn() -> Option<ToolCallId>>,
    on_approve: Rc<dyn Fn(ToolCallId)>,
    on_deny: Rc<dyn Fn(ToolCallId)>,
}

impl ApprovalController {
    pub(crate) fn new(
        pending_call_id: impl Fn() -> Option<ToolCallId> + 'static,
        extra_pending: impl Fn() -> usize + 'static,
        answered: impl Fn() -> Option<ToolCallId> + 'static,
        on_approve: impl Fn(ToolCallId) + 'static,
        on_deny: impl Fn(ToolCallId) + 'static,
    ) -> Self {
        Self {
            pending_call_id: Rc::new(pending_call_id),
            extra_pending: Rc::new(extra_pending),
            answered: Rc::new(answered),
            on_approve: Rc::new(on_approve),
            on_deny: Rc::new(on_deny),
        }
    }

    /// Exposed at `pub(super)` (rather than private) so `mod.rs`'s
    /// `agent_frame_view` can watch it directly for the forced-scroll-in
    /// effect (decision 8's "承認待ちが来たら強制的にそこへ") -- the same
    /// pending-call-id transition [`next_agent_pane_focus`] reacts to.
    pub(super) fn pending_call_id(&self) -> Option<ToolCallId> {
        (self.pending_call_id)()
    }

    fn extra_pending(&self) -> usize {
        (self.extra_pending)()
    }

    fn answered(&self) -> Option<ToolCallId> {
        (self.answered)()
    }
}

fn block_call_id(tool: RwSignal<ToolBlock>) -> Option<ToolCallId> {
    tool.with(|tool| tool.call_id.clone())
}

fn block_needs_confirmation(tool: RwSignal<ToolBlock>) -> bool {
    tool.with(ToolBlock::needs_confirmation)
}

fn block_approval_reason(tool: RwSignal<ToolBlock>) -> String {
    tool.with(|tool| tool.approval.clone())
        .map(|approval| approval.reason)
        .unwrap_or_default()
}

/// Whether this block's call is the oldest pending one -- the only one the
/// row's y/n buttons/`+N more` hint ever apply to, regardless of whether
/// it's already been answered locally (see [`is_actionable`] for that
/// narrower check).
fn is_head(tool: RwSignal<ToolBlock>, controller: &ApprovalController) -> bool {
    let call_id = block_call_id(tool);
    call_id.is_some() && call_id == controller.pending_call_id()
}

/// Whether this block's call is the one the y/n buttons/`esc` hint actually
/// act on right now -- the head, and not already answered locally.
fn is_actionable(tool: RwSignal<ToolBlock>, controller: &ApprovalController) -> bool {
    let call_id = block_call_id(tool);
    call_id.is_some()
        && call_id == awaiting_call(controller.pending_call_id(), controller.answered())
}

/// Whether this block's call is the head, already answered locally, and
/// just waiting for the resolution round-trip (the "answered -- running…"
/// state).
fn is_awaiting_this_blocks_resolution(
    tool: RwSignal<ToolBlock>,
    controller: &ApprovalController,
) -> bool {
    is_head(tool, controller)
        && is_awaiting_resolution(controller.pending_call_id(), controller.answered())
}

/// The inline approval control row (`docs/agent-output-ui-design.md`
/// decision 8), rendered at the bottom of a tool block's body while
/// [`super::transcript::ToolBlock::needs_confirmation`] holds: the approval
/// reason, then either the y/n buttons (only for the oldest pending call --
/// "targeting discipline", the rule the pre-slice-4 banner also followed),
/// an "answered -- running…" label once the optimistic local answer is in,
/// or a "queued" notice for a call that isn't the oldest yet. Hidden
/// entirely once the call resolves.
pub(crate) fn approval_control_row(
    tool: RwSignal<ToolBlock>,
    controller: ApprovalController,
) -> impl IntoView {
    let row_visible = move || block_needs_confirmation(tool);

    let reason_label = label(move || block_approval_reason(tool)).style(move |s| {
        if !row_visible() {
            return s.hide();
        }
        s.flex_basis(0.0)
            .flex_grow(1.0)
            .min_width(0.0)
            .font_family(font_family().to_string())
            .font_size(11)
            .color(theme::text_subtle())
    });

    let approve_click = controller.clone();
    let approve_gate = controller.clone();
    let approve_button = approval_button(
        "y",
        "approve",
        tool,
        approve_gate,
        move |call_id| (approve_click.on_approve)(call_id),
        theme::approval_confirm_surface(),
        theme::accent(),
    );

    let deny_click = controller.clone();
    let deny_gate = controller.clone();
    let deny_button = approval_button(
        "n",
        "deny",
        tool,
        deny_gate,
        move |call_id| (deny_click.on_deny)(call_id),
        theme::approval_deny_surface(),
        theme::danger(),
    );

    let esc_controller = controller.clone();
    let esc_hint = key_hint("esc", "back to draft").style(move |s| {
        if !row_visible() || !is_actionable(tool, &esc_controller) {
            return s.hide();
        }
        s
    });

    let waiting_controller = controller.clone();
    let waiting_label = label(|| "answered — running…".to_string()).style(move |s| {
        if !row_visible() || !is_awaiting_this_blocks_resolution(tool, &waiting_controller) {
            return s.hide();
        }
        s.font_family(font_family().to_string())
            .font_size(11)
            .color(theme::text_subtle())
    });

    let extra_text_controller = controller.clone();
    let extra_style_controller = controller.clone();
    let extra_label = label(move || format!("+{} more", extra_text_controller.extra_pending()))
        .style(move |s| {
            if !row_visible()
                || !is_head(tool, &extra_style_controller)
                || extra_style_controller.extra_pending() == 0
            {
                return s.hide();
            }
            s.font_family(font_family().to_string())
                .font_size(11)
                .color(theme::text_subtle())
        });

    let queued_controller = controller.clone();
    let queued_label = label(|| "queued behind an earlier approval".to_string()).style(move |s| {
        if !row_visible() || is_head(tool, &queued_controller) {
            return s.hide();
        }
        s.font_family(font_family().to_string())
            .font_size(11)
            .color(theme::text_subtle())
    });

    h_stack((
        reason_label,
        approve_button,
        deny_button,
        esc_hint,
        waiting_label,
        extra_label,
        queued_label,
    ))
    .style(move |s| {
        if !row_visible() {
            return s.hide();
        }
        s.width_full()
            .min_height(30)
            .items_center()
            .gap(10)
            .padding_horiz(spacing::SPACING_XS)
            .padding_vert(spacing::SPACING_XS)
    })
}

fn approval_button(
    key_label: &'static str,
    action_label: &'static str,
    tool: RwSignal<ToolBlock>,
    controller: ApprovalController,
    on_click: impl Fn(ToolCallId) + 'static,
    background: floem::peniko::Color,
    border: floem::peniko::Color,
) -> impl IntoView {
    let click_controller = controller.clone();
    key_hint(key_label, action_label)
        .on_click_stop(move |_| {
            if is_actionable(tool, &click_controller) {
                if let Some(call_id) = block_call_id(tool) {
                    on_click(call_id);
                }
            }
        })
        .style(move |s| {
            if !is_actionable(tool, &controller) {
                return s.hide();
            }
            s.height(26)
                .padding_horiz(spacing::SPACING_SM)
                .items_center()
                .justify_center()
                .background(background)
                .border(1.0)
                .border_color(border)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        awaiting_call, gate_pending_approval, is_awaiting_resolution, next_agent_pane_focus,
        next_answered_call, AgentPaneFocus,
    };
    use crate::agent::contract::ToolCallId;

    #[test]
    fn cancel_request_gates_pending_approval() {
        let pending = Some(ToolCallId("call-1".to_string()));
        assert_eq!(gate_pending_approval(true, pending), None);
        assert_eq!(gate_pending_approval(true, None), None);
    }

    #[test]
    fn pending_approval_passes_through_without_cancel_request() {
        let pending = Some(ToolCallId("call-1".to_string()));
        assert_eq!(gate_pending_approval(false, pending.clone()), pending);
        assert_eq!(gate_pending_approval(false, None), None);
    }

    // --- approval focus transitions (`next_agent_pane_focus`) -------------

    #[test]
    fn approval_row_auto_focuses_when_a_call_first_becomes_pending() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(
            next_agent_pane_focus(None, call),
            Some(AgentPaneFocus::Approval)
        );
    }

    #[test]
    fn approval_row_releases_focus_once_the_call_resolves() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(
            next_agent_pane_focus(call, None),
            Some(AgentPaneFocus::MessageBox)
        );
    }

    #[test]
    fn approval_row_stays_focused_when_the_oldest_pending_call_changes() {
        // The previous oldest resolved, revealing the next one in the queue
        // -- the row should keep focus rather than release it.
        let first = Some(ToolCallId("call-1".to_string()));
        let second = Some(ToolCallId("call-2".to_string()));
        assert_eq!(
            next_agent_pane_focus(first, second),
            Some(AgentPaneFocus::Approval)
        );
    }

    #[test]
    fn approval_focus_is_unchanged_when_the_pending_call_id_is_the_same() {
        // Must be a true no-op: this is what lets `Esc` (which doesn't
        // change the pending call, only the focus state) survive an
        // unrelated frame refresh without being fought back to `Approval`.
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(next_agent_pane_focus(call.clone(), call), None);
        assert_eq!(next_agent_pane_focus(None, None), None);
    }

    // --- optimistic answer state (`awaiting_call`/`next_answered_call`) ---

    #[test]
    fn awaiting_call_is_the_pending_call_when_unanswered() {
        let call = ToolCallId("call-1".to_string());
        assert_eq!(awaiting_call(Some(call.clone()), None), Some(call));
    }

    #[test]
    fn awaiting_call_is_none_once_the_pending_call_has_been_answered() {
        let call = ToolCallId("call-1".to_string());
        assert_eq!(awaiting_call(Some(call.clone()), Some(call)), None);
    }

    #[test]
    fn awaiting_call_is_none_when_nothing_is_pending() {
        assert_eq!(awaiting_call(None, None), None);
    }

    #[test]
    fn awaiting_call_ignores_a_stale_answered_marker_for_a_different_call() {
        // A previous call's answered marker must not gate a brand new
        // pending call -- `next_answered_call` is what's responsible for
        // clearing it, but this proves the render/dispatch guard itself is
        // keyed by call id, not just by "something was answered".
        let first = ToolCallId("call-1".to_string());
        let second = ToolCallId("call-2".to_string());
        assert_eq!(
            awaiting_call(Some(second.clone()), Some(first)),
            Some(second)
        );
    }

    #[test]
    fn next_answered_call_is_unchanged_when_pending_call_id_is_the_same() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(next_answered_call(call.clone(), call), None);
        assert_eq!(next_answered_call(None, None), None);
    }

    #[test]
    fn next_answered_call_clears_once_the_pending_call_resolves() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(next_answered_call(call, None), Some(None));
    }

    #[test]
    fn next_answered_call_clears_when_a_new_call_takes_over() {
        let first = Some(ToolCallId("call-1".to_string()));
        let second = Some(ToolCallId("call-2".to_string()));
        assert_eq!(next_answered_call(first, second), Some(None));
    }

    #[test]
    fn is_awaiting_resolution_true_once_the_pending_call_matches_the_answered_marker() {
        let call = ToolCallId("call-1".to_string());
        assert!(is_awaiting_resolution(Some(call.clone()), Some(call)));
    }

    #[test]
    fn is_awaiting_resolution_false_when_unanswered_or_nothing_pending() {
        let call = ToolCallId("call-1".to_string());
        assert!(!is_awaiting_resolution(Some(call), None));
        assert!(!is_awaiting_resolution(None, None));
    }
}
