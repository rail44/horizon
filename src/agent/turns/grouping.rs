//! Turn grouping: slicing an `AgentFrame`'s flat item list into per-turn
//! spans (`docs/agent-output-ui-amendment.md` stage C, decisions 1-2).

use std::time::Duration;

use horizon_agent::contract::{Message, MessageRole, TurnEndReason};
use horizon_agent::frame::AgentFrameItem;

/// One turn's items, sliced from `AgentFrame::items` by index range
/// `[start, end)`. `ended` is `None` for the turn currently in
/// progress -- the last span produced by [`group_into_turns`], and only
/// meaningful to render as such while the session state indicates a turn
/// is in flight (`horizon_agent::frame::state_indicates_turn_in_flight`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnSpan {
    pub start: usize,
    pub end: usize,
    pub ended: Option<TurnEnd>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnEnd {
    pub reason: TurnEndReason,
    pub model: Option<String>,
    pub elapsed: Duration,
}

/// Groups `items` into turn segments: a segment opens at the first item
/// seen while no segment is currently open (whatever its type -- see the
/// second invariant note below), and closes at the next `TurnEnded`
/// (inclusive). A trailing segment with no closing `TurnEnded` yet is the
/// turn in progress.
///
/// **Invariant 1 (root-caused 2026-07-13 from a real, reproduced event
/// sequence -- see `docs/agent-output-ui-amendment.md`'s post-review
/// note): every span partitions the item list -- no item ever falls
/// outside every span.** This used to not hold: a user `Message`
/// unconditionally opened a *new* segment even while the previous one
/// hadn't seen a `TurnEnded` yet, closing the stale one with `ended:
/// None` on the theory that this "shouldn't happen by contract". It does
/// happen, routinely -- sending from the composer is deliberately
/// next-turn delivery even while a turn is mid-flight
/// (`docs/agent-output-ui-amendment.md` decision 6), and a user *will*
/// type another message while an earlier tool call's approval is still
/// pending (e.g. nudging a turn that looks stuck, or retrying an
/// approval that didn't seem to take effect). Each such interjection
/// used to mint a fresh span and stale the previous one forever (it can
/// never retroactively gain a `TurnEnded`) -- and once the session's
/// state eventually left the in-flight set (a cancel, or the turn
/// finally settling), the view's fallback for a dangling `ended: None`
/// span while not in flight was to render every item in it
/// *individually*, raw (`render_item`'s per-type fallback: unprocessed
/// JSON `tool`/`tool result` blocks, `Debug`-formatted `tool
/// (preparing)`, standalone approval boxes with no visible link to their
/// row) -- exactly the "incomprehensible screen state" a real session
/// hit after several rapid interjections while one bash approval sat
/// unresolved. The fix: a mid-turn interjection no longer opens a new
/// segment at all -- while a segment is already open, a further user
/// `Message` is just one more item *within* it (rendered as its own
/// message block inside the running card/receipt, via the existing
/// per-item loop in `AgentView::render_turn` -- no separate "interjection
/// row" needed for this). The segment stays open, however many messages
/// land in it, until an actual `TurnEnded` closes it -- or, if none ever
/// arrives, it's the trailing in-progress span, same as always.
///
/// **Invariant 2 (broadened 2026-07-13, same investigation): opening a
/// segment never requires a user `Message` specifically -- any item can
/// open one, as long as none is currently open.** A resumed session, or
/// a provider continuation that follows a daemon-synthesized `TurnEnded`
/// (`resume_persisted_sessions` on a `horizon-sessiond` respawn mid-turn,
/// see `docs/agent-output-ui-amendment.md`'s round-4 finding) can produce
/// tool activity or assistant text with no user `Message` immediately
/// preceding it in the frame's own item window. Requiring a `Message` to
/// open a segment left exactly this kind of item sequence permanently
/// outside every span, hitting the same raw per-item fallback invariant
/// 1 just fixed. Opening on any item closes that structural gap: the
/// implicit segment renders as the running card while a turn is
/// genuinely still in flight, and closes normally the next time a
/// `TurnEnded` arrives, same as any other span.
///
/// Note this is about *grouping* only. A separate, real production
/// sequence (session `3fe93cdb-...`, "Agent #30",
/// `hf:moonshotai/Kimi-K2.7-Code`, 2026-07-13 -- reproduced in
/// `a_batch_of_concurrent_tool_calls_with_two_overlapping_approvals_stays_one_open_span`
/// below) proved grouping alone isn't enough to guarantee the running
/// card renders: the daemon's own live `SessionState` can read a
/// non-in-flight value (`WaitingForUser`) for an extended real span of
/// time (36s in the captured log) while a batch of concurrent tool calls
/// is still resolving and a *sibling* approval is still pending --  well
/// before the span's own `TurnEnded` arrives. `AgentView::render`'s
/// per-span dispatch used to gate a dangling span's rendering vocabulary
/// on that live state reading in addition to `ended.is_none()`; it no
/// longer does -- a dangling span (by these two invariants, always the
/// turn genuinely still in progress) always renders through
/// `AgentView::render_turn`, never the flat per-item fallback, regardless
/// of what the live session state happens to read at render time.
pub(crate) fn group_into_turns(items: &[AgentFrameItem]) -> Vec<TurnSpan> {
    let mut spans = Vec::new();
    let mut current_start: Option<usize> = None;
    for (index, item) in items.iter().enumerate() {
        if current_start.is_none() {
            current_start = Some(index);
        }
        if let AgentFrameItem::TurnEnded {
            reason,
            model,
            elapsed,
        } = item
        {
            let start = current_start.take().unwrap_or(index);
            spans.push(TurnSpan {
                start,
                end: index + 1,
                ended: Some(TurnEnd {
                    reason: *reason,
                    model: model.clone(),
                    elapsed: *elapsed,
                }),
            });
        }
    }
    if let Some(start) = current_start {
        spans.push(TurnSpan {
            start,
            end: items.len(),
            ended: None,
        });
    }
    spans
}

/// Whether `items` contains at least one user message -- used to resolve
/// which rendered transcript block (`AgentView::render`'s `blocks`, one
/// element per turn span, plus the rare orphan-item fallback) the
/// "jump to latest user message" pill (`docs/agent-output-ui-design.md`
/// decision 7) should target. `ScrollHandle::scroll_to_top_of_item` only
/// anchors to a *direct child* of the tracked scroll container -- a whole
/// turn's rendered block, not a single message -- so `AgentView` tracks
/// the latest block containing a user message as it walks `items`,
/// calling this once per span (see `view.rs`'s `jump_to_latest_user_
/// message` doc comment for the full trade-off this approximates).
pub(crate) fn contains_user_message(items: &[AgentFrameItem]) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            AgentFrameItem::Message(Message {
                role: MessageRole::User,
                ..
            })
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_support::*;
    use super::*;

    #[test]
    fn groups_a_completed_turn_followed_by_a_running_one() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested(
                "a",
                "fs.grep",
                json!({"base_path": ".", "pattern": "notify"}),
            ),
            tool_finished("a", json!({"returned_count": 1})),
            assistant_message("fixed it"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 38),
            user_message("check the other form too"),
            tool_requested("b", "fs.read", json!({"path": "signup_form.rs"})),
        ];

        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 2);

        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 5); // inclusive of TurnEnded
        let ended = spans[0].ended.as_ref().expect("first turn settled");
        assert_eq!(ended.reason, TurnEndReason::Completed);
        assert_eq!(ended.model.as_deref(), Some("gpt-5"));
        assert_eq!(ended.elapsed, Duration::from_secs(38));

        assert_eq!(spans[1].start, 5);
        assert_eq!(spans[1].end, 7);
        assert!(spans[1].ended.is_none());
    }

    #[test]
    fn a_turn_with_no_tool_calls_still_groups_and_has_no_chips() {
        let items = vec![
            user_message("hello"),
            assistant_message("hi"),
            turn_ended(TurnEndReason::Completed, None, 2),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert!(span.ended.is_some());
        assert!(super::super::build_tool_call_views(&items[span.start..span.end]).is_empty());
    }

    #[test]
    fn a_second_user_message_with_no_turn_ended_between_them_merges_into_one_open_span() {
        // Root-caused 2026-07-13: a mid-turn interjection (the user
        // typing again before the previous turn closed) must not orphan
        // the first message into a permanently-dangling span -- it's
        // just one more item inside the still-open one.
        let items = vec![user_message("first"), user_message("second")];
        let spans = group_into_turns(&items);
        assert_eq!(
            spans,
            vec![TurnSpan {
                start: 0,
                end: 2,
                ended: None,
            }]
        );
    }

    #[test]
    fn a_mid_turn_interjection_while_an_approval_is_pending_stays_in_the_same_open_span() {
        // Reproduces the real event sequence behind the owner's
        // 2026-07-13 "partial approve leads to an incomprehensible
        // screen state" report: the user sent a message while an earlier
        // bash call's approval was still unresolved, the model retried
        // the same bash call (a *second* unresolved approval), and the
        // user interjected again -- and again. Multiple interjections
        // must not fragment this into several dangling spans.
        let items = vec![
            user_message("一旦このMVPでよいです。"),
            tool_requested("a", "bash", json!({"command": "cargo build"})),
            approval_requested("a"),
            // "a" is never resolved -- the user, unable to tell whether
            // approving it worked, interjects instead of waiting.
            user_message("a"),
            tool_requested("b", "bash", json!({"command": "cargo build"})),
            approval_requested("b"),
            user_message("なんかapprove出来ないな"),
            tool_requested("c", "bash", json!({"command": "cargo build"})),
            approval_requested("c"),
            user_message("だから出来ないって言ってるでしょ"),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(
            spans,
            vec![TurnSpan {
                start: 0,
                end: items.len(),
                ended: None,
            }]
        );
    }

    #[test]
    fn the_interjection_span_closes_normally_once_a_turn_ended_finally_arrives() {
        // Continuing the same reproduction: eventually the turn is
        // cancelled, which finally closes the whole merged span -- every
        // interjection and its tool calls fold into one receipt, not
        // several dangling ones.
        let items = vec![
            user_message("一旦このMVPでよいです。"),
            tool_requested("a", "bash", json!({"command": "cargo build"})),
            approval_requested("a"),
            user_message("a"),
            tool_requested("b", "bash", json!({"command": "cargo build"})),
            approval_requested("b"),
            turn_ended(TurnEndReason::Cancelled, None, 42),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, items.len());
        let ended = spans[0].ended.as_ref().expect("closed by the TurnEnded");
        assert_eq!(ended.reason, TurnEndReason::Cancelled);
    }

    #[test]
    fn a_batch_of_concurrent_tool_calls_with_two_overlapping_approvals_stays_one_open_span() {
        // Reproduces the real event sequence behind the owner's
        // 2026-07-13 "approving the FORMER of two pending approvals
        // breaks the layout as attached" report (session
        // `3fe93cdb-3119-409d-8da7-b4c53c0883bf`, pane title "Agent #30",
        // `hf:moonshotai/Kimi-K2.7-Code`, reconstructed from
        // `~/.local/share/horizon/agent-events.jsonl`). The model issued
        // a batch of tool calls within one turn: a snapshot and several
        // `fs.read`s that never need approval, interleaved with three
        // `bash` calls that do -- the last two (`bash:7`/`bash:8`)
        // requested back-to-back before either resolved, exactly the
        // "two approvals showing" moment from the screenshot. The
        // daemon's own `SessionState` read `WaitingForUser` for a real
        // 36-second span between resolving `bash:7`'s approval and
        // starting `bash:8`'s -- `state_indicates_turn_in_flight` is
        // false for `WaitingForUser` -- but the *item* sequence itself
        // never gets a `TurnEnded` until everything settles. This
        // confirms grouping was never the bug for this case: it already
        // produces one continuous open span throughout, exactly as
        // asserted below. The actual root cause was
        // `AgentView::render`'s per-span dispatch additionally gating a
        // dangling span's rendering vocabulary on that live state
        // reading -- see `group_into_turns`'s invariant 2 note and
        // `AgentView::render`'s span walk, which no longer does that.
        let items = vec![
            user_message("このリポジトリの内容を把握してください"),
            tool_requested("workspace.snapshot:0", "workspace.snapshot", json!({})),
            tool_finished("workspace.snapshot:0", json!({"tab_count": 2})),
            tool_requested("bash:1", "bash", json!({"command": "ls -la"})),
            approval_requested("bash:1"),
            // Auto-approved siblings proceed immediately even while
            // `bash:1`'s approval is still unresolved -- the runtime
            // doesn't block the whole batch on one pending decision.
            tool_requested("fs.read:2", "fs.read", json!({"path": "README.md"})),
            tool_finished("fs.read:2", json!({"total_lines": 107})),
            tool_requested("fs.read:3", "fs.read", json!({"path": "Cargo.toml"})),
            tool_finished("fs.read:3", json!({"total_lines": 74})),
            tool_finished("bash:1", json!({"exit_code": 0})),
            tool_requested("bash:6", "bash", json!({"command": "ls src"})),
            approval_requested("bash:6"),
            tool_finished("bash:6", json!({"exit_code": 0})),
            tool_requested("fs.read:4", "fs.read", json!({"path": "docs/roadmap.md"})),
            tool_finished("fs.read:4", json!({"total_lines": 50})),
            tool_requested("fs.read:5", "fs.read", json!({"path": "AGENTS.md"})),
            tool_finished("fs.read:5", json!({"total_lines": 200})),
            // The two overlapping approvals: both requested before
            // either resolves.
            tool_requested("bash:7", "bash", json!({"command": "find . -maxdepth 2"})),
            approval_requested("bash:7"),
            tool_requested("bash:8", "bash", json!({"command": "cargo metadata"})),
            approval_requested("bash:8"),
            // The owner approves the FORMER (`bash:7`) first -- `bash:8`
            // stays pending for a long real-world gap (36s in the actual
            // log, invisible to grouping since it operates on items, not
            // timestamps) before it, too, resolves.
            tool_finished("bash:7", json!({"exit_code": 0})),
            tool_finished("bash:8", json!({"exit_code": 0})),
            assistant_message("Here's what I found in the repository..."),
            turn_ended(
                TurnEndReason::Completed,
                Some("hf:moonshotai/Kimi-K2.7-Code"),
                54,
            ),
        ];

        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, items.len());
        let ended = spans[0].ended.as_ref().expect("closed by the TurnEnded");
        assert_eq!(ended.reason, TurnEndReason::Completed);
    }

    #[test]
    fn a_turn_opening_item_that_is_not_a_user_message_still_opens_a_span() {
        // Invariant 2 (broadened 2026-07-13): a structural gap -- e.g. a
        // provider continuation following a daemon-synthesized
        // `TurnEnded` on a `horizon-sessiond` respawn mid-turn
        // (`docs/agent-output-ui-amendment.md`'s round-4 finding) -- can
        // leave tool activity or assistant text with no user `Message`
        // immediately preceding it in the frame's own item window.
        // Before this fix, only a user `Message` could open a segment,
        // so this item sequence fell entirely outside every span and hit
        // the raw per-item fallback despite being ordinary tool
        // activity.
        let items = vec![
            tool_requested("a", "fs.read", json!({"path": "README.md"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_message("done"),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(
            spans,
            vec![TurnSpan {
                start: 0,
                end: items.len(),
                ended: None,
            }]
        );
    }

    #[test]
    fn contains_user_message_finds_a_user_message_among_other_items() {
        let items = vec![
            assistant_message("hi"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            user_message("fix the bug"),
        ];
        assert!(contains_user_message(&items));
    }

    #[test]
    fn contains_user_message_false_without_one() {
        let items = vec![
            assistant_message("hi"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
        ];
        assert!(!contains_user_message(&items));
    }
}
