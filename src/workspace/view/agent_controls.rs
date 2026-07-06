use std::time::Duration;

use crate::agent::contract::{SessionState, ToolCallId, ToolCallRequest};
use crate::ui::fonts::font_family;
use crate::ui::hint_chip::key_hint;
use crate::ui::theme;
use floem::prelude::*;
use floem::{
    action::set_ime_cursor_area,
    peniko::kurbo::{Point, Size},
};
use serde_json::Value;

pub(super) fn agent_composer(
    visible: impl Fn() -> bool + 'static + Copy,
    active: impl Fn() -> bool + 'static + Copy,
    draft: RwSignal<String>,
    preedit: impl Fn() -> Option<String> + 'static + Copy,
    ime_cursor_area: RwSignal<(Point, Size)>,
) -> impl IntoView {
    label(move || {
        let text = draft.get();
        let preedit = preedit().unwrap_or_default();
        if text.is_empty() && preedit.is_empty() {
            "Message agent...".to_string()
        } else if preedit.is_empty() {
            text
        } else {
            format!("{text}{preedit}")
        }
    })
    .style(move |s| {
        if !visible() {
            return s.hide();
        }

        let border = if active() {
            theme::accent()
        } else {
            theme::border_default()
        };
        let color = if draft.with(|text| text.is_empty()) && preedit().is_none() {
            theme::text_subtle()
        } else {
            theme::text_primary()
        };

        s.width_full()
            .height(34)
            .min_height(34)
            .items_center()
            .padding_horiz(10)
            .margin_horiz(8)
            .margin_bottom(7)
            .font_family(font_family().to_string())
            .font_size(12)
            .line_height(1.2)
            .color(color)
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

/// Which of an agent pane's two pane-internal focus targets currently owns
/// the keyboard: the message-box composer, or the approval banner (see
/// `agent_approval_banner` below). Only meaningful for agent panes --
/// terminal panes have no banner and their pane never leaves `MessageBox`.
///
/// Design: the tool-approval interaction takes its "every interaction
/// request visibly explains which key does what" cue from crush
/// (charmbracelet's TUI) -- see `ui::hint_chip::key_hint` for the inline
/// `[y] approve` bindings this drives, and `workspace::input::
/// handle_agent_banner_key` for the key routing this focus state gates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum AgentPaneFocus {
    #[default]
    MessageBox,
    Banner,
}

/// Computes the pane's next approval-banner focus, if it should change,
/// after the oldest pending tool-call approval (`AgentFrame::
/// pending_approval_call_id`, gated by `gate_pending_approval`) goes from
/// `previous_pending` to `pending`.
///
/// Returns `None` when the pending call id didn't actually change -- an
/// unrelated frame update, or the same call still pending after the user
/// pressed `Esc` to leave the banner early (a no-op refresh must not undo
/// that). Otherwise: a *new* call becoming the oldest pending one (the
/// session's first pending approval, or the next one in a queue once the
/// previous oldest resolved) grabs the banner; no call pending any more
/// releases it back to the message box. This covers both halves of the
/// design -- the banner "takes... focus" the instant a request arrives, and
/// "focus returns to the message box" the instant it resolves -- regardless
/// of how the resolution happened, since a key, a click, and a palette
/// action all converge on the same `pending_approval` signal.
pub(super) fn next_agent_pane_focus(
    previous_pending: Option<ToolCallId>,
    pending: Option<ToolCallId>,
) -> Option<AgentPaneFocus> {
    if previous_pending == pending {
        return None;
    }
    Some(if pending.is_some() {
        AgentPaneFocus::Banner
    } else {
        AgentPaneFocus::MessageBox
    })
}

/// The call id the banner should currently treat as awaiting an answer:
/// `pending` itself, unless it's already been answered locally (`answered`
/// -- see the module doc below on the optimistic banner-state fix), in
/// which case `None`. Backs both the render-time enablement of the y/n
/// buttons/`esc` hint and the dispatch guard in `pane_view`'s `KeyDown`
/// handler, so a key or click can never re-dispatch for a call that's
/// already been locked in -- however long the resolution takes to round
/// trip back. See the module doc on the 2026-07 repeated-approval OOM
/// incident this closes the client-side half of (the authoritative,
/// server-side half is `agent::tools::approval`'s idempotence guard).
pub(super) fn awaiting_call(
    pending: Option<ToolCallId>,
    answered: Option<ToolCallId>,
) -> Option<ToolCallId> {
    match pending {
        Some(call_id) if answered.as_ref() != Some(&call_id) => Some(call_id),
        _ => None,
    }
}

/// Whether the banner's pending call has already been answered locally and
/// is now just waiting for the resolution to round-trip back -- the
/// "answered -- running…" state that replaces the interactive y/n buttons.
fn is_awaiting_resolution(pending: Option<ToolCallId>, answered: Option<ToolCallId>) -> bool {
    match pending {
        Some(call_id) => answered == Some(call_id),
        None => false,
    }
}

/// Computes the pane's next locally-recorded "answered" marker (the
/// optimistic banner-state fix's memory of "a keypress/click already locked
/// this call in") after the oldest pending approval goes from
/// `previous_pending` to `pending`. Returns `None` when the pending call id
/// is unchanged (a no-op refresh must leave a still-pending answered marker
/// alone) -- otherwise always clears it to `Some(None)`: the call either
/// resolved (no marker needed any more) or a new one just became pending
/// (which must start in the normal, unanswered state). Mirrors
/// `next_agent_pane_focus`'s exact shape, driven by the same
/// `pending_approval` transitions -- `pane_view` folds both into the one
/// effect.
pub(super) fn next_answered_call(
    previous_pending: Option<ToolCallId>,
    pending: Option<ToolCallId>,
) -> Option<Option<ToolCallId>> {
    if previous_pending == pending {
        None
    } else {
        Some(None)
    }
}

const MAX_SUMMARY_LINES: usize = 3;
const MAX_SUMMARY_CHARS: usize = 200;

/// Renders the substance of a pending tool-call approval for the banner
/// (`docs/agent-tools-design.md`'s "Show what is being approved"): the
/// command line itself for `bash` (the first `MAX_SUMMARY_LINES` lines and
/// `MAX_SUMMARY_CHARS` characters, whichever is hit first, with an ellipsis
/// marker), or the target path for `fs.write`/`fs.edit`. Falls back to the
/// bare tool id for anything else -- Horizon only ever gates these three
/// tools behind approval today (`agent::tools::approval::
/// is_horizon_executed_tool`), but the banner must still render *something*
/// sane if that ever changes.
pub(super) fn describe_pending_call(request: &ToolCallRequest) -> String {
    match request.tool_id.as_str() {
        "bash" => request
            .input
            .get("command")
            .and_then(Value::as_str)
            .map(truncate_summary)
            .unwrap_or_else(|| request.tool_id.clone()),
        "fs.write" | "fs.edit" => request
            .input
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| request.tool_id.clone()),
        other => other.to_string(),
    }
}

fn truncate_summary(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().take(MAX_SUMMARY_LINES + 1).collect();
    let line_truncated = lines.len() > MAX_SUMMARY_LINES;
    lines.truncate(MAX_SUMMARY_LINES);
    let mut joined = lines.join("\n");

    let char_truncated = joined.chars().count() > MAX_SUMMARY_CHARS;
    if char_truncated {
        joined = joined.chars().take(MAX_SUMMARY_CHARS).collect();
    }

    if line_truncated || char_truncated {
        joined.push('…');
    }
    joined
}

/// The approval banner: renders only the oldest pending tool call's
/// approve/deny controls (targeting discipline -- banner keys and buttons
/// alike act on the oldest pending call only; cross-session approvals stay
/// palette-only) with their key bindings spelled out inline
/// (`ui::hint_chip::key_hint`), plus a summary of what's being approved
/// (`describe_pending_call`) and a "+N more" hint (`extra_pending`) for
/// additional calls queued behind the one shown.
///
/// **Optimistic banner state** (fix for the 2026-07 repeated-approval OOM
/// incident: a banner that didn't visibly react to a held `y` key re-sent
/// `Approve` for the same still-running `bash` call 134 times in 29
/// seconds, spawning 134 concurrent processes). `answered` is the pane's
/// locally-recorded "this call was already answered" marker
/// (`next_answered_call` computes its transitions; `pane_view` owns the
/// signal and clears it once the resolution round-trips back). While the
/// pending call matches it, the y/n buttons and `esc` hint hide behind an
/// "answered -- running…" label instead -- disabled regardless of how long
/// the round trip takes, independent of `pane_view`'s keyboard-focus
/// release (which only protects the *keyboard* path; a mouse click on a
/// button is gated here).
pub(super) fn agent_approval_banner(
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    answered: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    pending_summary: impl Fn() -> Option<String> + 'static + Copy,
    extra_pending: impl Fn() -> usize + 'static + Copy,
    on_approve: impl Fn(ToolCallId) + 'static,
    on_deny: impl Fn(ToolCallId) + 'static,
) -> impl IntoView {
    let awaiting_resolution = move || is_awaiting_resolution(pending_approval(), answered());

    h_stack((
        label(move || pending_summary().unwrap_or_default()).style(move |s| {
            if !visible() || pending_approval().is_none() {
                return s.hide();
            }

            s.font_family(font_family().to_string())
                .font_size(11)
                .color(theme::text_subtle())
        }),
        agent_approval_button(
            "y",
            "approve",
            visible,
            pending_approval,
            answered,
            on_approve,
            theme::approval_confirm_surface(),
            theme::accent(),
        ),
        agent_approval_button(
            "n",
            "deny",
            visible,
            pending_approval,
            answered,
            on_deny,
            theme::approval_deny_surface(),
            theme::danger(),
        ),
        key_hint("esc", "back to draft").style(move |s| {
            if !visible() || pending_approval().is_none() || awaiting_resolution() {
                return s.hide();
            }
            s
        }),
        label(|| "answered — running…".to_string()).style(move |s| {
            if !visible() || !awaiting_resolution() {
                return s.hide();
            }

            s.font_family(font_family().to_string())
                .font_size(11)
                .color(theme::text_subtle())
        }),
        label(move || format!("+{} more", extra_pending())).style(move |s| {
            if extra_pending() == 0 {
                return s.hide();
            }

            s.font_family(font_family().to_string())
                .font_size(11)
                .color(theme::text_subtle())
        }),
    ))
    .style(move |s| {
        if !visible() || pending_approval().is_none() {
            return s.hide();
        }

        s.width_full()
            .height(30)
            .min_height(30)
            .items_center()
            .justify_end()
            .padding_horiz(8)
            .gap(10)
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

/// Gates the pane's pending approval behind its "cancel requested" latch.
///
/// Once the user clicks Cancel, approve/deny must go dead immediately — not
/// after the provider's cancel events round-trip. In that window the call
/// still looks pending in the frame, and an Approve click would really
/// execute the tool while history records the call as cancelled. The latch
/// is set the instant the cancel action fires and cleared when the frame's
/// session state next changes (by then the cancelled call has resolved, or
/// the cancel was ignored and the approval is genuinely still pending).
pub(super) fn gate_pending_approval(
    cancel_requested: bool,
    pending: Option<ToolCallId>,
) -> Option<ToolCallId> {
    if cancel_requested {
        None
    } else {
        pending
    }
}

#[allow(clippy::too_many_arguments)]
fn agent_approval_button(
    key_label: &'static str,
    action_label: &'static str,
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    answered: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    on_click: impl Fn(ToolCallId) + 'static,
    background: floem::peniko::Color,
    border: floem::peniko::Color,
) -> impl IntoView {
    key_hint(key_label, action_label)
        .on_click_stop(move |_| {
            if let Some(call_id) = awaiting_call(pending_approval(), answered()) {
                on_click(call_id);
            }
        })
        .style(move |s| {
            if !visible() || awaiting_call(pending_approval(), answered()).is_none() {
                return s.hide();
            }

            s.height(26)
                .padding_horiz(10)
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
        agent_pane_status_label, awaiting_call, describe_pending_call, gate_pending_approval,
        is_awaiting_resolution, next_agent_pane_focus, next_answered_call, AgentPaneFocus,
        MAX_SUMMARY_CHARS, MAX_SUMMARY_LINES,
    };
    use crate::agent::contract::{SessionState, ToolCallId, ToolCallRequest};
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

    // --- approval banner focus transitions (`next_agent_pane_focus`) -----

    #[test]
    fn banner_auto_focuses_when_a_call_first_becomes_pending() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(
            next_agent_pane_focus(None, call),
            Some(AgentPaneFocus::Banner)
        );
    }

    #[test]
    fn banner_releases_focus_once_the_call_resolves() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(
            next_agent_pane_focus(call, None),
            Some(AgentPaneFocus::MessageBox)
        );
    }

    #[test]
    fn banner_stays_focused_when_the_oldest_pending_call_changes() {
        // The previous oldest resolved, revealing the next one in the queue
        // -- the banner should keep focus rather than release it.
        let first = Some(ToolCallId("call-1".to_string()));
        let second = Some(ToolCallId("call-2".to_string()));
        assert_eq!(
            next_agent_pane_focus(first, second),
            Some(AgentPaneFocus::Banner)
        );
    }

    #[test]
    fn banner_focus_is_unchanged_when_the_pending_call_id_is_the_same() {
        // Must be a true no-op: this is what lets `Esc` (which doesn't
        // change the pending call, only the focus state) survive an
        // unrelated frame refresh without being fought back to `Banner`.
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(next_agent_pane_focus(call.clone(), call), None);
        assert_eq!(next_agent_pane_focus(None, None), None);
    }

    // --- optimistic banner state (`awaiting_call`/`next_answered_call`) --

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

    // --- banner substance (`describe_pending_call`) ------------------------

    fn request(tool_id: &str, input: serde_json::Value) -> ToolCallRequest {
        ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: tool_id.to_string(),
            input,
        }
    }

    #[test]
    fn describe_pending_call_shows_the_bash_command_line() {
        let request = request("bash", serde_json::json!({ "command": "cargo test" }));
        assert_eq!(describe_pending_call(&request), "cargo test");
    }

    #[test]
    fn describe_pending_call_shows_the_fs_write_target_path() {
        let request = request(
            "fs.write",
            serde_json::json!({ "path": "/tmp/example.rs", "content": "fn main() {}" }),
        );
        assert_eq!(describe_pending_call(&request), "/tmp/example.rs");
    }

    #[test]
    fn describe_pending_call_shows_the_fs_edit_target_path() {
        let request = request("fs.edit", serde_json::json!({ "path": "/tmp/example.rs" }));
        assert_eq!(describe_pending_call(&request), "/tmp/example.rs");
    }

    #[test]
    fn describe_pending_call_falls_back_to_the_tool_id_for_unknown_tools() {
        let request = request("mock.approval_required", serde_json::json!({}));
        assert_eq!(describe_pending_call(&request), "mock.approval_required");
    }

    #[test]
    fn describe_pending_call_truncates_a_long_bash_command_by_char_count() {
        let long = "a".repeat(MAX_SUMMARY_CHARS + 50);
        let request = request("bash", serde_json::json!({ "command": long }));
        let summary = describe_pending_call(&request);
        assert!(summary.ends_with('…'));
        assert!(summary.chars().count() <= MAX_SUMMARY_CHARS + 1);
    }

    #[test]
    fn describe_pending_call_truncates_a_multi_line_bash_command_by_line_count() {
        let command = "line1\nline2\nline3\nline4\nline5".to_string();
        let request = request("bash", serde_json::json!({ "command": command }));
        let summary = describe_pending_call(&request);
        assert_eq!(summary.lines().count(), MAX_SUMMARY_LINES);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn describe_pending_call_leaves_a_short_bash_command_untouched() {
        let request = request("bash", serde_json::json!({ "command": "ls -la" }));
        assert_eq!(describe_pending_call(&request), "ls -la");
    }
}
