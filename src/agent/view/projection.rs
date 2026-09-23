//! Pure projection of folded transcript items into virtual-list row descriptors.

use std::ops::Range;

use horizon_agent::frame::AgentFrameItem;

use super::super::turns;

/// One independently measured transcript row. Keeping only frame indices and
/// owned presentation metadata lets GPUI's variable-height list construct the
/// visible rows on demand without cloning the full frame or building every old
/// turn during a scroll frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TranscriptRow {
    Item {
        turn: Range<usize>,
        index: usize,
    },
    Burst {
        items: Range<usize>,
        receipt_key: usize,
        presentation: BurstPresentation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BurstPresentation {
    Running,
    Intermediate,
    Final(turns::TurnEnd),
}

/// Project the append-oriented frame into visual rows. This is the descriptor
/// twin of the former eager `render_turn` walk: burst folding and item
/// visibility are identical, but no GPUI elements are constructed here.
pub(super) fn build_transcript_rows(
    items: &[AgentFrameItem],
) -> (Vec<TranscriptRow>, Option<usize>) {
    let mut rows = Vec::new();
    let mut latest_user_row = None;

    for span in turns::group_into_turns(items) {
        let turn_items = &items[span.start..span.end];
        let first_row = rows.len();
        let bursts = turns::segment_bursts(turn_items);
        let last_burst_index = bursts.len().checked_sub(1);
        let mut burst_cursor = 0usize;
        let mut index = 0usize;

        while index < turn_items.len() {
            if let Some(burst) = bursts.get(burst_cursor) {
                if burst.start == index {
                    let is_final = Some(burst_cursor) == last_burst_index;
                    let presentation = match (&span.ended, is_final, burst.closed) {
                        (Some(end), true, _) => BurstPresentation::Final(end.clone()),
                        (_, _, true) => BurstPresentation::Intermediate,
                        _ => BurstPresentation::Running,
                    };
                    rows.push(TranscriptRow::Burst {
                        items: span.start + burst.start..span.start + burst.end,
                        receipt_key: span.start + burst.start,
                        presentation,
                    });
                    index = burst.end;
                    burst_cursor += 1;
                    continue;
                }
            }

            let item = &turn_items[index];
            // Thinking (`ReasoningDelta`) content is deliberately never a
            // row — hidden in full, streaming and replayed alike (owner
            // decision 2026-09-10, superseding 2026-07-13's tail-capped
            // view). Same-day owner feedback carves out exactly one
            // display affordance: while thinking is the open turn's
            // *current* activity, the delta projects an indicator-only
            // row (see `render_item`'s thinking arm) so a long reasoning
            // phase doesn't read as an idle pane.
            let visible = matches!(
                item,
                AgentFrameItem::Message(_)
                    | AgentFrameItem::AssistantTextDelta(_)
                    | AgentFrameItem::Error(_)
                    | AgentFrameItem::Exited(_)
                    // The compaction divider always gets its own row --
                    // `segment_bursts` closes the surrounding burst at it
                    // precisely so it can never be swallowed by a receipt.
                    | AgentFrameItem::HistoryCleared(_)
                    | AgentFrameItem::ProviderRateLimited(_)
            ) || matches!(
                item,
                AgentFrameItem::ApprovalRequested(request)
                    if span.ended.is_some()
                        && turns::is_approval_still_pending(turn_items, &request.call_id)
            ) || matches!(
                // The thinking-indicator carve-out: only while this
                // reasoning delta is the open turn's latest item. Anything
                // streaming after it (assistant text, a tool call) or the
                // turn ending retires the indicator, so it can never
                // linger as a stale pulse.
                item,
                AgentFrameItem::ReasoningDelta(_)
                    if span.ended.is_none() && index + 1 == turn_items.len()
            );
            if visible {
                rows.push(TranscriptRow::Item {
                    turn: span.start..span.end,
                    index: span.start + index,
                });
            }
            index += 1;
        }

        if turns::contains_user_message(turn_items) && rows.len() > first_row {
            // Keep the prior affordance's turn-level anchor for a user
            // interjection absorbed inside a tool burst.
            latest_user_row = Some(first_row);
        }
    }

    (rows, latest_user_row)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use horizon_agent::contract::TurnEndReason;
    use horizon_agent::frame::AgentFrameItem;

    use super::super::super::turns::test_support::{
        assistant_delta, reasoning_delta, tool_finished, tool_requested, tool_started, user_message,
    };
    use super::{build_transcript_rows, BurstPresentation, TranscriptRow};

    fn turn_end() -> AgentFrameItem {
        AgentFrameItem::TurnEnded {
            reason: TurnEndReason::Completed,
            model: Some("test-model".to_string()),
            elapsed: Duration::from_secs(2),
        }
    }

    #[test]
    fn projection_preserves_message_burst_receipt_and_prose_order() {
        let items = vec![
            user_message("fix it"),
            tool_requested("a", "fs.read", serde_json::json!({"path":"a.rs"})),
            tool_started("a"),
            tool_finished("a", serde_json::json!({"contents":"..."})),
            assistant_delta("done"),
            turn_end(),
            user_message("thanks"),
            assistant_delta("welcome"),
            turn_end(),
        ];

        let (rows, latest_user) = build_transcript_rows(&items);
        assert_eq!(latest_user, Some(3));
        assert!(matches!(&rows[0], TranscriptRow::Item { index: 0, .. }));
        assert!(matches!(
            &rows[1],
            TranscriptRow::Burst {
                items,
                receipt_key: 1,
                presentation: BurstPresentation::Final(_),
            } if items == &(1..4)
        ));
        assert!(matches!(&rows[2], TranscriptRow::Item { index: 4, .. }));
        assert!(matches!(&rows[3], TranscriptRow::Item { index: 6, .. }));
        assert!(matches!(&rows[4], TranscriptRow::Item { index: 7, .. }));
        assert_eq!(rows.len(), 5, "TurnEnded markers are not visual rows");
    }

    #[test]
    fn streaming_text_keeps_its_descriptor_stable_for_targeted_remeasurement() {
        let before = vec![user_message("q"), assistant_delta("a")];
        let after = vec![user_message("q"), assistant_delta("a longer answer")];
        assert_eq!(
            build_transcript_rows(&before).0,
            build_transcript_rows(&after).0
        );
    }

    /// Thinking is hidden in full. A reasoning
    /// delta produces no row while its turn runs and none after it ends —
    /// streaming or replayed — without disturbing neighboring rows or the
    /// latest-user anchor. (The same-day thinking-indicator carve-out is
    /// pinned by
    /// `thinking_shows_an_indicator_row_only_while_it_is_the_open_tail`
    /// below; these cases all have later activity or an ended turn, so
    /// they stay rowless.)
    #[test]
    fn thinking_is_never_a_transcript_row_streaming_or_replayed() {
        let running = vec![
            user_message("q"),
            reasoning_delta("a hidden thought"),
            assistant_delta("a"),
        ];
        let ended = vec![
            user_message("q"),
            reasoning_delta("a hidden thought"),
            assistant_delta("a"),
            turn_end(),
        ];
        for items in [&running, &ended] {
            let (rows, latest_user) = build_transcript_rows(items);
            assert_eq!(latest_user, Some(0));
            let row_indices: Vec<usize> = rows
                .iter()
                .map(|row| match row {
                    TranscriptRow::Item { index, .. } => *index,
                    TranscriptRow::Burst { .. } => {
                        panic!("thinking-only turns must not invent burst rows")
                    }
                })
                .collect();
            assert_eq!(row_indices, vec![0, 2], "the reasoning item is skipped");
        }
    }

    /// Same-day companion pin (owner feedback 2026-09-10): thinking
    /// content stays hidden, but while a reasoning delta is the open
    /// turn's latest item it projects exactly one indicator row (the
    /// breathing-dot treatment), retired the moment anything else
    /// streams or the turn ends — so the pane never looks idle
    /// mid-reasoning, and the indicator never outlives the thinking it
    /// describes.
    #[test]
    fn thinking_shows_an_indicator_row_only_while_it_is_the_open_tail() {
        let row_indices = |rows: &[TranscriptRow]| -> Vec<usize> {
            rows.iter()
                .map(|row| match row {
                    TranscriptRow::Item { index, .. } => *index,
                    TranscriptRow::Burst { .. } => {
                        panic!("a thinking indicator row is never part of a burst")
                    }
                })
                .collect()
        };

        // Tail of a running turn: the reasoning item IS a row (the
        // indicator).
        let (rows, latest_user) =
            build_transcript_rows(&[user_message("q"), reasoning_delta("hmm")]);
        assert_eq!(latest_user, Some(0));
        assert_eq!(
            row_indices(&rows),
            vec![0, 1],
            "open-tail thinking shows its indicator row"
        );

        // Anything streaming after the delta retires the indicator...
        let (rows, _) = build_transcript_rows(&[
            user_message("q"),
            reasoning_delta("hmm"),
            assistant_delta("a"),
        ]);
        assert_eq!(
            row_indices(&rows),
            vec![0, 2],
            "indicator is gone once text streams"
        );

        // ...and an ended turn never keeps one either.
        let (rows, _) =
            build_transcript_rows(&[user_message("q"), reasoning_delta("hmm"), turn_end()]);
        assert_eq!(row_indices(&rows), vec![0], "ended thinking leaves no row");
    }
}
