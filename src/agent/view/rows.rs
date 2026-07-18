//! Tool-call row rendering: the expanded receipt's per-call row list
//! (`render_expanded_receipt_rows`, `render_expandable_tool_call_row`),
//! the running card's own row (`render_tool_call_row`, including its
//! inline approval buttons and the `Waiting` proposal body), and the
//! shared per-tool expanded body (`render_tool_call_body`) reused by
//! both plus the failure log.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::Sizable as _;
use horizon_agent::contract::ToolCallId;
use horizon_agent::frame::AgentFrameItem;

use super::super::turns;
use crate::theme;

use super::AgentView;

impl AgentView {
    /// Toggles one expanded-receipt row's own body expansion.
    fn toggle_row(&mut self, call_id: ToolCallId, cx: &mut Context<Self>) {
        if !self.expanded_rows.remove(&call_id) {
            self.expanded_rows.insert(call_id);
        }
        cx.notify();
    }

    /// The expanded receipt's per-call row list (mock 6a's "opened
    /// receipt: in-place per-call row list, rows individually
    /// expandable"): a bordered, rounded container -- styled off the
    /// mock's own `border:1px solid #e4e4e7;border-radius:8px;
    /// overflow:hidden` panel -- holding one [`render_expandable_tool_call_row`]
    /// per call.
    pub(super) fn render_expanded_receipt_rows(
        &self,
        items: &[AgentFrameItem],
        tool_calls: &[turns::ToolCallView],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = div()
            .flex()
            .flex_col()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .overflow_hidden();
        let row_count = tool_calls.len();
        for (row_index, call) in tool_calls.iter().enumerate() {
            list = list.child(self.render_expandable_tool_call_row(
                items,
                call,
                row_index + 1 < row_count,
                cx,
            ));
        }
        list.into_any_element()
    }

    /// One expanded-receipt row: the same glyph + verb/target/summary
    /// line vocabulary as [`render_tool_call_row`] (the running card's
    /// non-expandable row), plus a leading `▸`/`▾` toggle and a click
    /// handler that reveals this call's [`turns::ToolCallBody`] beneath
    /// it (decision 3's "each row expands further individually"). The
    /// mock highlights an expanded row's header with a faint panel tint
    /// (`#fafafa`) -- `theme::surface_panel()` is that role here.
    /// `divider`'s border-bottom moves to the outer wrapper (rather than
    /// the header alone) so it still separates this row's body from the
    /// next row when expanded, mirroring the mock's own row grouping.
    fn render_expandable_tool_call_row(
        &self,
        items: &[AgentFrameItem],
        call: &turns::ToolCallView,
        divider: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded_rows.contains(&call.call_id);
        let arrow = if expanded { "▾" } else { "▸" };
        let (glyph, glyph_color) = tool_call_glyph(call);
        let text = tool_call_line_text(call);
        let call_id = call.call_id.clone();
        let row_id = ElementId::from(format!("receipt-row-{}", call.call_id.0));

        // The header's own background is `surface_panel` only while
        // expanded. Every text color
        // painted on it needs the UI-snap seam's contrast floor against
        // that surface, not just the app background
        // (`docs/theme-design.md`), so route through `readable_on` exactly
        // when the row will actually sit on it -- a no-op while collapsed.
        let snap = |color: Hsla| {
            if expanded {
                theme::readable_on(color, theme::surface_panel())
            } else {
                color
            }
        };

        let mut header = div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .cursor_pointer()
            .when(expanded, |this| this.bg(theme::surface_panel()))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.toggle_row(call_id.clone(), cx);
            }))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.0))
                    .text_color(theme::text_subtle())
                    .child(arrow),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(snap(glyph_color))
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(snap(theme::text_muted()))
                    .child(text),
            );
        // Surface the approval fact in a completed turn's expansion row
        // too (owner feedback 2026-07-13, round 3), same one-word
        // phrase as the running card -- but never buttons: history isn't
        // actionable.
        if let Some((phrase, color)) = approval_phrase(call.approval) {
            header = header.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(snap(color))
                    .child(phrase),
            );
        }

        let mut wrapper = div().flex().flex_col();
        if divider {
            wrapper = wrapper
                .border_b_1()
                .border_color(theme::text_subtle().alpha(0.3));
        }
        wrapper = wrapper.child(header);
        if expanded {
            if let Some(body) = turns::tool_call_body(items, &call.call_id) {
                wrapper = wrapper.child(
                    div()
                        .px_3()
                        .pb_2()
                        .child(self.render_tool_call_body(&call.call_id, &body)),
                );
            }
        }
        wrapper.into_any_element()
    }

    /// Renders one [`turns::ToolCallBody`] -- the reusable per-tool body
    /// machinery decision 3 asks for (fs.edit diff, fs.write preview,
    /// bash command+output, terse summaries, raw-JSON fallback), kept
    /// independent of the expansion-toggle wiring above so any other
    /// caller can call it directly: the failure-row log (stage F), the
    /// receipt's own expansion, and the `Waiting` row's auto-shown
    /// proposal body (row-centric v2, [`Self::render_waiting_proposal`])
    /// all reuse this one function. Every line-list body wraps in a
    /// height-bounded, internally scrollable container so one body can't
    /// swallow the transcript. `call_id` seeds the scrollable containers'
    /// element ids, stable across re-renders (GPUI's `overflow_y_scroll`
    /// needs a `Stateful` element -- i.e. one that's been given an id --
    /// to track scroll offset at all).
    fn render_tool_call_body(
        &self,
        call_id: &ToolCallId,
        body: &turns::ToolCallBody,
    ) -> AnyElement {
        match body {
            turns::ToolCallBody::Diff { lines, omitted } => {
                let mut container = div()
                    .id(ElementId::from(format!("body-diff-{}", call_id.0)))
                    .flex()
                    .flex_col()
                    .max_h(px(240.0))
                    .overflow_y_scroll();
                for line in lines {
                    container = container.child(render_diff_line(line));
                }
                if *omitted > 0 {
                    container = container.child(truncation_note(*omitted));
                }
                container.into_any_element()
            }
            turns::ToolCallBody::ContentPreview {
                label,
                lines,
                omitted,
            } => div()
                .flex()
                .flex_col()
                .gap_1()
                .py_1()
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme::text_subtle())
                        .child(label.clone()),
                )
                .child(render_line_body(
                    format!("body-content-{}", call_id.0),
                    lines,
                    *omitted,
                ))
                .into_any_element(),
            turns::ToolCallBody::Command {
                command,
                exit_code,
                lines,
                omitted,
            } => {
                let mut header_text = format!("$ {command}");
                if let Some(exit_code) = exit_code {
                    header_text.push_str(&format!("  · exit {exit_code}"));
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .py_1()
                    .child(
                        // The full command, wrapped rather than
                        // single-line-ellipsized (unlike the row's own
                        // `command_head`-based summary line): a proposal
                        // or failure log is exactly where a long or
                        // multi-line command needs to stay legible in
                        // full, not truncated a second time.
                        div()
                            .font(crate::terminal::resolved_font())
                            .text_size(px(11.5))
                            .text_color(theme::text_primary())
                            .min_w_0()
                            .whitespace_normal()
                            .child(header_text),
                    )
                    .child(render_line_body(
                        format!("body-command-{}", call_id.0),
                        lines,
                        *omitted,
                    ))
                    .into_any_element()
            }
            turns::ToolCallBody::Summary(text) => div()
                .py_1()
                .text_size(px(12.0))
                .text_color(theme::text_muted())
                .child(text.clone())
                .into_any_element(),
            turns::ToolCallBody::Raw { lines, omitted } => {
                render_line_body(format!("body-raw-{}", call_id.0), lines, *omitted)
            }
        }
    }

    /// One running-card row: status glyph (running/finished/error) +
    /// verb + target + result summary once finished — the base design's
    /// one-line tool-summary vocabulary (`docs/agent-output-ui-
    /// design.md` decision 2). `divider` draws the mock's subtle
    /// row-separator border-bottom (omitted on the last row, matching
    /// the mock). The verb/target/summary text is a single flex child
    /// with `min_w_0` + `overflow_hidden` + `text_ellipsis` +
    /// `whitespace_nowrap` so a long unbroken string (a deep file path,
    /// a long bash command head) truncates instead of pushing past the
    /// card's bounds — the glyph stays `flex_none` so it never shrinks.
    /// Every *finished* running-card row is click-expandable, success or
    /// failure (`docs/agent-output-ui-design.md` decision 2: "click
    /// expands the body ... collapsed is the default for every tool state
    /// including errors" -- stage F had narrowed this to failed calls
    /// only, closed 2026-07-13 as a deviation from decision 2, which never
    /// scoped the affordance to errors), reusing the same
    /// [`turns::tool_call_body`]/[`Self::render_tool_call_body`] machinery
    /// as the completed-turn receipt's own expandable rows
    /// (`render_expandable_tool_call_row`) -- `turns::running_row_expandable`
    /// is the shared pure predicate. A still-running row stays
    /// non-interactive: it has no result yet to show a body for.
    /// [`tool_call_glyph`]/[`tool_call_line_text`] factor out the
    /// verb/target/summary content this shares with
    /// [`render_expandable_tool_call_row`]'s expandable version.
    ///
    /// A `Waiting` approval renders inline at the row's right: small
    /// Approve/Deny buttons wired to this exact `call_id` (owner feedback
    /// 2026-07-13, round 3 -- integrating approval into the row it
    /// belongs to, replacing the standalone yellow box that gave no
    /// visible link back to its tool call), plus a subtle warning tint
    /// on the whole row so the eye finds it among a dozen other rows. A
    /// resolved approval (`Approved`/`Denied`) shows a short one-word
    /// phrase in that same area instead (`approval_phrase`) -- muted for
    /// approved, danger-colored for denied. `waiting` and a finished
    /// failure never coincide on the same call (a `Waiting` call has no
    /// result yet, so it can't be `is_error` yet either), so the two
    /// right-side affordances never compete for the same row. The
    /// keyboard/palette approve-tool-call/deny-tool-call commands and the
    /// control-plane path are untouched by any of this: they still
    /// dispatch by pending-queue order (`AgentSession::approve`/`deny`),
    /// independent of which row's buttons a pointer happens to click.
    ///
    /// Row-centric approval v2 (owner decision 2026-07-13, superseding
    /// stage E's composer banner): a `Waiting` row additionally shows
    /// exactly one of two things below its header. If it's the exact
    /// call `self.composer_mode` targets, its buttons carry a trailing
    /// "⏎ approve · esc deny" hint (`turns::is_keyboard_approval_target`)
    /// -- the composer's Enter/Esc still resolve *this* call, only its
    /// rendering moved from a banner onto the row. Every `Waiting` row,
    /// annotated or not, also auto-displays its proposal body
    /// (`Self::render_waiting_proposal`) -- unlike the failure log below,
    /// this isn't click-toggled, since a pending decision has exactly one
    /// thing to look at.
    pub(super) fn render_tool_call_row(
        &self,
        items: &[AgentFrameItem],
        call: &turns::ToolCallView,
        divider: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (glyph, glyph_color) = tool_call_glyph(call);
        let text = tool_call_line_text(call);
        let waiting = call.approval == turns::ApprovalState::Waiting;
        let expandable = turns::running_row_expandable(call);
        let expanded = expandable && self.expanded_rows.contains(&call.call_id);

        // The row's own background tint (warning while waiting on
        // approval, danger once resolved as an error -- see the `.bg(...)`
        // calls below) is the actual surface every text color in this row
        // sits on; contrast-snap against it rather than plain
        // `background`, the same treatment `render_expandable_tool_call_
        // row`'s own `snap` closure already gives its sibling row
        // (`docs/theme-design.md`'s 2026-07-15 contrast audit, item 5).
        // `text_subtle`-colored elements stay unsnapped throughout
        // (decorative by definition, exempt from the text floor -- the
        // sibling's arrow follows the same rule).
        let row_surface = if waiting {
            theme::tint_over_background(theme::warning(), 0.12)
        } else if call.is_error {
            theme::tint_over_background(theme::danger(), 0.1)
        } else {
            rgb(theme::background()).into()
        };
        let snap = |color: Hsla| theme::readable_on(color, row_surface);

        // Gives the row itself a stable, call_id-scoped identity -- the
        // same convention `render_expandable_tool_call_row`'s header
        // already uses (`.id(row_id)`), which this row lacked: only its
        // Approve/Deny `Button`s carried an explicit id, the row wrapping
        // them didn't. Owner feedback 2026-07-13 (round 4): the inline
        // buttons never registered a click at all, even for the live,
        // correctly-`Waiting` call -- an unstable/implicit-identity
        // ancestor in a row list that re-renders every second (the
        // elapsed-seconds ticker) is the most concrete, evidence-aligned
        // candidate found; this makes the row's identity as explicit and
        // stable as its buttons' own.
        let row_id = ElementId::from(format!("running-row-{}", call.call_id.0));
        let mut header = div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .when(waiting, |this| this.bg(theme::warning().alpha(0.12)))
            .when(call.is_error, |this| this.bg(theme::danger().alpha(0.1)))
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(snap(glyph_color))
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(snap(theme::text_muted()))
                    .child(text),
            );

        if waiting {
            let approve_id = call.call_id.clone();
            let deny_id = call.call_id.clone();
            let mut buttons = div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    Button::new(format!("row-approve-{}", call.call_id.0))
                        .primary()
                        .xsmall()
                        .label("Approve")
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.session.read(cx).approve(approve_id.clone());
                        })),
                )
                .child(
                    Button::new(format!("row-deny-{}", call.call_id.0))
                        .danger()
                        .xsmall()
                        .label("Deny")
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.session.read(cx).deny(deny_id.clone());
                        })),
                );
            // Row-centric approval v2 (owner decision 2026-07-13): only
            // the exact row `self.composer_mode` currently targets is
            // keyboard-operable, so only that row gets the hint --
            // derived from the mode itself (`is_keyboard_approval_target`),
            // never from queue position, so it can't lie about which
            // row Enter/Esc actually reach right now (see
            // `turns::ComposerMode`'s doc comment).
            if turns::is_keyboard_approval_target(&self.composer_mode, &call.call_id) {
                buttons = buttons.child(
                    div()
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::text_subtle())
                        .child("⏎ approve · esc deny"),
                );
            }
            header = header.child(buttons);
        } else if let Some((phrase, color)) = approval_phrase(call.approval) {
            header = header.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(snap(color))
                    .child(phrase),
            );
        }

        if expandable {
            // A trailing "show"/"hide" affordance -- stage F's original
            // "log" wording named a failure's output specifically, but
            // this row now expands to whatever per-tool body the call
            // has (a diff, a content preview, a command+output block, or
            // a summary, same as the receipt's own expansion), so a
            // generic show/hide reads correctly for every finished call,
            // not just a failed one. Danger-tinted for a failure (matching
            // the row's own error tint above); a neutral, muted tint
            // otherwise -- there's nothing wrong to flag on a success row.
            // The whole row is still the click target, matching
            // `render_expandable_tool_call_row`'s convention.
            let call_id = call.call_id.clone();
            let label_color = if call.is_error {
                snap(theme::danger())
            } else {
                theme::text_subtle()
            };
            header = header
                .cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.toggle_row(call_id.clone(), cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(label_color)
                        .child(if expanded { "hide" } else { "show" }),
                );
        }

        let mut wrapper = div().flex().flex_col().child(header);
        if waiting {
            // Decision 4: "the pending diff/command renders neutrally
            // ... labeled 'proposal — not applied'" -- shown
            // automatically for every `Waiting` row, not click-toggled
            // like the failure log below, since there is exactly one
            // thing to look at before deciding. `waiting` and `expanded`
            // never coincide (a `Waiting` call has no result yet, so it
            // can't be a finished failure either), so this and the
            // `expanded` branch below are mutually exclusive.
            if let Some(body) = turns::tool_call_body(items, &call.call_id) {
                wrapper = wrapper.child(self.render_waiting_proposal(&call.call_id, &body));
            }
        } else if expanded {
            if let Some(body) = turns::tool_call_body(items, &call.call_id) {
                wrapper = wrapper.child(
                    div()
                        .px_3()
                        .pb_2()
                        .child(self.render_tool_call_body(&call.call_id, &body)),
                );
            }
        }
        if divider {
            wrapper = wrapper
                .border_b_1()
                .border_color(theme::text_subtle().alpha(0.3));
        }
        wrapper.into_any_element()
    }

    /// A `Waiting` row's proposal body (decision 4, row-centric v2): the
    /// same [`turns::ToolCallBody`] the failure-row log and receipt
    /// expansion already reuse (fs.edit's diff, fs.write's content
    /// preview, bash's full command -- never the row's own 32-char
    /// `command_head` -- and the terse/raw fallbacks for everything
    /// else), labeled with a small muted tag so it reads as informational
    /// rather than a fact about what already happened.
    fn render_waiting_proposal(
        &self,
        call_id: &ToolCallId,
        body: &turns::ToolCallBody,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .pb_2()
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_subtle())
                    .child("proposal — not applied"),
            )
            .child(self.render_tool_call_body(call_id, body))
            .into_any_element()
    }
}

/// The status glyph (running/finished/error) shared by the running
/// card's row (`render_tool_call_row`) and the expanded receipt's
/// expandable row (`render_expandable_tool_call_row`).
fn tool_call_glyph(call: &turns::ToolCallView) -> (&'static str, Hsla) {
    if !call.finished {
        ("●", theme::accent())
    } else if call.is_error {
        ("✗", theme::danger())
    } else {
        ("✓", theme::success())
    }
}

/// The verb + target + result-summary line text shared by the same two
/// row renderers as [`tool_call_glyph`].
fn tool_call_line_text(call: &turns::ToolCallView) -> String {
    let mut text = call.verb.clone();
    if let Some(target) = &call.target {
        text.push(' ');
        text.push_str(target);
    }
    if call.finished {
        if let Some(summary) = &call.result_summary {
            text.push_str(" · ");
            text.push_str(summary);
        }
    }
    text
}

/// A resolved approval's one-word phrase (owner feedback 2026-07-13,
/// round 3): shown in place of buttons once a `Waiting` row's decision
/// lands, and in a completed turn's expanded receipt row (history is not
/// actionable there, so it's the phrase or nothing -- never buttons).
/// `None` for `ApprovalState::None` (never needed approval) and
/// `ApprovalState::Waiting` (that state gets buttons in the running
/// card, or -- in a receipt, which only shows resolved calls in the
/// normal case -- nothing extra at all).
fn approval_phrase(approval: turns::ApprovalState) -> Option<(&'static str, Hsla)> {
    match approval {
        turns::ApprovalState::Approved => Some(("approved", theme::text_muted())),
        turns::ApprovalState::Denied => Some(("denied", theme::danger())),
        turns::ApprovalState::None | turns::ApprovalState::Waiting => None,
    }
}

/// One reconstructed diff line (decision 4): the line background carries
/// the change (`theme::diff_added_surface`/`diff_removed_surface`), the
/// sign column colors separately (`diff_added_text`/`diff_removed_text`);
/// a `Context` line (the common prefix/suffix `reconstruct_line_diff`
/// trimmed) paints with neither, since the reconstruction has no access
/// to the file's real line numbers to show instead.
fn render_diff_line(line: &turns::DiffLine) -> AnyElement {
    let (surface, sign, sign_color) = match line.kind {
        turns::DiffLineKind::Context => (None, " ", theme::text_subtle()),
        turns::DiffLineKind::Added => (
            Some(theme::diff_added_surface()),
            "+",
            theme::diff_added_text(),
        ),
        turns::DiffLineKind::Removed => (
            Some(theme::diff_removed_surface()),
            "−",
            theme::diff_removed_text(),
        ),
    };

    let mut row = div().flex().flex_row().gap_2().px_2();
    if let Some(surface) = surface {
        row = row.bg(surface);
    }
    row.child(
        div()
            .flex_none()
            .w(px(14.0))
            .font(crate::terminal::resolved_font())
            .text_size(px(11.5))
            .text_color(sign_color)
            .child(sign),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .font(crate::terminal::resolved_font())
            .text_size(px(11.5))
            .text_color(theme::text_muted())
            .child(line.text.clone()),
    )
    .into_any_element()
}

/// A preformatted-text line body (fs.write's content preview, bash's
/// captured output, the raw-JSON fallback): one row per line, each
/// truncating rather than wrapping (`min_w_0` + `overflow_hidden` +
/// `text_ellipsis` + `whitespace_nowrap` — the same C.1 overflow idiom
/// `render_tool_call_row` uses) so a long line can't push past the
/// card's bounds. Wrapped in a height-bounded, internally scrollable
/// container so a large body can't swallow the transcript.
fn render_line_body(id: impl Into<ElementId>, lines: &[String], omitted: usize) -> AnyElement {
    let mut container = div()
        .id(id)
        .flex()
        .flex_col()
        .max_h(px(240.0))
        .overflow_y_scroll();
    for line in lines {
        container = container.child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .font(crate::terminal::resolved_font())
                .text_size(px(11.5))
                .text_color(theme::text_muted())
                .child(line.clone()),
        );
    }
    if omitted > 0 {
        container = container.child(truncation_note(omitted));
    }
    container.into_any_element()
}

/// The note appended when a body's line cap trims trailing content
/// (content previews/raw JSON: trailing lines past the cap; bash output:
/// leading lines before the kept tail — either way, "omitted" count of
/// lines not shown).
fn truncation_note(omitted: usize) -> AnyElement {
    div()
        .text_size(px(10.5))
        .text_color(theme::text_subtle())
        .child(format!("… {omitted} more line(s) trimmed"))
        .into_any_element()
}
