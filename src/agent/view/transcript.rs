//! The transcript body: per-item rendering (`render_item`, its orphan
//! tool-call fallback), virtual transcript-row projection, the
//! receipt line and its expanded per-call chip (`render_receipt`,
//! `render_receipt_chip`), the in-progress burst's running card
//! (`render_running_card`), and the session-wide Changes overview bar
//! (`render_changes_bar`, `render_changes_list`).

use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::tag::Tag;
use gpui_component::text::TextView;
use gpui_component::Sizable as _;
use horizon_agent::contract::{MessageRole, SessionState, ToolCallId, TurnEndReason};
use horizon_agent::frame::AgentFrameItem;
use horizon_workspace::commands::CommandId;

use super::super::turns;
use crate::theme;
use crate::workspace::RunCommand;

use super::AgentTranscript;

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
            let visible = matches!(
                item,
                AgentFrameItem::Message(_)
                    | AgentFrameItem::AssistantTextDelta(_)
                    | AgentFrameItem::Error(_)
                    | AgentFrameItem::Exited(_)
            ) || matches!(item, AgentFrameItem::ReasoningDelta(_) if span.ended.is_none())
                || matches!(
                    item,
                    AgentFrameItem::ApprovalRequested(request)
                        if span.ended.is_some()
                            && turns::is_approval_still_pending(turn_items, &request.call_id)
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

impl AgentTranscript {
    /// Reconcile the intrusive variable-height list with the latest folded
    /// frame. Stable prefix rows retain their measured heights; only the
    /// changed append tail is spliced. A descriptor-stable streaming update
    /// remeasures the last row because its Markdown/tool content may have
    /// grown in place.
    pub(super) fn sync_transcript_rows(&mut self, cx: &mut Context<Self>) {
        let (next_rows, latest_user_row) = {
            let session = self.session.read(cx);
            let rows = build_transcript_rows(&session.frame.items);
            let calls = turns::build_tool_call_views(&session.frame.items);
            self.session_changes = turns::aggregate_changes(&calls);
            rows
        };
        let stable_prefix = self
            .transcript_rows
            .iter()
            .zip(&next_rows)
            .take_while(|(old, new)| old == new)
            .count();

        if stable_prefix < self.transcript_rows.len() || stable_prefix < next_rows.len() {
            self.transcript_list.splice(
                stable_prefix..self.transcript_rows.len(),
                next_rows.len() - stable_prefix,
            );
        } else if !next_rows.is_empty() {
            let last = next_rows.len() - 1;
            self.transcript_list.remeasure_items(last..last + 1);
        }

        self.transcript_rows = next_rows;
        self.latest_user_row = latest_user_row;
    }

    /// Construct one visible list row on demand. The session frame stays in
    /// the model entity; only the selected descriptor and its referenced
    /// slices participate in this frame's Markdown/tool rendering.
    pub(super) fn render_transcript_row(
        &mut self,
        row_index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.transcript_rows.get(row_index).cloned() else {
            return Empty.into_any_element();
        };
        let element = match row {
            TranscriptRow::Item { turn, index } => {
                let turn_start = turn.start;
                let turn_items = {
                    let session = self.session.read(cx);
                    session.frame.items.get(turn).map(<[_]>::to_vec)
                };
                let Some(turn_items) = turn_items else {
                    return Empty.into_any_element();
                };
                let Some(item) = turn_items.get(index.saturating_sub(turn_start)) else {
                    return Empty.into_any_element();
                };
                self.render_item(&turn_items, index, item, cx)
                    .unwrap_or_else(|| Empty.into_any_element())
            }
            TranscriptRow::Burst {
                items,
                receipt_key,
                presentation,
            } => {
                let items = {
                    let session = self.session.read(cx);
                    session.frame.items.get(items).map(<[_]>::to_vec)
                };
                let Some(items) = items else {
                    return Empty.into_any_element();
                };
                match presentation {
                    BurstPresentation::Running => self.render_running_card(&items, cx),
                    BurstPresentation::Intermediate => self.render_receipt(
                        receipt_key,
                        &items,
                        turns::ReceiptTail::Intermediate,
                        cx,
                    ),
                    BurstPresentation::Final(end) => self.render_receipt(
                        receipt_key,
                        &items,
                        turns::ReceiptTail::Final(&end),
                        cx,
                    ),
                }
            }
        };

        div().px_2().pb_2().child(element).into_any_element()
    }

    /// Toggles a completed turn's receipt expansion (decision 3's `▸`/`▾`).
    fn toggle_receipt(&mut self, receipt_key: usize, cx: &mut Context<Self>) {
        if !self.expanded_receipts.remove(&receipt_key) {
            self.expanded_receipts.insert(receipt_key);
        }
        self.transcript_list.remeasure();
        cx.notify();
    }

    /// Toggles the Changes overview bar's expansion (decision 9).
    fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        self.changes_expanded = !self.changes_expanded;
        cx.notify();
    }

    /// Renders one item outside its normal turn/burst/receipt grouping --
    /// either as one projected virtual row (`Message`/
    /// `AssistantTextDelta`/`Error`/`Exited`, plus the defensive
    /// already-ended-turn-with-a-dangling-approval case), or, defensively,
    /// an item that has genuinely ended up outside every turn span at all
    /// (`AgentTranscript::render`'s own item walk -- see
    /// `turns::group_into_turns`'s
    /// invariant notes for why that should be unreachable for any
    /// legitimate sequence now). `all_items` is whatever superset of
    /// `item` the caller has in scope (a turn's own slice, or the whole
    /// frame) -- used only by the tool-related arms below to correlate a
    /// possibly-orphaned `ToolCallRequested`/`ToolCallFinished` back to
    /// its call's other items for humane rendering.
    pub(super) fn render_item(
        &self,
        all_items: &[AgentFrameItem],
        index: usize,
        item: &AgentFrameItem,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let block = |label: &str, label_color: Hsla, text: String| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(label_color)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme::text_primary())
                        .child(text),
                )
                .into_any_element()
        };
        // Assistant content renders as Markdown (gpui-component's `TextView`,
        // reuse over port); the element id keys its managed parse state, so
        // it must stay stable across re-renders of the same transcript item.
        let markdown_block =
            |label: &str, label_color: Hsla, id: (&'static str, usize), text: String| {
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(label_color)
                            .child(label.to_string()),
                    )
                    .child(
                        TextView::markdown(id, text)
                            .text_size(px(13.0))
                            .text_color(theme::text_primary()),
                    )
                    .into_any_element()
            };
        match item {
            AgentFrameItem::Message(message) => Some(match message.role {
                MessageRole::User => block("you", theme::accent(), message.text.clone()),
                // Unknown renders as agent-authored -- see `MessageRole::
                // Unknown`'s doc (never invent user words).
                MessageRole::Assistant | MessageRole::Unknown => markdown_block(
                    "agent",
                    theme::info(),
                    ("agent-message", index),
                    message.text.clone(),
                ),
            }),
            AgentFrameItem::AssistantTextDelta(delta) => Some(markdown_block(
                "agent…",
                theme::info(),
                ("agent-delta", index),
                delta.text.clone(),
            )),
            AgentFrameItem::ReasoningDelta(delta) => {
                // Height-bounded tail view (owner requirement 2026-07-13,
                // closing an un-instructed deviation from base decision
                // 5): `delta.text` is the item's own coalesced field, so
                // this re-caps the whole accumulated block on every
                // render rather than growing unboundedly while it
                // streams (`turns::cap_thinking_text`'s own doc comment).
                // The "…" label suffix mirrors `AssistantTextDelta`'s own
                // "agent…" -- thinking only ever exists as this streaming
                // delta shape, never a committed message, so it always
                // reads as in-progress.
                let (tail_text, omitted) =
                    turns::cap_thinking_text(&delta.text, turns::THINKING_TAIL_LINES);
                let text = if omitted > 0 {
                    format!("… {omitted} earlier line(s) …\n{tail_text}")
                } else {
                    tail_text
                };
                Some(block("thinking…", theme::text_subtle(), text))
            }
            // Retired the raw-JSON `tool`/`tool result` dumps this arm and
            // the one below used to fall back to (owner feedback
            // 2026-07-13: leaking `{tool_id} {input}`/output JSON straight
            // to the transcript was part of the "incomprehensible screen
            // state" report -- see `turns::group_into_turns`'s invariant
            // notes for the actual root cause; both items should be
            // unreachable here for any legitimate sequence now, but a
            // genuinely unknown future shape must still degrade to the
            // same humane verb/target/summary vocabulary the running
            // card/receipt rows use, not `Display`-dumped JSON).
            AgentFrameItem::ToolCallRequested(request) => {
                self.render_orphan_tool_row(all_items, index, &request.call_id, cx)
            }
            AgentFrameItem::ToolCallFinished(result) => {
                self.render_orphan_tool_row(all_items, index, &result.call_id, cx)
            }
            AgentFrameItem::ApprovalRequested(request) => {
                // The actionable (ghost-excluding) reading: this arm only
                // renders at all for the defensive completed-turn-with-a-
                // dangling-approval case (`turns::is_approval_still_pending`,
                // which deliberately keeps the *unscoped* reading for its own
                // purpose -- see that function's doc comment). By the time a
                // request's own turn has ended without resolving, it's a
                // ghost with no live daemon-side gate left to answer a
                // decision (`docs/agent-output-ui-amendment.md`'s post-review
                // note) -- so buttons never show here; the box is purely
                // informational.
                let pending = self
                    .session
                    .read(cx)
                    .pending_approval_call_ids()
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
                        .border_color(theme::warning())
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme::warning())
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
            // A humane one-liner, not `ToolCallProgress`'s `Debug` dump
            // (owner feedback 2026-07-13: a raw Debug format leaking
            // through was part of the "incomprehensible screen state"
            // report -- see `turns::group_into_turns`'s doc comment for
            // the actual root cause; this item only reaches the flat
            // per-item fallback at all in that same narrow edge case, so
            // it's humanized defensively rather than left raw).
            AgentFrameItem::ToolCallPreparing(progress) => {
                let verb = progress.tool_id.as_deref().unwrap_or("tool call");
                Some(block(
                    "tool (preparing)",
                    theme::text_subtle(),
                    format!("{verb} … ({} bytes streamed)", progress.bytes),
                ))
            }
            AgentFrameItem::Error(error) => {
                Some(block("error", theme::danger(), format!("{error:?}")))
            }
            AgentFrameItem::Exited(reason) => {
                Some(block("exited", theme::text_muted(), format!("{reason:?}")))
            }
            AgentFrameItem::ToolCallStarted(_) => None,
            // Consumed by turn grouping (`turns::group_into_turns`) into
            // the turn's receipt line; never reaches this per-item path in
            // practice (see `AgentTranscript::render`'s span walk), kept only as a
            // defensive no-op.
            AgentFrameItem::TurnEnded { .. } => None,
        }
    }

    /// [`Self::render_item`]'s defensive fallback for a tool call whose
    /// `ToolCallRequested`/`ToolCallFinished` item has genuinely ended up
    /// outside every turn span: renders it with the same glyph +
    /// verb/target/summary vocabulary as a running-card row
    /// ([`tool_call_glyph`]/[`tool_call_line_text`]), correlating across
    /// `all_items` (rather than just the one orphaned item) so the result
    /// still reflects the call's actual tool id/input/output wherever its
    /// other items happen to live. Skips re-rendering a call whose row
    /// already appeared at an earlier index within `all_items` -- a
    /// call's `ToolCallRequested`/`ApprovalRequested`/`ToolCallFinished`
    /// items can each independently land in this fallback if they're all
    /// orphaned, and would otherwise each mint their own duplicate row.
    /// Falls back to a minimal call-id-only line (never a raw-JSON dump)
    /// in the genuinely-shouldn't-happen case where `all_items` doesn't
    /// even contain the call's own `ToolCallRequested` to classify from.
    fn render_orphan_tool_row(
        &self,
        all_items: &[AgentFrameItem],
        index: usize,
        call_id: &ToolCallId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let already_rendered = all_items[..index]
            .iter()
            .filter_map(item_call_id)
            .any(|seen| seen == call_id);
        if already_rendered {
            return None;
        }
        match turns::build_tool_call_views(all_items)
            .into_iter()
            .find(|call| &call.call_id == call_id)
        {
            Some(call) => Some(self.render_tool_call_row(all_items, &call, false, cx)),
            None => Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .text_size(px(12.0))
                    .text_color(theme::text_muted())
                    .child(format!("tool call {}", call_id.0))
                    .into_any_element(),
            ),
        }
    }

    /// One burst's one-line receipt (decision 1, aggregated per owner
    /// feedback 2026-07-13 -- see `docs/agent-output-ui-amendment.md`'s
    /// post-review note): the `▸`/`▾` expansion affordance
    /// (accent-tinted), prose counts for the low-signal query/edit calls
    /// (`turns::receipt_prose`), individual chips only for bash calls and
    /// any failed call, then a `tail` -- the turn's actual final burst
    /// (round 5) carries the end-reason status + model id
    /// (`ReceiptTail::Final`); every other burst's receipt carries
    /// neither (`ReceiptTail::Intermediate` -- the contract has no
    /// per-burst timing to show). The row carries a persistent-but-quiet
    /// resting-state look (a faint border + rounded corners + modest
    /// padding -- the same muted-border language as the expanded row
    /// list below) plus a stronger hover background, both round 2 of the
    /// "hard to notice it's clickable" feedback. Clicking anywhere on
    /// the row toggles `receipt_key`'s expansion (mock 6a): the per-call
    /// row list (decision 3) renders beneath, each row individually
    /// expandable in turn (`render_expandable_tool_call_row`) --
    /// unaggregated, exactly as built for stage D.
    pub(super) fn render_receipt(
        &self,
        receipt_key: usize,
        items: &[AgentFrameItem],
        tail: turns::ReceiptTail<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tool_calls = turns::build_tool_call_views(items);
        let aggregate = turns::aggregate_receipt(&tool_calls);
        let prose = turns::receipt_prose(&aggregate);
        let (status, model, halted) = match tail {
            turns::ReceiptTail::Final(end) => {
                let status = turns::receipt_status(end);
                let color = if status.is_error {
                    theme::danger()
                } else {
                    theme::text_muted()
                };
                let halted = matches!(
                    end.reason,
                    TurnEndReason::Halted
                        | TurnEndReason::HaltedByIterationCap
                        | TurnEndReason::HaltedByDoomLoop
                );
                (Some((status.text, color)), end.model.clone(), halted)
            }
            turns::ReceiptTail::Intermediate => (None, None, false),
        };
        let receipt_text =
            |color: Hsla, text: String| div().text_size(px(11.0)).text_color(color).child(text);
        let separator = || receipt_text(theme::text_subtle(), "·".to_string());

        let expanded = self.expanded_receipts.contains(&receipt_key);
        let arrow = if expanded { "▾" } else { "▸" };

        let mut row = div()
            .id(ElementId::from(format!("receipt-{receipt_key}")))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .cursor_pointer()
            .hover(|this| this.bg(theme::text_subtle().alpha(0.12)))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.toggle_receipt(receipt_key, cx);
            }))
            .child(receipt_text(theme::accent(), arrow.to_string()));
        if let Some(prose) = &prose {
            row = row.child(receipt_text(theme::text_muted(), prose.clone()));
        }
        for call in &aggregate.individual_calls {
            row = row.child(self.render_receipt_chip(call));
        }
        let has_leading_content = prose.is_some() || !aggregate.individual_calls.is_empty();
        if let Some((status_text, status_color)) = status {
            if has_leading_content {
                row = row.child(separator());
            }
            row = row.child(receipt_text(status_color, status_text));
            if let Some(model) = &model {
                row = row.child(separator());
                row = row.child(
                    div()
                        .max_w(px(220.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(theme::text_subtle())
                        .child(model.clone()),
                );
            }
            if halted {
                row = row.child(render_continue_button(receipt_key));
            }
        }

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(row.into_any_element());
        if expanded && !tool_calls.is_empty() {
            wrapper = wrapper.child(self.render_expanded_receipt_rows(items, &tool_calls, cx));
        }
        wrapper.into_any_element()
    }

    /// One receipt chip -- post-aggregation (owner feedback 2026-07-13:
    /// query/edit calls fold into prose, then bash followed suit once a
    /// dozen near-identical `cd … && …` chips turned out just as
    /// uninformative), only rendered for `aggregate_receipt`'s
    /// `individual_calls` -- any failed call, of any class, plus the
    /// defensive never-finished case: a bash chip (command head + mark)
    /// for a failed bash call, a file chip (name + mark -- no diffstat
    /// once failed, see below) for a failed fs.edit/fs.write/fs.patch, and a
    /// plain verb + mark for everything else.
    fn render_receipt_chip(&self, call: &turns::ToolCallView) -> AnyElement {
        let (mark, mark_color) = if !call.finished {
            ("…", theme::text_subtle())
        } else if call.is_error {
            ("✗", theme::danger())
        } else {
            ("✓", theme::success())
        };

        let content: AnyElement = match &call.kind {
            turns::ToolCallKind::File {
                file_name,
                diffstat,
            } => {
                let mut label = div().flex().flex_row().items_center().gap_1().child(
                    div()
                        .max_w(px(160.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(file_name.clone()),
                );
                if call.is_error {
                    // A failed edit/write never actually applied (the
                    // tool aborts before writing) -- showing the
                    // would-be diffstat here would misleadingly imply it
                    // did. Owner feedback 2026-07-13: a failed call keeps
                    // its own error-marked chip regardless of class, so
                    // just the mark, not the attempted diffstat.
                    label =
                        label.child(div().text_size(px(11.0)).text_color(mark_color).child(mark));
                } else if let Some((added, removed)) = diffstat.filter(|_| call.finished) {
                    label = label
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::success())
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::danger())
                                .child(format!("−{removed}")),
                        );
                } else if !call.finished {
                    label = label.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_subtle())
                            .child(mark),
                    );
                }
                label.into_any_element()
            }
            turns::ToolCallKind::Bash { command_head } => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(format!("bash {command_head}")),
                )
                .child(div().text_size(px(11.0)).text_color(mark_color).child(mark))
                .into_any_element(),
            turns::ToolCallKind::Generic => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(call.verb.to_lowercase()),
                )
                .child(div().text_size(px(11.0)).text_color(mark_color).child(mark))
                .into_any_element(),
        };

        // `Tag::custom` (rather than `Tag::secondary()`/etc.) so the chip's
        // colors resolve through Horizon's own `theme` roles, not
        // gpui-component's independent, uncustomized global `Theme` (see
        // `src/theme.rs`'s module doc).
        Tag::custom(
            transparent_black(),
            theme::text_muted(),
            theme::text_subtle(),
        )
        .rounded_full()
        .xsmall()
        .child(content)
        .into_any_element()
    }

    /// The in-progress *burst*'s card (decision 2; mock 2a/3b/7a's "live
    /// card"; round 5 owner decision 2026-07-13 scopes this to one
    /// `turns::Burst`'s own item range rather than the whole turn's --
    /// see [`build_transcript_rows`]): a thin accent-tinted border
    /// around the whole card (the mock's border is a muted echo of the
    /// accent hue, not a full-saturation perimeter — see `accent_tint`),
    /// a faint accent-tinted fill scoped to the header strip only, and a
    /// header (status dot + bold state label — the card's one
    /// full-strength accent element, plus `n / m` progress + ticking
    /// elapsed seconds + the stop button, decision 6/mock 7a --
    /// `render_stop_button`, dispatching `CancelAgentTurn` through the
    /// same `RunCommand` action path as the palette) and one
    /// row per tool call in `items` (the burst's own range, not
    /// necessarily every tool call the turn has made). The row area
    /// itself carries no distinct panel fill, matching the mock's card
    /// having no background of its own beyond the header tint.
    /// `overflow_hidden` keeps row/chip content that would otherwise
    /// overflow (long paths, command heads) from painting past the
    /// card's rounded corners.
    ///
    /// A pending approval renders *inline in its own row*
    /// (`render_tool_call_row`'s `Waiting` branch), not as a standalone
    /// box below every row (owner feedback 2026-07-13, round 3: "can't
    /// tell which tool call corresponds to which approval" -- a screen
    /// with over a dozen stacked yellow boxes and no visible link back to
    /// the call that requested each one). There is no longer any
    /// `ApprovalRequested` rendering path inside the running card at all.
    pub(super) fn render_running_card(
        &self,
        items: &[AgentFrameItem],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tool_calls = turns::build_tool_call_views(items);
        let (finished, total) = turns::progress(&tool_calls);
        let elapsed = self
            .running_turn_clock
            .map(|clock| clock.started_at.elapsed())
            .unwrap_or_default();
        let state_label = self
            .session
            .read(cx)
            .frame
            .state
            .map(running_state_label)
            .unwrap_or("running…");

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .bg(accent_tint(0.14))
            .border_b_1()
            .border_color(accent_tint(0.3))
            .child(
                div()
                    .flex_none()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme::accent()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent())
                    .child(state_label),
            )
            // Spacer: pushes the progress/elapsed text and the stop button
            // (stage F, mock 7a) to the header's right edge.
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme::text_muted())
                    .child(format!(
                        "{finished} / {total} · {}",
                        turns::humanize_duration(elapsed)
                    )),
            )
            .child(render_stop_button("running-card-stop"));

        let mut card = div()
            .flex()
            .flex_col()
            .rounded_sm()
            .border_1()
            .border_color(accent_tint(0.35))
            .overflow_hidden()
            .child(header);

        let row_count = tool_calls.len();
        for (row_index, call) in tool_calls.iter().enumerate() {
            card =
                card.child(self.render_tool_call_row(items, call, row_index + 1 < row_count, cx));
        }

        card.into_any_element()
    }

    /// The Changes overview bar (`docs/agent-output-ui-design.md` decision
    /// 9, never ported from the retired Floem shell -- rebuilt fresh):
    /// a collapsible aggregation of every file the session has edited or
    /// written, across the *whole* session's items, not just the visible
    /// transcript window. `None` when no file was ever touched
    /// (`turns::changes_summary_text`'s own gating), hiding the bar
    /// entirely rather than showing a hollow "0 files" row -- no adopted
    /// mock in `agent-ui-options.html` draws an overview bar of this shape
    /// (only 8a's unrelated, unadopted session-manager option mentions a
    /// diffstat at all), so this reuses the receipt row's own idiom
    /// instead: a faint persistent border + rounded corners + modest
    /// padding (`render_receipt`'s "quiet pill/button row" language) with
    /// a stronger hover background, and an accent-tinted `▸`/`▾` toggle.
    /// Clicking anywhere on the row expands a bordered, rounded, height-
    /// capped-and-scrollable list below it (mirroring
    /// `render_expanded_receipt_rows`' own container), one row per file:
    /// filename, muted full path, this file's own `+n −m` (`theme::
    /// success`/`theme::danger`, the same roles the receipt chip's file
    /// diffstat already uses), and a "created" tag for a write that
    /// created rather than overwrote. No further drill-down per row in
    /// this pass -- the receipts already offer a per-call diff/preview;
    /// wiring a click-through from a Changes row to its originating
    /// call's receipt is a possible future hook, not built here.
    pub(super) fn render_changes_bar(
        &self,
        changes: &[turns::FileChange],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let summary = turns::changes_summary_text(changes)?;

        let expanded = self.changes_expanded;
        let arrow = if expanded { "▾" } else { "▸" };
        let bar_text =
            |color: Hsla, text: String| div().text_size(px(11.0)).text_color(color).child(text);

        let row = div()
            .id("changes-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .cursor_pointer()
            .hover(|this| this.bg(theme::text_subtle().alpha(0.12)))
            .on_click(cx.listener(|view, _, _, cx| view.toggle_changes(cx)))
            .child(bar_text(theme::accent(), arrow.to_string()))
            .child(bar_text(theme::text_muted(), "Changes".to_string()))
            .child(bar_text(theme::text_subtle(), "·".to_string()))
            .child(bar_text(theme::text_muted(), summary));

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .child(row.into_any_element());
        if expanded {
            wrapper = wrapper.child(self.render_changes_list(changes));
        }
        Some(wrapper.into_any_element())
    }

    /// The Changes overview bar's expanded per-file list: a bordered,
    /// rounded, `max_h` + `overflow_y_scroll` container (so a
    /// large-session change list can't swallow the pane, the same
    /// height-capped-scroll idiom `render_line_body`'s output bodies use)
    /// holding one row per [`turns::FileChange`], in the aggregation's own
    /// first-touch order.
    fn render_changes_list(&self, changes: &[turns::FileChange]) -> AnyElement {
        let mut list = div()
            .id("changes-list")
            .flex()
            .flex_col()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25));
        let row_count = changes.len();
        for (row_index, change) in changes.iter().enumerate() {
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .when(row_index + 1 < row_count, |this| {
                    this.border_b_1()
                        .border_color(theme::text_subtle().alpha(0.2))
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme::text_primary())
                        .child(change.file_name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(theme::text_subtle())
                        .child(change.path.clone()),
                );
            if change.created {
                row = row.child(
                    Tag::custom(
                        transparent_black(),
                        theme::text_muted(),
                        theme::text_subtle(),
                    )
                    .rounded_full()
                    .xsmall()
                    .child("created"),
                );
            }
            row = row
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme::success())
                        .child(format!("+{}", change.added)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme::danger())
                        .child(format!("−{}", change.removed)),
                );
            list = list.child(row);
        }
        list.into_any_element()
    }
}

/// The `ToolCallId` `item` references, if any -- used by
/// [`AgentTranscript::render_orphan_tool_row`] to correlate a possibly-orphaned
/// item back to its call's other items anywhere in a wider item slice,
/// and to de-duplicate against an earlier item for the same call.
fn item_call_id(item: &AgentFrameItem) -> Option<&ToolCallId> {
    match item {
        AgentFrameItem::ToolCallRequested(request) => Some(&request.call_id),
        AgentFrameItem::ToolCallStarted(call_id) => Some(call_id),
        AgentFrameItem::ToolCallFinished(result) => Some(&result.call_id),
        AgentFrameItem::ApprovalRequested(request) => Some(&request.call_id),
        _ => None,
    }
}

/// The stop affordance (decision 6, mock 7a): a small, quiet button --
/// outlined rather than filled, "danger-leaning but not alarming" per the
/// mock's neutral-gray chrome, distinct from the emphatic filled
/// `.danger()` styling the row-level Deny button uses -- that dispatches
/// `CommandId::CancelAgentTurn` through the same [`RunCommand`] gpui
/// action the palette and `[keybindings]` chords use
/// (`WorkspaceShell::execute`), rather than calling `AgentSession::cancel`
/// directly: AGENTS.md's "operations go through the command model"
/// convention, and the one path every cancel source -- keyboard, palette,
/// control plane, now the pointer too -- funnels through. `id` is a plain
/// string rather than a `call_id`: unlike the tool-call rows, there is at
/// most one stop affordance of each kind on screen at a time (one running
/// card, one status line), so no per-call disambiguation is needed. A
/// free function (no `&self`/`Context` needed) since the click handler is
/// entirely stateless -- it only dispatches an action, it never touches
/// `AgentTranscript`'s own fields -- so it works identically from the running
/// card's header and the status line (the latter needs its own copy since
/// the running card's *last burst* can close, folding into a receipt,
/// before `TurnEnded` arrives to end the turn -- round 5's "burst-fold
/// gap": final-text streaming can leave no card on screen at all while a
/// turn is still technically in flight).
pub(super) fn render_stop_button(id: &'static str) -> AnyElement {
    Button::new(id)
        .outline()
        .danger()
        .xsmall()
        .label("Stop")
        .on_click(|_, window, cx| {
            window.dispatch_action(
                Box::new(RunCommand {
                    id: CommandId::CancelAgentTurn,
                }),
                cx,
            );
        })
        .into_any_element()
}

/// The Continue affordance on a guard-halted turn's receipt row
/// (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution,
/// decision 3): plain outline styling, not `.danger()` -- a halt reads as a
/// calm pause here, the opposite tone from [`render_stop_button`]'s
/// deliberately danger-leaning Stop. Dispatches `CommandId::
/// ContinueAgentTurn` through the same [`RunCommand`] action path as every
/// other command (palette, `[keybindings]`, control plane), rather than
/// calling `AgentSession::continue_turn` directly -- AGENTS.md's
/// "operations go through the command model" convention. Keyed off the
/// receipt's own `receipt_key` (stable across re-renders, same as the
/// row's own `ElementId`) since a transcript can show more than one
/// halted receipt at once (an earlier turn's halt the user never acted
/// on, plus a later one) and each needs its own click target. `Button`
/// already calls `cx.stop_propagation()` on click before running this
/// handler, so clicking it doesn't also toggle the row's own
/// expand/collapse.
fn render_continue_button(receipt_key: usize) -> AnyElement {
    Button::new(ElementId::from(format!("receipt-continue-{receipt_key}")))
        .outline()
        .xsmall()
        .label("Continue")
        .on_click(|_, window, cx| {
            window.dispatch_action(
                Box::new(RunCommand {
                    id: CommandId::ContinueAgentTurn,
                }),
                cx,
            );
        })
        .into_any_element()
}

/// A muted echo of the accent role — the running card's border and
/// header fill (mock 2a/3b/7a: `#bfdbfe`/`#eff6ff`/`#dbeafe`, all clearly
/// the same blue hue as the header's full-strength `#1d4ed8` label and
/// `#2563eb` status dot, just lightened/desaturated toward the page
/// background). Deriving this from `theme::accent()` via `Hsla::alpha`
/// (rather than adding independent `[theme]` hex roles for it) keeps the
/// tint locked to whatever hue the user's `accent` override uses, the
/// same relationship the mock expresses — a separately configured color
/// could drift from the accent hue it's meant to echo.
fn accent_tint(alpha: f32) -> Hsla {
    theme::accent().alpha(alpha)
}

/// The running card's header label for the three in-flight
/// `SessionState`s (`state_indicates_turn_in_flight`'s own set) — any
/// other state falls back to the generic label defensively, since this
/// is only ever called while a turn is in flight.
fn running_state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Running => "running…",
        SessionState::ToolRunning => "tool running…",
        SessionState::WaitingForApproval => "waiting for approval",
        _ => "running…",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use horizon_agent::contract::TurnEndReason;
    use horizon_agent::frame::AgentFrameItem;

    use super::super::super::turns::test_support::{
        assistant_delta, tool_finished, tool_requested, tool_started, user_message,
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
}
