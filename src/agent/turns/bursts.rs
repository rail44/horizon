//! Tool-activity bursts within a turn (round 5, monotone burst splitting)
//! and the reasoning-visibility decision that rides alongside them.

use horizon_agent::contract::{Message, MessageRole};
use horizon_agent::frame::AgentFrameItem;

use super::grouping::TurnEnd;
use super::tool_call::build_tool_call_views;

/// Whether `item` is part of a tool call's lifecycle -- used by
/// [`segment_bursts`] to find burst boundaries.
fn is_tool_related(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::ToolCallRequested(_)
            | AgentFrameItem::ToolCallStarted(_)
            | AgentFrameItem::ToolCallFinished(_)
            | AgentFrameItem::ApprovalRequested(_)
            | AgentFrameItem::ToolCallPreparing(_)
    )
}

/// Whether `item` is assistant-authored text -- a streaming delta or a
/// committed assistant `Message` -- used by [`segment_bursts`].
fn is_assistant_text(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::AssistantTextDelta(_)
            | AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
                ..
            })
    )
}

/// One tool burst within a turn: a maximal run of tool activity. Indices
/// are relative to the turn's own item slice (the same convention
/// [`build_tool_call_views`] uses), `[start, end)`.
///
/// Round 5 (owner decision 2026-07-13, "monotone burst splitting" --
/// superseding round 2's whole-turn provisional-receipt flip-back, see
/// `docs/agent-output-ui-amendment.md`'s post-review note): a turn can
/// fold into *more than one* receipt as it progresses -- tools run,
/// finish, the model answers, then decides to run more tools, answers
/// again, and so on. Each such run is its own burst, and a burst that
/// has closed (see [`segment_bursts`]) never reopens into a card again,
/// however much more the turn goes on to do -- eliminating the round-2
/// mechanism's "flips back to a card" bounce entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Burst {
    pub start: usize,
    pub end: usize,
    /// Whether this burst has permanently folded into a receipt.
    /// `false` only for the trailing burst of a still-running turn whose
    /// tool calls aren't all finished yet, or that has no closing
    /// assistant text after them yet -- the one burst still eligible to
    /// render as the running card. Every other burst -- including the
    /// turn's very last one, once `TurnEnded` folds -- is always `true`.
    pub closed: bool,
}

/// Segments `items` (a turn's own slice, `ended: None` or `Some` --
/// either way, from `group_into_turns`) into [`Burst`]s.
///
/// A burst opens at the first tool-related item found while none is
/// currently open, and keeps absorbing every further tool-related item
/// (however far apart, and whatever non-tool items -- an interjected
/// user message, a stray reasoning delta -- happen to fall between them)
/// until it **closes**: assistant text (a streaming delta or a committed
/// assistant `Message`) appears while every tool call opened so far in
/// the burst has finished, or the turn's own `TurnEnded` item is
/// reached. Closing is permanent -- `end` stops exactly at the last
/// absorbed tool-related item (the closing text itself is *not* part of
/// the burst; `AgentView::render_turn` renders it separately, right
/// after the burst's receipt) -- and a tool call arriving *after* that
/// closing text starts a brand new burst rather than reopening the
/// closed one. A user-message interjection never closes a burst (it
/// isn't assistant text); the burst just keeps growing through it, per
/// the same "next-turn delivery is deliberate mid-flight" reasoning
/// `group_into_turns` already documents.
///
/// A turn with no tool activity at all segments to an empty `Vec` --
/// nothing worth a receipt for; the text keeps rendering as plain
/// prose, exactly as it always has.
pub(crate) fn segment_bursts(items: &[AgentFrameItem]) -> Vec<Burst> {
    let mut bursts = Vec::new();
    let mut open: Option<(usize, usize)> = None; // (start, last_tool_index)

    for (index, item) in items.iter().enumerate() {
        if is_tool_related(item) {
            match &mut open {
                Some((_, last)) => *last = index,
                None => open = Some((index, index)),
            }
            continue;
        }
        if is_assistant_text(item) {
            if let Some((start, last)) = open {
                let all_finished = build_tool_call_views(&items[start..=last])
                    .iter()
                    .all(|call| call.finished);
                if all_finished {
                    bursts.push(Burst {
                        start,
                        end: last + 1,
                        closed: true,
                    });
                    open = None;
                }
                // Else: not closeable yet (a call opened in this burst
                // is still unfinished) -- this text isn't the closing
                // one, keep the burst open and scanning.
            }
            continue;
        }
        if matches!(item, AgentFrameItem::TurnEnded { .. }) {
            if let Some((start, last)) = open.take() {
                bursts.push(Burst {
                    start,
                    end: last + 1,
                    closed: true,
                });
            }
            // `TurnEnded` is always the turn's own last item
            // (`group_into_turns`'s invariant), so there's nothing left
            // to scan either way.
        }
        // Anything else (an interjected user `Message`, a
        // `ReasoningDelta`, `Error`, `Exited`, ...) never affects burst
        // boundaries.
    }

    if let Some((start, last)) = open {
        bursts.push(Burst {
            start,
            end: last + 1,
            closed: false,
        });
    }

    bursts
}

/// Whether a `ReasoningDelta` item outside every burst's own absorbed range
/// should render at all (owner requirement 2026-07-13: closing an
/// un-instructed deviation from base decision 5 -- thinking was completely
/// invisible while a turn ran, since `AgentView::render_turn`'s per-item
/// walk never had a match arm for it). A reasoning item that falls *inside*
/// a burst's `[start, end)` range (between two of its tool-related items,
/// "a stray reasoning delta" per [`segment_bursts`]'s own doc comment)
/// never reaches this decision at all -- it's structurally absorbed into
/// `burst_items` and dropped by `build_tool_call_views`, unaffected by this
/// fix, exactly as it always has been. For everything else (before the
/// first burst, between two bursts, after the last one, or a turn with no
/// bursts at all), visibility is simply "the turn hasn't ended yet":
/// decision 1's "thinking folds into the receipt on completion" applies
/// uniformly regardless of which of those positions a given item happened
/// to land in, so once `TurnEnded` folds, this goes back to invisible too --
/// no different from the burst-absorbed case's own fold.
pub(crate) fn thinking_visible_outside_burst(ended: Option<&TurnEnd>) -> bool {
    ended.is_none()
}

#[cfg(test)]
mod tests {
    use horizon_agent::contract::TurnEndReason;
    use serde_json::json;

    use super::super::test_support::*;
    use super::super::{aggregate_receipt, receipt_prose};
    use super::*;

    #[test]
    fn thinking_visible_outside_burst_only_while_the_turn_is_running() {
        assert!(thinking_visible_outside_burst(None));
        let end = TurnEnd {
            reason: TurnEndReason::Completed,
            model: None,
            elapsed: std::time::Duration::ZERO,
        };
        assert!(!thinking_visible_outside_burst(Some(&end)));
    }

    #[test]
    fn segment_bursts_never_lets_a_stray_reasoning_delta_split_a_burst() {
        // `segment_bursts`'s own doc comment calls out "a stray reasoning
        // delta" between two tool-related items of the same burst as
        // absorbed, not boundary-affecting -- pin it directly so the
        // burst-absorption half of this fix's design fork (thinking
        // structurally inside a burst's range stays invisible, unchanged)
        // has its own regression coverage.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            reasoning_delta("considering the second call…"),
            tool_requested("b", "fs.read", json!({"path": "b.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            tool_finished("b", json!({"total_lines": 5})),
            assistant_delta("Looking at both files, I"),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].start, 1);
        assert_eq!(bursts[0].end, 6);
        assert!(bursts[0].closed);
    }

    #[test]
    fn segment_bursts_is_empty_for_an_all_prose_turn() {
        // Nothing worth a receipt for -- the text keeps rendering as
        // plain prose, exactly as it always has.
        let items = vec![user_message("hi"), assistant_delta("hello there")];
        assert_eq!(segment_bursts(&items), Vec::new());
    }

    #[test]
    fn segment_bursts_finds_a_single_open_burst_while_tools_are_unfinished() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            // no matching tool_finished("a", ..) yet
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 2,
                closed: false,
            }]
        );
    }

    #[test]
    fn segment_bursts_stays_open_while_an_approval_is_pending() {
        // A pending approval means its call has no `ToolCallFinished`
        // yet -- covered by the same "every call finished" check, no
        // separate approval-specific branch needed.
        let items = vec![
            user_message("delete the file"),
            approval_requested("a"),
            // no matching tool_finished("a", ..) yet: still pending.
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        assert!(!bursts[0].closed);
    }

    #[test]
    fn segment_bursts_closes_once_tools_are_done_and_text_follows() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 3,
                closed: true,
            }]
        );
    }

    #[test]
    fn segment_bursts_starts_a_new_burst_for_a_tool_call_after_closing_text() {
        // The model answered, then decided to run one more tool call --
        // round 5 (monotone splitting): the first burst stays closed
        // forever, and this is a brand new *second* burst, not a reopen.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
            tool_requested(
                "b",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![
                Burst {
                    start: 1,
                    end: 3,
                    closed: true,
                },
                Burst {
                    start: 4,
                    end: 5,
                    closed: false,
                },
            ]
        );
    }

    #[test]
    fn segment_bursts_closes_on_a_committed_assistant_message_too() {
        // Accepts either a streaming delta or an already-committed
        // assistant `Message` as the closing text.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished("a", json!({"exit_code": 0, "output": "ok"})),
            assistant_message("Fixed it, tests pass."),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        assert!(bursts[0].closed);
    }

    #[test]
    fn segment_bursts_an_interjected_user_message_never_closes_a_burst() {
        // The user typing again mid-burst doesn't count as assistant
        // text and doesn't split anything -- a later tool call still
        // just extends the same still-open burst, which then closes
        // normally once real (assistant) text follows it.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo build"})),
            tool_finished("a", json!({"exit_code": 0, "output": ""})),
            user_message("still there?"),
            tool_requested("b", "bash", json!({"command": "cargo test"})),
            tool_finished("b", json!({"exit_code": 0, "output": ""})),
        ];
        // No assistant text anywhere yet: one still-open burst spanning
        // straight through the interjection to both bash calls.
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 6,
                closed: false,
            }]
        );

        let mut closed_items = items;
        closed_items.push(assistant_message("Both ran fine."));
        assert_eq!(
            segment_bursts(&closed_items),
            vec![Burst {
                start: 1,
                end: 6,
                closed: true,
            }]
        );
    }

    #[test]
    fn segment_bursts_two_tool_text_tool_runs_are_two_bursts() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Found it, fixing now."),
            tool_requested(
                "b",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("b", json!({"path": "a.rs", "replaced": true})),
            assistant_message("Fixed."),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![
                Burst {
                    start: 1,
                    end: 3,
                    closed: true,
                },
                Burst {
                    start: 4,
                    end: 6,
                    closed: true,
                },
            ]
        );
    }

    #[test]
    fn segment_bursts_turn_ended_closes_the_trailing_burst_even_with_no_closing_text() {
        // Tools ran right up to the end -- no assistant text ever
        // followed them, but `TurnEnded` still closes the burst (it
        // folds directly into the final receipt, `AgentView::
        // render_turn`'s job, not this function's).
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished("a", json!({"exit_code": 0, "output": ""})),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 12),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 3,
                closed: true,
            }]
        );
    }

    #[test]
    fn segment_bursts_turn_ended_closes_an_already_text_closed_burst_the_same_way() {
        // The common case: tools finish, text follows (closes the
        // burst already), then `TurnEnded` arrives -- still exactly one
        // closed burst, unaffected by the extra close signal.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished("a", json!({"exit_code": 0, "output": ""})),
            assistant_message("Done, tests pass."),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 12),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 3,
                closed: true,
            }]
        );
    }

    #[test]
    fn a_burst_reconstructs_the_same_receipt_content_a_completed_turns_own_aggregation_would() {
        // A closed burst's own item range feeds `aggregate_receipt`/
        // `receipt_prose` exactly the way a whole completed turn's items
        // used to -- proving per-burst aggregation reuses the existing
        // machinery verbatim, just scoped to the burst's own range.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("a", json!({"returned_count": 2})),
            tool_requested("b", "fs.read", json!({"path": "a.rs"})),
            tool_finished("b", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        let burst = &bursts[0];
        assert!(burst.closed);
        let tool_calls = build_tool_call_views(&items[burst.start..burst.end]);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("1 tool call · read 1 file")
        );
        assert_eq!(aggregate.bash_count, 0);
        assert!(aggregate.individual_calls.is_empty());
    }

    #[test]
    fn a_bursts_start_index_stays_stable_as_more_items_stream_in() {
        // Proves the rendering-side receipt key (`base_index +
        // burst.start`) stays stable across re-renders: appending more
        // items to the tail (new deltas/tool calls arriving) never
        // changes the `start` a burst already claimed in an earlier,
        // shorter snapshot of the same items.
        let short = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        let first_start = segment_bursts(&short)[0].start;

        let mut grown = short.clone();
        grown.push(assistant_delta(" think the bug is here."));
        grown.push(tool_requested(
            "b",
            "fs.edit",
            json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
        ));
        let grown_bursts = segment_bursts(&grown);
        assert_eq!(grown_bursts.len(), 2);
        assert_eq!(grown_bursts[0].start, first_start);
    }
}
