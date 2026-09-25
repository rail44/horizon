//! Render individual transcript messages and defensive orphan tool rows.

use super::super::super::turns;
use super::AgentTranscript;
use crate::theme;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::text::TextView;
use horizon_agent::{
    contract::{MessageRole, OccurrenceId, ToolCallId},
    frame::AgentFrameItem,
};
use std::time::Duration;

impl AgentTranscript {
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
                            .selectable(true)
                            .text_size(px(crate::terminal::font_size()))
                            .text_color(theme::text_primary()),
                    )
                    .into_any_element()
            };
        // Plain-text bodies render through the same selectable TextView
        // pipeline the assistant markdown uses so every transcript row's text
        // is selectable and copyable; `escape_markdown` keeps the text
        // verbatim (no GFM construct reinterpretation).
        let block = |label: &str, label_color: Hsla, id: (&'static str, usize), text: String| {
            markdown_block(label, label_color, id, escape_markdown(&text))
        };
        match item {
            AgentFrameItem::Message(message) => {
                // The label string is centralized in
                // `MessageRole::display_label`; only the color (accent for
                // the human's own messages, muted info for everything else)
                // and the plain-vs-markdown rendering choice are local here.
                let label = message.role.display_label();
                let color = if message.role == MessageRole::User {
                    theme::accent()
                } else {
                    theme::info()
                };
                if message.role == MessageRole::Assistant {
                    Some(markdown_block(
                        label,
                        color,
                        ("agent-message", index),
                        message.text.clone(),
                    ))
                } else {
                    Some(block(
                        label,
                        color,
                        ("user-message", index),
                        message.text.clone(),
                    ))
                }
            }
            AgentFrameItem::AssistantTextDelta(delta) => Some(markdown_block(
                "agent…",
                theme::info(),
                ("agent-delta", index),
                delta.text.clone(),
            )),
            // Thinking content is hidden in full (superseding 2026-07-13's
            // tail-capped "thinking…" view) — but one affordance is kept
            // while a reasoning delta is the open
            // turn's current tail, its row renders in the running card's
            // own visual language — a breathing `theme::accent()` dot
            // beside a semibold accent "thinking…" label (the owner
            // passed on a loader-icon spinner as too terminal-flavored
            // for a GUI app). No delta text reaches the screen here, and
            // `build_transcript_rows` routes only the tail-of-open-turn
            // item to this arm, so the indicator is always retired by the
            // next streamed item or the turn end.
            AgentFrameItem::ReasoningDelta(_) => {
                // One full breath per cycle: the eased delta runs 0→1 and
                // repeats, so fold it into a triangle wave (0→1→0) and
                // ride the dot's opacity between a dim floor and full
                // accent. The label itself stays static; only the dot
                // breathes.
                let breathe = |dot: Div, delta: f32| {
                    let phase = if delta < 0.5 {
                        delta * 2.0
                    } else {
                        (1.0 - delta) * 2.0
                    };
                    dot.opacity(0.35 + 0.65 * phase)
                };
                Some(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .py_0p5()
                        .child(
                            div()
                                .flex_none()
                                .size(px(6.0))
                                .rounded_full()
                                .bg(theme::accent())
                                .with_animation(
                                    "thinking-pulse",
                                    Animation::new(Duration::from_secs_f64(1.6))
                                        .repeat()
                                        .with_easing(ease_in_out),
                                    breathe,
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::accent())
                                .child("thinking…"),
                        )
                        .into_any_element(),
                )
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
            AgentFrameItem::ToolCallRequested(request) => self.render_orphan_tool_row(
                all_items,
                index,
                &request.call_id,
                &request.occurrence_id,
                cx,
            ),
            AgentFrameItem::ToolCallFinished(result) => self.render_orphan_tool_row(
                all_items,
                index,
                &result.call_id,
                &result.occurrence_id,
                cx,
            ),
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
                    .pending_approval_identities()
                    .contains(&request.identity());
                let call_id = request.identity();
                let deny_id = request.identity();
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
                                                view.session.read(cx).deny(deny_id.clone(), None);
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
                    ("tool-preparing", index),
                    format!("{verb} … ({} bytes streamed)", progress.bytes),
                ))
            }
            // The Tier 1 compaction divider. Deliberately terse and muted:
            // it reports a change to what the *model* sees, not to the
            // transcript -- every cleared result is still rendered above,
            // and still re-fetchable through `recall`.
            AgentFrameItem::HistoryCleared(cleared) => Some(block(
                "context",
                theme::text_subtle(),
                ("context", index),
                format!(
                    "cleared {} old tool result(s) (~{} chars) — recoverable via recall",
                    cleared.cleared_call_ids.len(),
                    cleared.recovered_chars,
                ),
            )),
            AgentFrameItem::MemoryDigest(digest) => {
                if let Some(reason) = &digest.no_update_reason {
                    Some(block(
                        "memory",
                        theme::text_subtle(),
                        ("memory", index),
                        format!("no update — {reason}"),
                    ))
                } else {
                    Some(block(
                        "memory",
                        theme::text_subtle(),
                        ("memory", index),
                        format!("updated {} field(s)", digest.updates.len()),
                    ))
                }
            }
            AgentFrameItem::MemoryCheckpointMissed => Some(block(
                "memory",
                theme::text_subtle(),
                ("memory", index),
                "checkpoint missed — turn ended without a memory update".to_string(),
            )),
            AgentFrameItem::ProviderRateLimited(rate_limited) => Some(block(
                "throttled",
                theme::text_subtle(),
                ("throttled", index),
                format!(
                    "provider {} — retrying in {}.{:03}s (attempt {})",
                    rate_limited
                        .status
                        .map(|s| format!("HTTP {s}"))
                        .unwrap_or_else(|| "transport failure".to_string()),
                    rate_limited.backoff_ms / 1000,
                    rate_limited.backoff_ms % 1000,
                    rate_limited.attempt,
                ),
            )),
            AgentFrameItem::Error(error) => Some(block(
                "error",
                theme::danger(),
                ("error", index),
                format!("{error:?}"),
            )),
            AgentFrameItem::Exited(reason) => Some(block(
                "exited",
                theme::text_muted(),
                ("exited", index),
                format!("{reason:?}"),
            )),
            AgentFrameItem::ToolCallStarted(_) => None,
            // Consumed by turn grouping (`turns::group_into_turns`) into
            // the turn's receipt line; never reaches this per-item path in
            // practice (see `AgentTranscript::render`'s span walk), kept only as a
            // defensive no-op.
            AgentFrameItem::TurnEnded { .. } | AgentFrameItem::ApprovalResolved(_) => None,
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
        occurrence_id: &OccurrenceId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let already_rendered = all_items[..index]
            .iter()
            .filter_map(item_execution)
            .any(|seen| seen == (call_id, occurrence_id));
        if already_rendered {
            return None;
        }
        match turns::build_tool_call_views(all_items)
            .into_iter()
            .find(|call| {
                item_execution(&all_items[call.request_index]) == Some((call_id, occurrence_id))
            }) {
            Some(call) => Some(self.render_tool_call_row(index, all_items, &call, false, cx)),
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
}

/// Execution identity used to correlate and deduplicate defensive orphan rows.
fn item_execution(item: &AgentFrameItem) -> Option<(&ToolCallId, &OccurrenceId)> {
    match item {
        AgentFrameItem::ToolCallRequested(request) => {
            Some((&request.call_id, &request.occurrence_id))
        }
        AgentFrameItem::ToolCallStarted(identity) => {
            Some((&identity.call_id, &identity.occurrence_id))
        }
        AgentFrameItem::ToolCallFinished(result) => Some((&result.call_id, &result.occurrence_id)),
        AgentFrameItem::ApprovalRequested(request) => {
            Some((&request.call_id, &request.occurrence_id))
        }
        _ => None,
    }
}

/// Escape plain transcript text for verbatim rendering through
/// `TextView::markdown` (the selectable-text pipeline the assistant messages
/// already use). Backslash-escaping every ASCII punctuation character keeps
/// GFM constructs -- headings, emphasis, fences, list markers, tables, HTML
/// -- from reinterpreting text that was never written as markdown (a user
/// prompt's `*`, an error line's `1.`); CommonMark resolves each escape back
/// to the literal character, so the painted text is unchanged. Newlines pass
/// through untouched and render as line breaks (the renderer pushes a `"\n"`
/// inline text node for its own `<br>`, so embedded newlines in a paragraph's
/// text take the same path).
fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        // `char::is_ascii_punctuation` is exactly CommonMark's escapable set
        // (`!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~`), so every escape it emits is
        // one the parser strips, and nothing else is touched.
        if ch.is_ascii_punctuation() {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_markdown;
    #[test]
    fn escape_markdown_neutralizes_gfm_construct_prefixes() {
        assert_eq!(escape_markdown("# not a heading"), "\\# not a heading");
        assert_eq!(escape_markdown("- not a list"), "\\- not a list");
        assert_eq!(escape_markdown("1. not ordered"), "1\\. not ordered");
        assert_eq!(escape_markdown("*not emphasis*"), "\\*not emphasis\\*");
        assert_eq!(escape_markdown("a|b"), "a\\|b");
        assert_eq!(escape_markdown("<script>"), "\\<script\\>");
    }

    #[test]
    fn escape_markdown_preserves_plain_text_and_newlines() {
        assert_eq!(
            escape_markdown("just words 日本語 🎉"),
            "just words 日本語 🎉"
        );
        assert_eq!(escape_markdown("two\nlines"), "two\nlines");
    }

    #[test]
    fn escape_markdown_keeps_backslashes_verbatim() {
        // A literal backslash must come back as a literal backslash, not be
        // consumed as an escape of the character that follows it.
        assert_eq!(escape_markdown("C:\\Users\\tmp"), "C\\:\\\\Users\\\\tmp");
        assert_eq!(escape_markdown("path\\*glob"), "path\\\\\\*glob");
    }
}
