//! Composer state derived from the pending-approval queue: the keyboard
//! approval target, the placeholder text, and the read-only model chip.

use horizon_agent::contract::ToolCallId;
use horizon_agent::frame::AgentFrameItem;

/// The approval keyboard-capture state (`docs/agent-output-ui-
/// amendment.md` decision 4, stage E; re-scoped to row-centric v2 by
/// owner decision 2026-07-13): `Normal`, or targeting one specific
/// pending call for the keyboard path. Its *rendering* surface is no
/// longer a composer transformation -- stage E's banner is gone -- it's
/// now a compact "⏎ approve · esc deny" annotation on that call's own
/// row (`view::render_tool_call_row`, gated by
/// [`is_keyboard_approval_target`]). The keyboard semantics themselves
/// are unchanged: while this holds `Approval { call_id }` and the
/// composer is empty/not typing, Enter approves and Esc denies that
/// exact call; typing past it reverts to `Normal` (`next_composer_mode`'s
/// no-flap rule, below). Kept as an explicit enum -- rather than folding
/// "is approval showing" into a bool alongside a separately tracked
/// call_id -- so the amendment's own recorded future direction
/// (prompt-intent auto-approval, "auto mode") has a clean third arm to
/// add later: skip or auto-resolve this state without touching the row's
/// other paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerMode {
    Normal,
    Approval { call_id: ToolCallId },
}

/// Recomputes [`ComposerMode`] from the session's actionable pending
/// queue (oldest-first -- the same ordering
/// `horizon_agent::frame::actionable_pending_approval_call_ids_in`
/// returns, ghost-excluded per the round-4 post-review fix) and
/// `dismissed`: the call_id, if any, the composer most recently reverted
/// to `Normal` for because the user started typing instead of deciding.
///
/// No-flap rule (stage E): typing past a shown approval dismisses
/// *that exact call_id*, not "approval mode" in general. The composer
/// only shows `Approval` again once the queue's head actually changes --
/// either this call resolves via any of the other three paths (row
/// button, palette, CLI) and a different one takes its place, or the
/// queue was empty and gains its first entry. A queue whose head is
/// still the dismissed call_id keeps returning `Normal` here on every
/// call, however many times it's asked (e.g. once per keystroke) --
/// nothing about typing further, or deleting back to an empty composer,
/// flips it back. An empty queue always clears any dismissal along with
/// it, since there's nothing left to have dismissed.
pub(crate) fn next_composer_mode(
    actionable_queue: &[ToolCallId],
    dismissed: Option<&ToolCallId>,
) -> ComposerMode {
    match actionable_queue.first() {
        None => ComposerMode::Normal,
        Some(call_id) if Some(call_id) == dismissed => ComposerMode::Normal,
        Some(call_id) => ComposerMode::Approval {
            call_id: call_id.clone(),
        },
    }
}

/// Whether `call_id` is the exact call [`ComposerMode`] currently targets
/// for the keyboard path (row-centric v2, owner decision 2026-07-13):
/// decides which single `Waiting` row, if any, shows the "⏎ approve · esc
/// deny" annotation next to its Approve/Deny buttons. Derived purely from
/// the mode -- never from queue position -- so the hint can never lie:
/// once typing dismisses the mode back to `Normal`
/// (`next_composer_mode`'s no-flap rule), this returns `false` for every
/// call_id, including the one just shown, so the annotation disappears
/// exactly when the keys it describes stop doing anything.
pub(crate) fn is_keyboard_approval_target(mode: &ComposerMode, call_id: &ToolCallId) -> bool {
    matches!(mode, ComposerMode::Approval { call_id: target } if target == call_id)
}

/// The composer's placeholder text (decision 6): sending from the composer
/// is always next-turn delivery, even while a turn is running (interjecting
/// into the live turn is 7b's unbuilt "steering" idea, not today's
/// behavior) -- the placeholder says so explicitly while a turn is in
/// flight, mirroring mock 7a's "続けて指示する…（送信は次のターン）".
pub(crate) fn composer_placeholder(turn_in_flight: bool) -> &'static str {
    if turn_in_flight {
        "Message the agent (sends as the next turn)…"
    } else {
        "Message the agent…"
    }
}

/// One of two inputs to the composer's model chip (see
/// [`composer_model_chip`]): the most recent `AgentFrameItem::TurnEnded`
/// that actually carries a model id, scanning `items` from the end. A
/// still-running turn's own `TurnEnded` hasn't folded yet, so it never
/// masks the previous turn's model; a completed turn that ended before any
/// provider request (`TurnEnded`'s own doc comment -- e.g. an immediate
/// cancel) is skipped in favor of an earlier turn's model, the "best
/// available value" rather than flickering the chip away. `None` until the
/// very first turn with a provider request completes.
pub(crate) fn latest_turn_model(items: &[AgentFrameItem]) -> Option<&str> {
    items.iter().rev().find_map(|item| match item {
        AgentFrameItem::TurnEnded {
            model: Some(model), ..
        } => Some(model.as_str()),
        _ => None,
    })
}

/// The composer's read-only model chip (mock's `claude-sonnet-4` pill),
/// combining the session's resolved model
/// (`agent::session::AgentSession::model`, known from session start/attach
/// -- see `docs/agent-output-ui-amendment.md`'s dated model-chip addendum,
/// which closed the "no session-start signal" gap [`latest_turn_model`]'s
/// own doc comment used to describe) with the latest completed turn's own
/// model ([`latest_turn_model`]).
///
/// **Precedence**: `session_model` is the steady-state source of truth --
/// resolved once, synchronously, before any turn ever runs. `turn_model`
/// overrides it only when the two actively disagree, since that can only
/// mean the session's *actual* provider has moved on from what was resolved
/// at session start (there is no model switcher yet -- deferred, unbuilt
/// future work -- so this can't happen today, but the precedence is decided
/// now rather than left implicit for whenever one lands): the latest
/// completed turn is always closer to "what would happen if you sent a
/// message right now" than a possibly-stale session-start value. Falls back
/// to whichever one is `Some` if the other is `None`; `None` (chip hidden)
/// only when neither is known.
pub(crate) fn composer_model_chip<'a>(
    session_model: Option<&'a str>,
    turn_model: Option<&'a str>,
) -> Option<&'a str> {
    match (session_model, turn_model) {
        (Some(session), Some(turn)) if session != turn => Some(turn),
        (Some(session), _) => Some(session),
        (None, turn) => turn,
    }
}

#[cfg(test)]
mod tests {
    use horizon_agent::contract::TurnEndReason;
    use serde_json::json;

    use super::super::test_support::*;
    use super::*;

    #[test]
    fn composer_placeholder_names_next_turn_delivery_while_a_turn_is_in_flight() {
        assert_eq!(composer_placeholder(false), "Message the agent…");
        let in_flight = composer_placeholder(true);
        assert!(in_flight.starts_with("Message the agent"));
        assert!(in_flight.contains("next turn"));
    }

    #[test]
    fn latest_turn_model_is_none_before_any_turn_completes() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
        ];
        assert_eq!(latest_turn_model(&items), None);
    }

    #[test]
    fn latest_turn_model_reads_the_most_recently_completed_turn() {
        let items = vec![
            user_message("fix the bug"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 10),
            user_message("check the other form too"),
            turn_ended(TurnEndReason::Completed, Some("claude-sonnet-4"), 20),
        ];
        assert_eq!(latest_turn_model(&items), Some("claude-sonnet-4"));
    }

    #[test]
    fn latest_turn_model_skips_a_running_turns_dangling_span() {
        let items = vec![
            user_message("fix the bug"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 10),
            user_message("one more thing"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
        ];
        // The second turn is still running (no closing `TurnEnded`), so its
        // model -- if any -- hasn't folded yet; the chip keeps showing the
        // last completed turn's model rather than going blank mid-turn.
        assert_eq!(latest_turn_model(&items), Some("gpt-5"));
    }

    #[test]
    fn latest_turn_model_falls_back_past_a_completed_turn_with_no_provider_request() {
        let items = vec![
            user_message("fix the bug"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 10),
            user_message("cancel immediately"),
            turn_ended(TurnEndReason::Cancelled, None, 0),
        ];
        // The most recent turn ended before any provider request (e.g. an
        // immediate cancel) and so carries no model -- the chip falls back
        // to the earlier turn's model rather than disappearing.
        assert_eq!(latest_turn_model(&items), Some("gpt-5"));
    }

    #[test]
    fn composer_model_chip_shows_the_session_model_before_any_turn_completes() {
        // The gap `latest_turn_model_is_none_before_any_turn_completes`
        // exercises above: with a session-start model now known, the chip
        // no longer has to wait for the first turn to complete.
        assert_eq!(composer_model_chip(Some("gpt-5"), None), Some("gpt-5"));
    }

    #[test]
    fn composer_model_chip_prefers_the_session_model_when_the_turn_model_agrees() {
        assert_eq!(
            composer_model_chip(Some("gpt-5"), Some("gpt-5")),
            Some("gpt-5")
        );
    }

    #[test]
    fn composer_model_chip_lets_a_diverging_turn_model_override_the_session_model() {
        // A future model switcher (unbuilt) could change what a session
        // actually runs mid-session -- the latest completed turn is closer
        // to "what would happen if you sent a message right now" than the
        // value resolved once at session start.
        assert_eq!(
            composer_model_chip(Some("gpt-5"), Some("claude-sonnet-4")),
            Some("claude-sonnet-4")
        );
    }

    #[test]
    fn composer_model_chip_falls_back_to_the_turn_model_when_the_session_model_is_unknown() {
        // e.g. a role-less session, or a provider with no resolvable model
        // (`contract::Provider::resolved_model`'s doc comment) -- the latest
        // completed turn is still the best available value.
        assert_eq!(composer_model_chip(None, Some("gpt-5")), Some("gpt-5"));
    }

    #[test]
    fn composer_model_chip_is_none_when_neither_is_known() {
        assert_eq!(composer_model_chip(None, None), None);
    }

    #[test]
    fn next_composer_mode_is_normal_for_an_empty_queue() {
        assert_eq!(next_composer_mode(&[], None), ComposerMode::Normal);
    }

    #[test]
    fn next_composer_mode_shows_the_oldest_actionable_call() {
        let queue = vec![ToolCallId("a".to_string()), ToolCallId("b".to_string())];
        assert_eq!(
            next_composer_mode(&queue, None),
            ComposerMode::Approval {
                call_id: ToolCallId("a".to_string())
            }
        );
    }

    #[test]
    fn next_composer_mode_stays_normal_while_the_dismissed_call_is_still_the_head() {
        // The no-flap rule: typing past the shown approval dismisses that
        // exact call_id, and it keeps reporting `Normal` for that same
        // head on every subsequent call (e.g. once per keystroke) --
        // never re-showing the approval state underneath what the user is
        // typing.
        let queue = vec![ToolCallId("a".to_string())];
        assert_eq!(
            next_composer_mode(&queue, Some(&ToolCallId("a".to_string()))),
            ComposerMode::Normal
        );
    }

    #[test]
    fn next_composer_mode_advances_once_the_dismissed_call_resolves() {
        // Decision 4's "smoothly advance": once the previously-dismissed
        // head resolves (row button/palette/CLI) and a different call
        // becomes the head, approval mode reappears for the new one --
        // the dismissal doesn't carry over to a call it was never shown
        // for.
        let queue = vec![ToolCallId("b".to_string())];
        assert_eq!(
            next_composer_mode(&queue, Some(&ToolCallId("a".to_string()))),
            ComposerMode::Approval {
                call_id: ToolCallId("b".to_string())
            }
        );
    }

    #[test]
    fn next_composer_mode_clears_once_the_queue_empties() {
        // A stale dismissal for a call that has since left the queue
        // entirely (every pending approval resolved) doesn't matter --
        // an empty queue is always `Normal`.
        assert_eq!(
            next_composer_mode(&[], Some(&ToolCallId("a".to_string()))),
            ComposerMode::Normal
        );
    }

    #[test]
    fn approving_a_bash_call_advances_composer_mode_the_instant_started_folds() {
        // End-to-end through the real seam `AgentView::sync_composer_mode`
        // uses (`horizon_agent::frame::actionable_pending_approval_call_ids_in`
        // feeding `next_composer_mode`): approving targets the oldest
        // actionable call; the daemon's synchronous ack for that click
        // folds `ToolCallStarted` immediately, well before `bash`'s
        // eventual `ToolCallFinished` -- the composer must advance to the
        // next actionable call right there, not wait for the result.
        let before = vec![approval_requested("a"), approval_requested("b")];
        let queue_before = horizon_agent::frame::actionable_pending_approval_call_ids_in(&before);
        assert_eq!(
            next_composer_mode(&queue_before, None),
            ComposerMode::Approval {
                call_id: ToolCallId("a".to_string())
            }
        );

        let after = vec![
            approval_requested("a"),
            approval_requested("b"),
            tool_started("a"),
        ];
        let queue_after = horizon_agent::frame::actionable_pending_approval_call_ids_in(&after);
        assert_eq!(
            next_composer_mode(&queue_after, None),
            ComposerMode::Approval {
                call_id: ToolCallId("b".to_string())
            }
        );
    }

    #[test]
    fn approving_the_only_pending_call_clears_composer_mode_once_started_folds() {
        let items = vec![approval_requested("a"), tool_started("a")];
        let queue = horizon_agent::frame::actionable_pending_approval_call_ids_in(&items);
        assert_eq!(next_composer_mode(&queue, None), ComposerMode::Normal);
    }

    #[test]
    fn is_keyboard_approval_target_true_only_for_the_modes_own_call() {
        let a = ToolCallId("a".to_string());
        let b = ToolCallId("b".to_string());
        let mode = ComposerMode::Approval { call_id: a.clone() };
        assert!(is_keyboard_approval_target(&mode, &a));
        assert!(!is_keyboard_approval_target(&mode, &b));
    }

    #[test]
    fn is_keyboard_approval_target_is_false_while_normal() {
        // Dismissed-by-typing (or never-pending) both collapse to
        // `Normal`, which targets no call at all -- the annotation must
        // vanish from whatever row last showed it.
        let a = ToolCallId("a".to_string());
        assert!(!is_keyboard_approval_target(&ComposerMode::Normal, &a));
    }
}
