//! Transcript entity: owns projected rows, expansion state and virtual-list effects.

mod changes;
mod items;
mod projection;
mod receipts;
mod rows;
mod scroll;

use super::super::{session::AgentSession, turns};
use super::composer::{AgentComposer, ComposerEvent};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use projection::{BurstPresentation, RowUpdate, TranscriptProjection, TranscriptRow};
pub(super) use receipts::render_stop_button;
use scroll::RunningTurnClock;
use std::{collections::HashSet, time::Duration};

/// The stable, expensive portion of an agent pane. Session updates project
/// into compact row descriptors here; `Render` constructs only visible rows.
pub(super) struct AgentTranscript {
    session: Entity<AgentSession>,
    scroller: Entity<MessageScrollerState>,
    projection: TranscriptProjection,
    running_turn_clock: Option<RunningTurnClock>,
    expanded_receipts: HashSet<usize>,
    /// Absolute request indices keep reused provider call IDs independent.
    expanded_rows: HashSet<usize>,
    changes_expanded: bool,
    /// Explicitly projected from `AgentComposer` events so row keyboard-target
    /// annotation never reaches across into the composer entity during render.
    composer_mode: turns::ComposerMode,
    _subscriptions: Vec<Subscription>,
}

impl AgentTranscript {
    pub(super) fn new(
        session: Entity<AgentSession>,
        composer_mode: turns::ComposerMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let projection = TranscriptProjection::from_items(&session.read(cx).frame.items);
        let scroller = cx.new(|cx| MessageScrollerState::new(projection.rows.len(), cx));

        let subscriptions =
            vec![
                cx.observe(&session, |transcript: &mut AgentTranscript, _, cx| {
                    transcript.sync_running_turn_clock(cx);
                    transcript.sync_transcript_rows(cx);
                    cx.notify();
                }),
            ];

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |transcript, cx| {
                if transcript.running_turn_clock.is_some() {
                    cx.notify();
                }
            });
            if alive.is_err() {
                return;
            }
        })
        .detach();

        Self {
            session,
            scroller,
            projection,
            running_turn_clock: None,
            expanded_receipts: HashSet::new(),
            expanded_rows: HashSet::new(),
            changes_expanded: false,
            composer_mode,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn bind_composer(
        &mut self,
        composer: &Entity<AgentComposer>,
        cx: &mut Context<Self>,
    ) {
        self._subscriptions.push(cx.subscribe(
            composer,
            |transcript, _, event: &ComposerEvent, cx| {
                let ComposerEvent::ModeChanged(mode) = event;
                if transcript.composer_mode != *mode {
                    transcript.composer_mode = mode.clone();
                    transcript
                        .scroller
                        .update(cx, |scroller, cx| scroller.remeasure(cx));
                    cx.notify();
                }
            },
        ));
    }

    pub(super) fn repin_to_tail(&mut self, cx: &mut Context<Self>) {
        // `scroll_to_end` re-engages tail following (message_scroller's own
        // contract), so this is the same explicit re-pin the composer's send
        // path has always performed.
        self.scroller
            .update(cx, |scroller, cx| scroller.scroll_to_end(cx));
        cx.notify();
    }
}

impl Render for AgentTranscript {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let changes_bar = self.render_changes_bar(&self.projection.changes, cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // The scroller owns tail-following, the scrollbar, the
                    // jump-to-latest button, and the bottom fade; the
                    // hand-rolled follow pill is gone with them.
                    .child({
                        let transcript_view = cx.entity();
                        MessageScroller::new(
                            "transcript-rows",
                            self.scroller.clone(),
                            move |row_index, window, cx| {
                                transcript_view.update(cx, |transcript, cx| {
                                    transcript.render_transcript_row(row_index, window, cx)
                                })
                            },
                        )
                        .with_content_style(StyleRefinement::default().pt_2())
                        .size_full()
                    }),
            )
            .when_some(changes_bar, |this, bar| this.child(bar))
    }
}

impl AgentTranscript {
    /// Apply the pure row update plan; GPUI owns measured heights and tail following.
    fn sync_transcript_rows(&mut self, cx: &mut Context<Self>) {
        let next = TranscriptProjection::from_items(&self.session.read(cx).frame.items);
        match next.update_from(&self.projection) {
            RowUpdate::Splice { old, new_count } => {
                self.scroller
                    .update(cx, |scroller, cx| scroller.splice(old, new_count, cx));
            }
            RowUpdate::Remeasure(range) => {
                self.scroller
                    .update(cx, |scroller, cx| scroller.remeasure_items(range, cx));
            }
            RowUpdate::None => {}
        }
        self.projection = next;
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
        let Some(row) = self.projection.rows.get(row_index).cloned() else {
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
                    BurstPresentation::Running => self.render_running_card(receipt_key, &items, cx),
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
}
