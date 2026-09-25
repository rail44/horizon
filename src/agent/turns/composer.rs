//! Composer state derived from the pending-approval queue: the keyboard
//! approval target, the placeholder text, and the model chip (the
//! provider→model picker's entry point as of the 2026-09-19 model-switcher
//! addendum to `docs/agent-output-ui-amendment.md`). `latest_turn_model`
//! (the model chip's other input) moved to
//! `horizon_agent::transcript` -- it's plain model-id extraction, not
//! wording -- and is re-exported from `super` under its original name
//! (see `turns/mod.rs`'s doc comment).

use horizon_agent::contract::ToolCallIdentity;
use horizon_agent::wire::ModelSelection;

/// The approval keyboard-capture state (`docs/agent-output-ui-
/// amendment.md` decision 4, stage E; re-scoped to row-centric v2):
/// `Normal`, or targeting one specific
/// pending call for the keyboard path. Its *rendering* surface is no
/// longer a composer transformation -- stage E's banner is gone -- it's
/// now a compact "⏎ approve · esc deny" annotation on that call's own
/// row (`view::render_tool_call_row`, gated by
/// [`is_keyboard_approval_target`]). The keyboard semantics themselves
/// are unchanged: while this holds `Approval { identity }` and the
/// composer is empty/not typing, Enter approves and Esc denies that
/// exact call; typing past it reverts to `Normal` (`next_composer_mode`'s
/// no-flap rule, below). Kept as an explicit enum -- rather than folding
/// "is approval showing" into a bool alongside a separately tracked
/// identity -- so the amendment's own recorded future direction
/// (prompt-intent auto-approval, "auto mode") has a clean third arm to
/// add later: skip or auto-resolve this state without touching the row's
/// other paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerMode {
    Normal,
    Approval { identity: ToolCallIdentity },
}

/// Recomputes [`ComposerMode`] from the session's actionable pending
/// queue (oldest-first -- the same ordering
/// `horizon_agent::frame::actionable_pending_approval_identities_in`
/// returns, ghost-excluded per the round-4 post-review fix) and
/// `dismissed`: the identity, if any, the composer most recently reverted
/// to `Normal` for because the user started typing instead of deciding.
///
/// No-flap rule (stage E): typing past a shown approval dismisses
/// *that exact identity*, not "approval mode" in general. The composer
/// only shows `Approval` again once the queue's head actually changes --
/// either this call resolves via any of the other three paths (row
/// button, palette, CLI) and a different one takes its place, or the
/// queue was empty and gains its first entry. A queue whose head is
/// still the dismissed identity keeps returning `Normal` here on every
/// call, however many times it's asked (e.g. once per keystroke) --
/// nothing about typing further, or deleting back to an empty composer,
/// flips it back. An empty queue always clears any dismissal along with
/// it, since there's nothing left to have dismissed.
pub(crate) fn next_composer_mode(
    actionable_queue: &[ToolCallIdentity],
    dismissed: Option<&ToolCallIdentity>,
) -> ComposerMode {
    match actionable_queue.first() {
        None => ComposerMode::Normal,
        Some(identity) if Some(identity) == dismissed => ComposerMode::Normal,
        Some(identity) => ComposerMode::Approval {
            identity: identity.clone(),
        },
    }
}

/// Whether `identity` is the exact call [`ComposerMode`] currently targets
/// for the keyboard path (row-centric v2):
/// decides which single `Waiting` row, if any, shows the "⏎ approve · esc
/// deny" annotation next to its Approve/Deny buttons. Derived purely from
/// the mode -- never from queue position -- so the hint can never lie:
/// once typing dismisses the mode back to `Normal`
/// (`next_composer_mode`'s no-flap rule), this returns `false` for every
/// identity, including the one just shown, so the annotation disappears
/// exactly when the keys it describes stop doing anything.
pub(crate) fn is_keyboard_approval_target(
    mode: &ComposerMode,
    identity: &ToolCallIdentity,
) -> bool {
    matches!(mode, ComposerMode::Approval { identity: target } if target == identity)
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

/// The composer's model chip (mock's `claude-sonnet-4` pill) — read-only
/// display until the 2026-09-19 model-switcher addendum
/// (`docs/agent-output-ui-amendment.md`) made the composer's rendered chip
/// the picker's clickable entry point; this function stays the pure
/// label/precedence computation either way,
/// combining the session's resolved model
/// (`agent::session::AgentSession::model`, known from session start/attach
/// -- see `docs/agent-output-ui-amendment.md`'s dated model-chip addendum,
/// which closed the "no session-start signal" gap
/// [`super::latest_turn_model`]'s own doc comment used to describe) with
/// the latest completed turn's own model ([`super::latest_turn_model`]).
///
/// **Precedence** (reversing the original rule):
/// `session_model` wins on disagreement. It used to be the steady-state
/// value resolved once at session start, with `turn_model` overriding on
/// divergence because "the latest completed turn is always closer to what
/// would happen if you sent a message right now" -- but that rationale
/// explicitly assumed *there is no model switcher yet*. One exists now
/// (parent task #1's Phase 2): every mid-session change flows through the
/// explicit `set_session_model` RPC, and the daemon re-announces the
/// resolved model (`AgentWireEvent::SessionModel`) on every switch, so the
/// session value is no longer a possibly-stale startup snapshot -- it is
/// the freshest intent signal there is, arriving ahead of any turn that
/// could reflect it. The drift protection the old rule provided is now the
/// echo mechanism's job. A turn that ran on the previous model is one
/// switch behind by construction, so letting it mask the switch would show
/// a stale model through the exact window the switcher exists for (the
/// seconds-to-minutes before the next `TurnEnded` folds). Falls back to
/// whichever one is `Some` if the other is `None`; `None` only when neither
/// is known (the composer renders that as the `Model…` placeholder, which
/// doubles as the picker's entry point).
pub(crate) fn composer_model_chip<'a>(
    session_model: Option<&'a str>,
    turn_model: Option<&'a str>,
) -> Option<&'a str> {
    match (session_model, turn_model) {
        (Some(session), Some(turn)) if session != turn => Some(session),
        (Some(session), _) => Some(session),
        (None, turn) => turn,
    }
}

/// The chip's display label, the selection-aware wrapper over
/// [`composer_model_chip`]: when the session's last applied selection is
/// known it reads `provider · model` (so a MoA session shows `moa · mix`
/// instead of the aggregator's resolved model id), otherwise it falls back
/// to the resolved/turn model id unchanged.
pub(crate) fn composer_model_label(
    selection: Option<&ModelSelection>,
    session_model: Option<&str>,
    turn_model: Option<&str>,
) -> Option<String> {
    if let Some(selection) = selection {
        return Some(format!("{} · {}", selection.provider, selection.model));
    }
    composer_model_chip(session_model, turn_model).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    fn approval_identity(id: &str) -> ToolCallIdentity {
        ToolCallIdentity {
            call_id: horizon_agent::contract::ToolCallId(id.into()),
            occurrence_id: horizon_agent::contract::OccurrenceId(id.into()),
        }
    }

    #[test]
    fn composer_placeholder_names_next_turn_delivery_while_a_turn_is_in_flight() {
        assert_eq!(composer_placeholder(false), "Message the agent…");
        let in_flight = composer_placeholder(true);
        assert!(in_flight.starts_with("Message the agent"));
        assert!(in_flight.contains("next turn"));
    }

    #[test]
    fn composer_model_chip_shows_the_session_model_before_any_turn_completes() {
        // The gap `horizon_agent::transcript::grouping::tests::
        // latest_turn_model_is_none_before_any_turn_completes` exercises
        // (`latest_turn_model` moved there, see this module's doc
        // comment): with a session-start model now known, the chip no
        // longer has to wait for the first turn to complete.
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
    fn composer_model_chip_prefers_the_session_model_when_the_turn_diverges() {
        // Reversing the original turn-wins rule: with the model switcher
        // live, every mid-session change is
        // an explicit `set_session_model` echoed back as a `SessionModel`
        // re-announcement, so the session value IS "what would happen if
        // you sent a message right now" and the latest completed turn is
        // one switch behind it by construction.
        assert_eq!(
            composer_model_chip(Some("claude-sonnet-4"), Some("gpt-5")),
            Some("claude-sonnet-4")
        );
    }

    #[test]
    fn composer_model_chip_falls_back_to_the_turn_model_when_the_session_model_is_unknown() {
        // e.g. a role-less session, or a provider with no resolvable model
        // (`registry::Provider::resolved_model`'s doc comment) -- the latest
        // completed turn is still the best available value.
        assert_eq!(composer_model_chip(None, Some("gpt-5")), Some("gpt-5"));
    }

    #[test]
    fn composer_model_chip_is_none_when_neither_is_known() {
        assert_eq!(composer_model_chip(None, None), None);
    }

    #[test]
    fn composer_model_label_shows_the_selection_pair_when_known() {
        // A MoA switch: the chip reads `moa · mix`, not the aggregator's
        // resolved model id the `SessionModel` announcement carries.
        let selection = ModelSelection {
            provider: "moa".to_string(),
            model: "mix".to_string(),
        };
        assert_eq!(
            composer_model_label(Some(&selection), Some("hf:deepseek-ai/x"), None),
            Some("moa · mix".to_string())
        );
    }

    #[test]
    fn composer_model_label_falls_back_to_resolved_or_turn_model_without_a_selection() {
        assert_eq!(
            composer_model_label(None, Some("gpt-5"), None),
            Some("gpt-5".to_string())
        );
        assert_eq!(
            composer_model_label(None, None, Some("gpt-5")),
            Some("gpt-5".to_string())
        );
        assert_eq!(composer_model_label(None, None, None), None);
    }

    #[test]
    fn next_composer_mode_is_normal_for_an_empty_queue() {
        assert_eq!(next_composer_mode(&[], None), ComposerMode::Normal);
    }

    #[test]
    fn next_composer_mode_shows_the_oldest_actionable_call() {
        let queue = vec![approval_identity("a"), approval_identity("b")];
        assert_eq!(
            next_composer_mode(&queue, None),
            ComposerMode::Approval {
                identity: approval_identity("a")
            }
        );
    }

    #[test]
    fn next_composer_mode_stays_normal_while_the_dismissed_call_is_still_the_head() {
        // The no-flap rule: typing past the shown approval dismisses that
        // exact identity, and it keeps reporting `Normal` for that same
        // head on every subsequent call (e.g. once per keystroke) --
        // never re-showing the approval state underneath what the user is
        // typing.
        let queue = vec![approval_identity("a")];
        assert_eq!(
            next_composer_mode(&queue, Some(&approval_identity("a"))),
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
        let queue = vec![approval_identity("b")];
        assert_eq!(
            next_composer_mode(&queue, Some(&approval_identity("a"))),
            ComposerMode::Approval {
                identity: approval_identity("b")
            }
        );
    }

    #[test]
    fn next_composer_mode_clears_once_the_queue_empties() {
        // A stale dismissal for a call that has since left the queue
        // entirely (every pending approval resolved) doesn't matter --
        // an empty queue is always `Normal`.
        assert_eq!(
            next_composer_mode(&[], Some(&approval_identity("a"))),
            ComposerMode::Normal
        );
    }

    #[test]
    fn approving_a_bash_call_advances_composer_mode_the_instant_started_folds() {
        // End-to-end through the real seam `AgentView::sync_composer_mode`
        // uses (`horizon_agent::frame::actionable_pending_approval_identities_in`
        // feeding `next_composer_mode`): approving targets the oldest
        // actionable call; the daemon's synchronous ack for that click
        // folds `ToolCallStarted` immediately, well before `bash`'s
        // eventual `ToolCallFinished` -- the composer must advance to the
        // next actionable call right there, not wait for the result.
        let before = vec![approval_requested("a"), approval_requested("b")];
        let queue_before = horizon_agent::frame::actionable_pending_approval_identities_in(&before);
        assert_eq!(
            next_composer_mode(&queue_before, None),
            ComposerMode::Approval {
                identity: approval_identity("a")
            }
        );

        let after = vec![
            approval_requested("a"),
            approval_requested("b"),
            tool_started("a"),
        ];
        let queue_after = horizon_agent::frame::actionable_pending_approval_identities_in(&after);
        assert_eq!(
            next_composer_mode(&queue_after, None),
            ComposerMode::Approval {
                identity: approval_identity("b")
            }
        );
    }

    #[test]
    fn approving_the_only_pending_call_clears_composer_mode_once_started_folds() {
        let items = vec![approval_requested("a"), tool_started("a")];
        let queue = horizon_agent::frame::actionable_pending_approval_identities_in(&items);
        assert_eq!(next_composer_mode(&queue, None), ComposerMode::Normal);
    }

    #[test]
    fn is_keyboard_approval_target_true_only_for_the_modes_own_call() {
        let a = approval_identity("a");
        let b = approval_identity("b");
        let mode = ComposerMode::Approval {
            identity: a.clone(),
        };
        assert!(is_keyboard_approval_target(&mode, &a));
        assert!(!is_keyboard_approval_target(&mode, &b));
    }

    #[test]
    fn is_keyboard_approval_target_is_false_while_normal() {
        // Dismissed-by-typing (or never-pending) both collapse to
        // `Normal`, which targets no call at all -- the annotation must
        // vanish from whatever row last showed it.
        let a = approval_identity("a");
        assert!(!is_keyboard_approval_target(&ComposerMode::Normal, &a));
    }

    #[test]
    fn dismissing_an_old_occurrence_does_not_hide_a_retry_with_the_same_call_id() {
        let first = approval_identity("reused");
        let mut retry = first.clone();
        retry.occurrence_id = horizon_agent::contract::OccurrenceId("retry".into());
        let mode = next_composer_mode(std::slice::from_ref(&retry), Some(&first));
        assert_eq!(
            mode,
            ComposerMode::Approval {
                identity: retry.clone()
            }
        );
        assert!(!is_keyboard_approval_target(&mode, &first));
        assert!(is_keyboard_approval_target(&mode, &retry));
    }
}
