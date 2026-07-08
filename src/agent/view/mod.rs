use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::agent::contract::ToolCallId;
use crate::agent::frame::AgentFrame;
use crate::ui::fonts::font_family;
use crate::ui::theme;
use floem::peniko::kurbo::Point;
use floem::prelude::*;
use floem::reactive::{create_effect, create_memo, untrack};

mod approval;
mod changes;
mod diff;
mod follow_scroll;
mod labels;
mod markdown;
mod style;
mod tool_header;
mod tool_view;
mod transcript;

pub(crate) use approval::{
    awaiting_call, gate_pending_approval, next_agent_pane_focus, next_answered_call,
    AgentPaneFocus, ApprovalController,
};
use follow_scroll::{classify_scroll, next_follow_state, FollowState};
use labels::{block_label, shows_label};
use markdown::{markdown_lines, MarkdownLine, MarkdownLineKind};
use style::{block_colors, block_max_width, block_text_color};
use transcript::{
    compute_transcript_window, diff_block_content, is_thinking_streaming, latest_user_block_id,
    show_turn_end_rule, starts_new_turn, status_block, transcript_blocks, BlockKind, ToolBlock,
    TranscriptBlock, TranscriptTone, TranscriptWindow,
};

/// How much taller the transcript's measured content height must get,
/// since the previous `on_scroll` call, to count as "the content grew"
/// (`follow_scroll::ScrollCause::ContentGrew`) rather than layout-rounding
/// noise on an unchanged document.
const CONTENT_GROWTH_EPSILON: f64 = 0.5;

pub(crate) fn agent_frame_view(
    frame: impl Fn() -> AgentFrame + Copy + 'static,
    visible: impl Fn() -> bool + Copy + 'static,
    approval: ApprovalController,
) -> impl IntoView {
    let follow = RwSignal::new(FollowState::Following);
    let viewport = RwSignal::new(None::<floem::peniko::kurbo::Rect>);
    // `on_scroll`'s "did the content grow" input -- see `follow_scroll`'s
    // `ScrollCause` doc comment for why this, not a "was this our own
    // jump?" flag, is what guards against misreading a streaming height
    // change as the user scrolling away.
    let last_content_height = RwSignal::new(0.0_f64);
    // The last rendered block's id at the moment `follow` most recently
    // went `Detached` -- what the return pill's "new output arrived while
    // you were looking away" label (part b of this slice) compares the
    // live last-block id against.
    let detached_since_block = RwSignal::new(None::<usize>);
    // Every currently-mounted block's own top-level `ViewId`, registered by
    // `transcript_block_view` as each block is first built. The "jump to
    // latest user message" pill resolves a block *id* via
    // `latest_user_block_id`, then looks it up here to get something
    // `.scroll_to_view` can actually target. A plain `RefCell`, not a
    // signal: nothing needs to react to this map changing, only to read it
    // at click time. Stale entries for blocks the 200-block window has
    // since trimmed are harmless (see this slice's report on window-trim
    // scroll position).
    let block_view_ids: Rc<RefCell<HashMap<usize, floem::ViewId>>> =
        Rc::new(RefCell::new(HashMap::new()));
    // Where the "jump to latest user message" pill wants the viewport,
    // consumed by `.scroll_to_view` below.
    let jump_to_view = RwSignal::new(None::<floem::ViewId>);

    // Recomputed only when `transcript_revision` actually changes (see
    // `compute_transcript_window`), so a reactive re-run caused by some
    // *other* pane's agent frame updating the shared `Frames` signal is a
    // cheap no-op here instead of re-walking this session's whole item log.
    let window = create_memo(move |previous: Option<&TranscriptWindow>| {
        compute_transcript_window(&frame(), previous)
    });

    // Two coarse structural-revision proxies for `frame`, shared by every
    // per-block reactive closure below that only needs to know "did the
    // item log change" / "did the turn's in-flight status change" rather
    // than the whole frame -- the same intermediate-memo pattern `window`
    // above already uses, applied to `current_tool_block`'s and
    // `is_thinking_streaming`'s per-block call sites (`transcript_block_view`)
    // instead of the transcript's own window/revision. `frame.items` never
    // shrinks and text deltas coalesce into an existing item in place
    // (`apply_agent_event_to_frame`), so pure token streaming leaves
    // `items.len()` unchanged; `frame.state` only changes via a separate
    // `StateChanged` event that doesn't touch `items` at all, hence the
    // second, independent proxy.
    let items_revision = create_memo(move |_| {
        crate::profiling::timed("transcript.items_revision", || frame().items.len())
    });
    let turn_in_flight = create_memo(move |_| {
        crate::profiling::timed("transcript.turn_in_flight", || frame().is_turn_in_flight())
    });

    // A full-fidelity (not coarsened to a bool) proxy for `frame.state`,
    // `PartialEq`-gated like every memo above -- what the status indicator
    // below needs instead of `turn_in_flight`'s collapsed boolean, since its
    // *text* differs across every terminal state (`Cancelled`/`Failed`/
    // `Terminated`/...), not just "in flight or not".
    let session_state =
        create_memo(move |_| crate::profiling::timed("transcript.session_state", || frame().state));

    // The transcript's ephemeral status line ("Agent is replying...",
    // "Agent failed", ...) -- leg 1 hardening (`docs/agent-ui-performance-
    // design.md`). This used to be a synthetic block `transcript_blocks`
    // appended, which under leg 1 would be rendered through a per-block
    // content signal seeded once at mount and refreshed only by
    // `diff_block_content`'s item-keyed diff -- but this line has no
    // backing `frame.items` entry, so a state-only transition with no new
    // item (e.g. `Running -> Failed`, `ToolRunning -> Completed`) would
    // never reach it, freezing stale text. Recomputed only on
    // `session_state`/`items_revision` changes (real state transitions or
    // new items), not per streamed token -- `frame`/`transcript_blocks` are
    // read `untrack`ed inside, the same shape `is_thinking_streaming`'s
    // effect above already uses, so this memo doesn't itself become a new
    // per-token dependency of anything reading it.
    let status_text = create_memo(move |_| {
        let state = session_state.get();
        items_revision.get();
        crate::profiling::timed("transcript.status_text", || {
            untrack(|| {
                let blocks = transcript_blocks(&frame());
                status_block(state, 0, blocks.last()).map(|block| match block.kind {
                    BlockKind::Text { text, .. } => text,
                    BlockKind::Tool(_) => String::new(),
                })
            })
        })
    });

    // --- Leg 1 spike: per-block content signals -------------------------
    //
    // `docs/agent-ui-performance-design.md` leg 1's spike: rather than every
    // mounted block's own view re-deriving its live content by reading the
    // whole `frame` directly (the diagnosed hot path for streaming text and
    // tool blocks -- every block re-running on every streamed token,
    // session-wide, because `dyn_stack` never rebuilds a view for an
    // unchanged `(id, tone)` key), each block gets its own content signal,
    // created once by `transcript_block_view` when the block is first built
    // (`get_or_create_text_signal`/`get_or_create_tool_signal`) and kept
    // live only by the one bridge effect below -- the sole place, for these
    // two block kinds, that still reads the raw `frame` signal directly.
    let text_content: Rc<RefCell<HashMap<usize, RwSignal<String>>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let tool_content: Rc<RefCell<HashMap<usize, RwSignal<ToolBlock>>>> =
        Rc::new(RefCell::new(HashMap::new()));
    // The bridge's own `call_id -> tool block id` registry (built up
    // incrementally by `diff_block_content`, see its doc comment) -- shared
    // with the trim effect below purely so a call whose block has scrolled
    // out of the window doesn't linger here forever either.
    let call_id_to_block: Rc<RefCell<HashMap<ToolCallId, usize>>> =
        Rc::new(RefCell::new(HashMap::new()));

    {
        let text_content = text_content.clone();
        let tool_content = tool_content.clone();
        let call_id_to_block = call_id_to_block.clone();
        // Bridge-internal bookkeeping, not shared with any view beyond the
        // trim effect above: previous fire's `frame.items.len()`, what
        // `diff_block_content` diffs against. A plain `Cell`, not a signal
        // -- nothing needs to react to it changing, only this effect needs
        // to read it, and only from inside itself.
        let previous_items_len = Cell::new(0usize);
        create_effect(move |_| {
            let frame_snapshot = frame();
            crate::profiling::timed("transcript.block_signal_bridge", || {
                let diff = diff_block_content(
                    &frame_snapshot,
                    previous_items_len.get(),
                    &mut call_id_to_block.borrow_mut(),
                );
                previous_items_len.set(frame_snapshot.items.len());

                // Only applied to signals that already exist: a block whose
                // view `dyn_stack` hasn't built yet seeds itself fresh from
                // `window`'s own snapshot when it *is* built (see
                // `transcript_block_view`), so there's nothing for this
                // bridge to create -- only already-mounted blocks need a
                // live update. `set_if_changed` (not a plain `.set`) skips
                // the write when the derived value didn't actually change,
                // so a frame update unrelated to this specific block (e.g.
                // another block's token, or a `state`-only change) doesn't
                // wake this block's view.
                let text_content = text_content.borrow();
                for (id, text) in diff.text {
                    if let Some(signal) = text_content.get(&id) {
                        set_if_changed(*signal, text);
                    }
                }
                drop(text_content);

                let tool_content = tool_content.borrow();
                for (id, tool) in diff.tool {
                    if let Some(signal) = tool_content.get(&id) {
                        set_if_changed(*signal, tool);
                    }
                }
            });
        });
    }

    // Trims per-block content signals (and the bridge's own
    // `call_id_to_block`) for blocks the transcript window
    // (`transcript::TRANSCRIPT_WINDOW`) has already dropped -- otherwise
    // these maps would grow without bound over a long session, exactly the
    // class of bug this whole initiative exists to avoid. Gated on
    // `omitted`'s *value* (the same `create_memo`-PartialEq-gates-
    // notification trick `items_revision`/`turn_in_flight` use above)
    // rather than firing on every token the way `window` itself does
    // (`compute_transcript_window`'s revision incorporates growing text
    // length, by design, so `.scroll_to`'s autoscroll tracks smoothly --
    // out of scope for this spike, see the design doc): trimming only needs
    // to run when a block actually falls out of the trailing window, not
    // every time an already-visible block's text grows.
    let omitted = create_memo(move |_| window.with(|window| window.omitted));
    {
        let text_content = text_content.clone();
        let tool_content = tool_content.clone();
        let call_id_to_block = call_id_to_block.clone();
        create_effect(move |_| {
            omitted.get();
            untrack(|| {
                let Some(oldest_visible_id) =
                    window.with(|window| window.blocks.first().map(|block| block.id))
                else {
                    return;
                };
                text_content
                    .borrow_mut()
                    .retain(|id, _| *id >= oldest_visible_id);
                tool_content
                    .borrow_mut()
                    .retain(|id, _| *id >= oldest_visible_id);
                call_id_to_block
                    .borrow_mut()
                    .retain(|_, block_id| *block_id >= oldest_visible_id);
            });
        });
    }

    // Forced scroll-in (`docs/agent-output-ui-design.md` decision 8): the
    // instant a new tool call becomes the oldest pending approval, jump the
    // viewport to its block regardless of `follow`'s current state --
    // mirrors the "jump to latest user message" pill's own `.scroll_to_view`
    // use just below, but fired automatically rather than by a click.
    // Deliberately doesn't touch `follow`/`detached_since_block` itself: the
    // next `on_scroll` this jump triggers classifies the resulting position
    // through the ordinary slice-3 state machine, same as any other scroll.
    let approval_for_scroll = approval.clone();
    let block_view_ids_for_scroll = block_view_ids.clone();
    create_effect(
        move |previous: Option<Option<crate::agent::contract::ToolCallId>>| {
            let pending = approval_for_scroll.pending_call_id();
            if let Some(previous) = previous {
                if previous != pending {
                    if let Some(call_id) = &pending {
                        let target_block = window.with(|window| {
                            window.blocks.iter().find_map(|block| match &block.kind {
                                BlockKind::Tool(tool) if tool.call_id.as_ref() == Some(call_id) => {
                                    Some(block.id)
                                }
                                _ => None,
                            })
                        });
                        if let Some(view_id) = target_block.and_then(|block_id| {
                            block_view_ids_for_scroll.borrow().get(&block_id).copied()
                        }) {
                            jump_to_view.set(Some(view_id));
                        }
                    }
                }
            }
            pending
        },
    );

    let block_ids_for_blocks = block_view_ids.clone();
    let content = v_stack((
        label(move || omitted_summary(window.with(|window| window.omitted))).style(move |s| {
            if window.with(|window| window.omitted) == 0 {
                return s.hide();
            }

            s.width_full().font_size(11).color(theme::text_muted())
        }),
        dyn_stack(
            move || window.with(|window| window.blocks.clone()),
            move |block| (block.id, block.tone),
            move |block| {
                transcript_block_view(
                    block,
                    frame,
                    items_revision,
                    turn_in_flight,
                    block_ids_for_blocks.clone(),
                    text_content.clone(),
                    tool_content.clone(),
                    approval.clone(),
                )
            },
        )
        // Dense within a turn (decision 6): whitespace belongs at turn
        // boundaries only, which `turn_boundary_rule` supplies per-block via
        // its own margin, not this shared gap.
        .style(|s| s.width_full().flex_col().gap(4)),
        status_indicator_view(status_text),
        turn_end_rule_view(window, frame),
    ))
    .style(|s| s.width_full().flex_col().gap(4).padding(8));
    let content_id = content.id();

    let transcript_scroll = scroll(content)
        .on_scroll(move |rect| {
            viewport.set(Some(rect));

            let height = content_height(content_id);
            let content_grew =
                height > last_content_height.get_untracked() + CONTENT_GROWTH_EPSILON;
            last_content_height.set(height);

            let at_bottom = viewport_is_at_bottom(rect, height);
            let cause = classify_scroll(at_bottom, content_grew);
            let previous = follow.get_untracked();
            let next = next_follow_state(previous, cause);
            if previous == FollowState::Following && next == FollowState::Detached {
                detached_since_block
                    .set(window.with(|window| window.blocks.last().map(|block| block.id)));
            }
            follow.set(next);
        })
        .scroll_to(move || {
            if !visible() || follow.get() != FollowState::Following {
                return None;
            }

            // Track the memoized revision (a `usize` copy) instead of
            // calling `frame()` directly: this used to clone the whole
            // `AgentFrame` on every scroll re-check just to derive the same
            // revision the transcript memo above already computed.
            let _ = window.with(|window| window.revision);
            Some(Point::new(0.0, 1_000_000_000.0))
        })
        .scroll_to_view(move || jump_to_view.get())
        .scroll_style(|s| s.shrink_to_fit().overflow_clip(true))
        .style(|s| s.size_full());

    // Cloned before `follow_scroll_pills`/`changes_bar_view` each consume
    // their own copy of the same registry -- both resolve a block id to a
    // `ViewId` through it (the "jump to latest user message" pill and a
    // Changes row's own jump, respectively).
    let block_view_ids_for_changes = block_view_ids.clone();

    let transcript_area = stack((
        transcript_scroll,
        follow_scroll_pills(
            frame,
            window,
            follow,
            detached_since_block,
            block_view_ids,
            jump_to_view,
        ),
    ))
    .style(|s| {
        s.width_full()
            .flex_basis(0.0)
            .flex_grow(1.0)
            .min_height(0.0)
    });

    v_stack((
        transcript_area,
        // The Changes overview bar (`docs/agent-output-ui-design.md`
        // decision 9) -- a sibling below the scrollable transcript, above
        // the composer, so it never scrolls out of view itself and never
        // steals space from the transcript except its own (collapsed by
        // default) height.
        changes_bar_view(
            frame,
            items_revision,
            window,
            follow,
            detached_since_block,
            block_view_ids_for_changes,
            jump_to_view,
        ),
    ))
    .style(move |s| {
        if !visible() {
            return s.hide();
        }

        s.width_full()
            .flex_basis(0.0)
            .flex_grow(1.0)
            .min_height(0.0)
            .background(theme::terminal_background())
    })
}

/// How tall the Changes bar's expanded file list can grow before it starts
/// scrolling internally -- keeps a session with many touched files from
/// pushing the composer off screen (`docs/agent-output-ui-design.md`
/// decision 9's "展開リストは高さ上限+スクロール").
const CHANGES_LIST_MAX_HEIGHT: f64 = 200.0;

/// The Changes overview bar: collapsed by default, showing only when the
/// session has at least one successful `fs.edit`/`fs.write` call
/// (`changes::session_changes`). Collapsed reads `Changes · N files · +A
/// −B` (Zed's Edits-bar convention, `docs/research/agent-ui.md`'s Part
/// 4-5/5-2); expanded lists one row per file, each clickable to jump the
/// transcript to that file's most recent edit -- a display jump, not a
/// `CommandId` (`docs/ux-principles.md`'s "Not commands" list; same
/// reasoning as the follow-scroll pills' own jump just above).
fn changes_bar_view(
    frame: impl Fn() -> AgentFrame + Copy + 'static,
    items_revision: floem::reactive::Memo<usize>,
    window: floem::reactive::Memo<TranscriptWindow>,
    follow: RwSignal<FollowState>,
    detached_since_block: RwSignal<Option<usize>>,
    block_view_ids: Rc<RefCell<HashMap<usize, floem::ViewId>>>,
    jump_to_view: RwSignal<Option<floem::ViewId>>,
) -> impl IntoView {
    let expanded = RwSignal::new(false);
    // `items_revision` (`agent_frame_view`'s shared structural-revision
    // proxy for `frame.items`) versus `session_changes`'s full item-log walk
    // below. `apply_agent_event_to_frame` (`crates/horizon-agent/src/
    // frame.rs`) never removes items, and text deltas / `MessageCommitted`
    // coalesce into an existing item in place (`text.push_str`, or an
    // in-place replace), so pure token streaming leaves `items.len()`
    // unchanged. `ToolCallRequested` can likewise supersede a pending
    // `ToolCallPreparing` item in place -- but the matching
    // `ToolCallFinished` that actually makes `session_changes` see a new
    // file change is always a plain push, never coalesced, so it's
    // guaranteed to bump this count whenever `session_changes`'s output
    // could have changed.
    //
    // Derived straight from `frame.items` (see `changes::session_changes`'s
    // doc comment), so -- like `latest_user_block_id` -- this is immune to
    // the transcript window's 200-block trailing trim: a change made far
    // enough back to have scrolled out of the rendered window still counts.
    //
    // Tracks `items_revision`'s *value* rather than `frame()` directly, and
    // reads `frame()` itself `untrack`ed so this closure isn't *also*
    // subscribed to the raw frame signal -- otherwise this memo would still
    // re-walk the whole item log on every streamed text delta (the
    // per-token cost that made the Changes bar a performance regression),
    // even though its own recomputation is skipped whenever the value
    // doesn't change. Chaining through `items_revision` means the O(N) walk
    // below only runs when the structural revision above actually changed.
    let changes = create_memo(move |_| {
        items_revision.get();
        crate::profiling::timed("transcript.session_changes", || {
            untrack(move || changes::session_changes(&frame()))
        })
    });

    let header = h_stack((
        label(move || {
            if expanded.get() {
                "\u{25be}"
            } else {
                "\u{25b8}"
            }
            .to_string()
        })
        .style(|s| s.width(12).font_size(10).color(theme::text_muted())),
        label(move || {
            changes.with(|changes| changes_summary_text(changes::changes_total(changes)))
        })
        .style(|s| {
            s.flex_basis(0.0)
                .flex_grow(1.0)
                .min_width(0.0)
                .font_size(11)
                .color(theme::text_muted())
        }),
    ))
    .on_click_stop(move |_| expanded.update(|value| *value = !*value))
    .style(|s| {
        s.width_full()
            .items_center()
            .gap(6)
            .padding_horiz(10)
            .padding_vert(6)
            .border_top(1.0)
            .border_color(theme::border_subtle())
            .background(theme::surface_chrome())
    });

    let file_list = scroll(
        dyn_stack(
            move || changes.get(),
            |change| change.path.clone(),
            move |change| {
                changes_file_row_view(
                    change,
                    window,
                    follow,
                    detached_since_block,
                    block_view_ids.clone(),
                    jump_to_view,
                )
            },
        )
        .style(|s| s.width_full().flex_col()),
    )
    .style(move |s| {
        let s = s.width_full().max_height(CHANGES_LIST_MAX_HEIGHT);
        if expanded.get() {
            s
        } else {
            s.hide()
        }
    });

    v_stack((header, file_list)).style(move |s| {
        if changes.with(|changes| changes.is_empty()) {
            s.hide()
        } else {
            s.width_full().flex_col()
        }
    })
}

/// One file row in the expanded Changes list: path, that file's own
/// `+added −removed`, and how many edit/write calls touched it.
fn changes_file_row_view(
    change: changes::FileChange,
    window: floem::reactive::Memo<TranscriptWindow>,
    follow: RwSignal<FollowState>,
    detached_since_block: RwSignal<Option<usize>>,
    block_view_ids: Rc<RefCell<HashMap<usize, floem::ViewId>>>,
    jump_to_view: RwSignal<Option<floem::ViewId>>,
) -> impl IntoView {
    let block_id = change.last_block_id;
    let path = change.path.clone();
    let added = change.added;
    let removed = change.removed;
    let edits = change.edits;

    h_stack((
        label(move || path.clone()).style(move |s| {
            s.flex_basis(0.0)
                .flex_grow(1.0)
                .min_width(0.0)
                .font_family(font_family().to_string())
                .font_size(12)
                .color(theme::text_primary())
        }),
        label(move || format!("+{added}"))
            .style(|s| s.font_size(11).color(theme::diff_added_text())),
        label(move || format!("\u{2212}{removed}"))
            .style(|s| s.font_size(11).color(theme::diff_removed_text())),
        label(move || format!("{edits} edit{}", if edits == 1 { "" } else { "s" }))
            .style(|s| s.font_size(11).color(theme::text_muted())),
    ))
    .on_click_stop(move |_| {
        let Some(view_id) = block_view_ids.borrow().get(&block_id).copied() else {
            return;
        };
        jump_to_view.set(Some(view_id));
        // A deliberate look-back at an earlier tool call -- same posture as
        // the "jump to latest user message" pill above (decision 7): forced
        // regardless of where the jump lands, so the return pill reliably
        // reappears even if this file's last edit happens to be near the
        // current bottom.
        detached_since_block.set(window.with(|window| window.blocks.last().map(|block| block.id)));
        follow.set(FollowState::Detached);
    })
    .style(|s| {
        s.width_full()
            .items_center()
            .gap(10)
            .padding_horiz(10)
            .padding_vert(4)
    })
}

/// The Changes bar's collapsed-state summary text -- Zed's Edits-bar
/// convention (`docs/research/agent-ui.md`'s Part 4-5/Part 5-2: "Zed の
/// Edits バーは `N files +add/-del`").
fn changes_summary_text(total: changes::ChangesTotal) -> String {
    format!(
        "Changes \u{b7} {} file{} \u{b7} +{} \u{2212}{}",
        total.files,
        if total.files == 1 { "" } else { "s" },
        total.added,
        total.removed,
    )
}

/// The follow-scroll return pills (`docs/agent-output-ui-design.md`
/// decision 7: "a return pill, and a jump to the latest user message"),
/// overlaid on the transcript's bottom-right corner and shown only while
/// `follow` is `Detached`. Scrolling -- including where these buttons send
/// it -- is continuous/positional input and pure display state, not an
/// app-level operation (`docs/ux-principles.md`'s "Not commands" list), so
/// these stay plain click handlers rather than `CommandId`s.
fn follow_scroll_pills(
    frame: impl Fn() -> AgentFrame + Copy + 'static,
    window: floem::reactive::Memo<TranscriptWindow>,
    follow: RwSignal<FollowState>,
    detached_since_block: RwSignal<Option<usize>>,
    block_view_ids: Rc<RefCell<HashMap<usize, floem::ViewId>>>,
    jump_to_view: RwSignal<Option<floem::ViewId>>,
) -> impl IntoView {
    let has_unread = move || {
        detached_since_block.get().is_some_and(|since| {
            window.with(|window| window.blocks.last().map(|block| block.id)) != Some(since)
        })
    };

    let return_pill = pill_button(
        move || {
            if has_unread() {
                "\u{2193} New output".to_string()
            } else {
                "\u{2193} Latest".to_string()
            }
        },
        move || {
            detached_since_block.set(None);
            follow.set(FollowState::Following);
        },
    );

    let jump_to_user_pill = pill_button(
        || "Your last message".to_string(),
        move || {
            let Some(block_id) = crate::profiling::timed("transcript.latest_user_block_id", || {
                latest_user_block_id(&frame())
            }) else {
                return;
            };
            let Some(view_id) = block_view_ids.borrow().get(&block_id).copied() else {
                return;
            };

            jump_to_view.set(Some(view_id));
            // A deliberate look-back at earlier context, not a resumed
            // follow (decision 7) -- forced regardless of where the target
            // happens to land, rather than relying on the next `on_scroll`
            // call to infer it.
            detached_since_block
                .set(window.with(|window| window.blocks.last().map(|block| block.id)));
            follow.set(FollowState::Detached);
        },
    );

    h_stack((return_pill, jump_to_user_pill)).style(move |s| {
        let s = s.absolute().inset_bottom(16.0).inset_right(16.0).gap(8);
        if follow.get() == FollowState::Following {
            s.hide()
        } else {
            s
        }
    })
}

fn pill_button(
    text: impl Fn() -> String + 'static,
    on_click: impl Fn() + 'static,
) -> impl IntoView {
    label(text).on_click_stop(move |_| on_click()).style(|s| {
        s.padding_horiz(10)
            .padding_vert(6)
            .font_size(11)
            .color(theme::text_primary())
            .background(theme::surface_raised())
            .border(1.0)
            .border_color(theme::accent())
    })
}

fn omitted_summary(omitted: usize) -> String {
    format!(
        "{omitted} earlier item{} hidden",
        if omitted == 1 { "" } else { "s" }
    )
}

fn content_height(id: floem::ViewId) -> f64 {
    id.get_layout()
        .map(|layout| layout.size.height as f64)
        .unwrap_or(0.0)
}

fn viewport_is_at_bottom(viewport: floem::peniko::kurbo::Rect, content_height: f64) -> bool {
    content_height <= 0.0 || viewport.y1 >= content_height - 2.0
}

/// Returns `block_id`'s text content signal, creating it (seeded with
/// `initial`) the first time this block is built. Leg 1 spike support --
/// see `agent_frame_view`'s content-signal doc comment.
fn get_or_create_text_signal(
    text_content: &Rc<RefCell<HashMap<usize, RwSignal<String>>>>,
    block_id: usize,
    initial: &str,
) -> RwSignal<String> {
    *text_content
        .borrow_mut()
        .entry(block_id)
        .or_insert_with(|| RwSignal::new(initial.to_string()))
}

/// Returns `block_id`'s tool content signal, creating it (seeded with
/// `initial`) the first time this block is built. Leg 1 spike support --
/// see `agent_frame_view`'s content-signal doc comment.
fn get_or_create_tool_signal(
    tool_content: &Rc<RefCell<HashMap<usize, RwSignal<ToolBlock>>>>,
    block_id: usize,
    initial: &ToolBlock,
) -> RwSignal<ToolBlock> {
    *tool_content
        .borrow_mut()
        .entry(block_id)
        .or_insert_with(|| RwSignal::new(initial.clone()))
}

/// Writes `value` into `signal` only if it actually differs from the
/// current value -- so `agent_frame_view`'s bridge effect never wakes a
/// block's view over a frame change that didn't touch that block's own
/// content (e.g. another block's token, or a `state`-only change with no
/// item mutation at all).
fn set_if_changed<T: PartialEq + 'static>(signal: RwSignal<T>, value: T) {
    let changed = signal.with_untracked(|current| *current != value);
    if changed {
        signal.set(value);
    }
}

#[allow(clippy::too_many_arguments)]
fn transcript_block_view(
    block: TranscriptBlock,
    frame: impl Fn() -> AgentFrame + Copy + 'static,
    items_revision: floem::reactive::Memo<usize>,
    turn_in_flight: floem::reactive::Memo<bool>,
    block_view_ids: Rc<RefCell<HashMap<usize, floem::ViewId>>>,
    text_content: Rc<RefCell<HashMap<usize, RwSignal<String>>>>,
    tool_content: Rc<RefCell<HashMap<usize, RwSignal<ToolBlock>>>>,
    approval: ApprovalController,
) -> impl IntoView {
    let tone = block.tone;
    let block_id = block.id;
    let expanded = RwSignal::new(!style::is_collapsible(tone));
    let kind = block.kind;

    // Leg 1 spike (docs/agent-ui-performance-design.md): this block's own
    // content signal, seeded from `kind` -- the freshly-derived snapshot
    // `dyn_stack` just built this view from (`window`'s memo re-derives the
    // whole block list every token; see that memo's doc comment for why
    // that's out of scope here), so no extra `frame` read is needed just to
    // seed it. Exactly one of the two is ever `Some`: a block is either
    // `Text` or `Tool` for its whole lifetime (`transcript::transcript_blocks`'s
    // doc comment). Every later update to whichever signal exists comes
    // from `agent_frame_view`'s single bridge effect, not from re-reading
    // `frame` here.
    let tool_signal = match &kind {
        BlockKind::Tool(tool) => Some(get_or_create_tool_signal(&tool_content, block_id, tool)),
        BlockKind::Text { .. } => None,
    };
    let text_signal = match &kind {
        BlockKind::Text { text, .. } => {
            Some(get_or_create_text_signal(&text_content, block_id, text))
        }
        BlockKind::Tool(_) => None,
    };

    // Thinking's auto-expand-while-streaming (decision 5): `manual_override`
    // is `None` until the user first clicks the header, after which it wins
    // forever for this block. The effect composes it with the live
    // `is_thinking_streaming` result and writes it into `expanded` -- the
    // one signal the header/body below already read, so no other call site
    // needs to change.
    //
    // Tracks `items_revision`/`turn_in_flight` (`agent_frame_view`'s shared
    // coarse proxies) rather than `frame()` directly, reading `frame()`
    // itself `untrack`ed -- the same pattern `changes_bar_view`'s `changes`
    // memo uses for `session_changes`. `is_thinking_streaming` reads
    // `frame.items` (whether a later item has superseded this one as the
    // turn's last) *and* `frame.is_turn_in_flight()` (`frame.state`), so
    // both proxies are needed: `items_revision` alone would miss a turn
    // ending without any new item being pushed. Either one changing is
    // exactly the set of frame changes that can flip this effect's result;
    // a streamed token that only grows this block's own text (coalesced in
    // place, see `apply_agent_event_to_frame`'s `ReasoningDelta` arm)
    // changes neither, so this effect no longer re-fires per token.
    let manual_override = RwSignal::new(None::<bool>);
    if tone == TranscriptTone::Thinking {
        create_effect(move |_| {
            items_revision.get();
            turn_in_flight.get();
            let auto = crate::profiling::timed("transcript.is_thinking_streaming", || {
                untrack(|| is_thinking_streaming(&frame(), block_id))
            });
            expanded.set(manual_override.get().unwrap_or(auto));
        });
    }

    let view = v_stack((
        turn_boundary_rule_view(tone),
        h_stack((
            label(String::new).style(move |s| {
                if tone == TranscriptTone::User {
                    s.flex_basis(0.0).flex_grow(1.0).min_width(40.0)
                } else {
                    s.hide()
                }
            }),
            v_stack((
                transcript_header_view(tone, kind, expanded, manual_override, tool_signal),
                transcript_body_view(tone, expanded, text_signal, tool_signal, approval),
            ))
            .style(move |s| {
                let s = s.flex_col().min_width(0.0).max_width(block_max_width(tone));
                // Assistant prose stays chromeless (research heuristic: user
                // boxed, assistant bare text) -- every other tone keeps its
                // surface/border (`docs/agent-output-ui-design.md` decision
                // 6). A `Tool` block awaiting approval swaps in the approval
                // theme roles instead (decision 8), live-checked so the tint
                // clears the instant it resolves.
                let s = if tone == TranscriptTone::Assistant {
                    s
                } else {
                    // Reads this block's own content signal (leg 1 spike)
                    // rather than re-deriving from `frame` -- this was the
                    // dominant hot path measured by `horizon profile`
                    // (`docs/agent-ui-performance-design.md`): a raw-`frame()`
                    // read here re-derived every Tool block's state on every
                    // streamed token, session-wide. Now only *this* block's
                    // own signal changing wakes this closure.
                    let confirming = tool_signal
                        .is_some_and(|signal| signal.with(|tool| tool.needs_confirmation()));
                    let (background, border) = if confirming {
                        style::tool_block_colors(true)
                    } else {
                        block_colors(tone)
                    };
                    s.background(background).border(1.0).border_color(border)
                };

                match tone {
                    TranscriptTone::User => s,
                    _ => s.flex_basis(0.0).flex_grow(1.0),
                }
            }),
        ))
        .style(move |s| s.width_full().items_start().gap(12)),
    ))
    .style(|s| s.width_full().flex_col());

    // Registered once per block (see `dyn_stack`'s `(block.id, block.tone)`
    // key in `agent_frame_view`: an unchanged key means this constructor
    // never runs again for the same block) -- what the "jump to latest
    // user message" pill resolves a block id through to reach an actual
    // `.scroll_to_view` target.
    block_view_ids.borrow_mut().insert(block_id, view.id());
    view
}

/// The subtle rule that opens a new turn (decision 6) -- rendered above
/// every user-message block, whose `tone` never changes over the block's
/// lifetime, so this can be a plain, non-reactive style rather than a live
/// re-derivation.
fn turn_boundary_rule_view(tone: TranscriptTone) -> impl IntoView {
    label(String::new).style(move |s| {
        if !starts_new_turn(tone) {
            return s.hide();
        }

        s.width_full()
            .height(1.0)
            .margin_top(14)
            .margin_bottom(6)
            .background(theme::border_subtle())
    })
}

/// The trailing rule marking a completed turn's end (decision 6), rendered
/// once after the whole transcript rather than per-block: unlike
/// `turn_boundary_rule_view`'s `tone`, whether the turn just ended is a
/// live property of `frame`'s current state, so this reads `frame`/`window`
/// reactively in its own `.style` closure -- the same pattern
/// `omitted_summary`'s label above already uses.
fn turn_end_rule_view(
    window: floem::reactive::Memo<TranscriptWindow>,
    frame: impl Fn() -> AgentFrame + Copy + 'static,
) -> impl IntoView {
    label(String::new).style(move |s| {
        let last_tone = window.with(|window| window.blocks.last().map(|block| block.tone));
        if !show_turn_end_rule(&frame(), last_tone) {
            return s.hide();
        }

        s.width_full()
            .height(1.0)
            .margin_top(14)
            .background(theme::border_subtle())
    })
}

/// The transcript's live status line ("Agent is replying...", "Agent
/// failed", ...), driven by `status_text` (`agent_frame_view`'s
/// `session_state`/`items_revision`-gated memo) -- chrome rendered as a
/// plain sibling in `content`'s `v_stack`, right after the blocks
/// `dyn_stack` and before `turn_end_rule_view`, the same position the
/// pre-leg-1 synthetic status block occupied (it was always the last
/// pushed block, so windowing kept it at the tail). Deliberately *not* a
/// transcript block/per-block content signal any more (leg 1 hardening,
/// see `transcript_blocks`' doc comment): there is no `frame.items` entry
/// backing this text for `diff_block_content` to key a signal refresh off
/// of, so it must be driven by its own live memo instead.
fn status_indicator_view(status_text: floem::reactive::Memo<Option<String>>) -> impl IntoView {
    label(move || status_text.get().unwrap_or_default()).style(move |s| {
        if status_text.with(Option::is_none) {
            return s.hide();
        }

        s.width_full()
            .font_family(font_family().to_string())
            .font_size(12)
            .color(theme::text_muted())
            .padding_vert(4)
    })
}

/// The block's one-line header. `Tool`-kind blocks route to
/// `tool_view::tool_header_view`, whose text/color re-derive from
/// `tool_signal` (the block's own content signal, leg 1 spike) on every
/// status transition; every other kind keeps the pre-slice-1 static label
/// (computed once -- these blocks' headers never change over their
/// lifetime, only their body text streams in).
fn transcript_header_view(
    tone: TranscriptTone,
    kind: BlockKind,
    expanded: RwSignal<bool>,
    manual_override: RwSignal<Option<bool>>,
    tool_signal: Option<RwSignal<ToolBlock>>,
) -> impl IntoView {
    match kind {
        BlockKind::Tool(_) => {
            let tool_signal = tool_signal
                .expect("a Tool-kind block must already have its content signal seeded above");
            tool_view::tool_header_view(tool_signal, expanded).into_any()
        }
        BlockKind::Text { .. } => {
            let text = block_label(tone, &kind);
            label(move || text.clone())
                .on_click_stop(move |_| {
                    if tone == TranscriptTone::Thinking {
                        // A manual click always wins from here on (decision
                        // 5) -- toggled relative to what's currently shown,
                        // not the raw auto-derived value, so a click always
                        // does what it visually looks like it should do.
                        manual_override.set(Some(!expanded.get_untracked()));
                    }
                })
                .style(move |s| {
                    if !shows_label(tone) {
                        return s.hide();
                    }
                    style::header_row_style(s, tone, expanded.get()).color(block_text_color(tone))
                })
                .into_any()
        }
    }
}

fn transcript_body_view(
    tone: TranscriptTone,
    expanded: RwSignal<bool>,
    text_signal: Option<RwSignal<String>>,
    tool_signal: Option<RwSignal<ToolBlock>>,
    approval: ApprovalController,
) -> impl IntoView {
    if let Some(tool_signal) = tool_signal {
        tool_view::tool_body_view(tool_signal, expanded, approval).into_any()
    } else {
        let text_signal = text_signal
            .expect("a Text-kind block must already have its content signal seeded above");
        markdown_block_view(tone, expanded, text_signal).into_any()
    }
}

/// Renders a streaming text block's markdown, reading `text` -- the block's
/// own content signal (leg 1 spike, `docs/agent-ui-performance-design.md`)
/// -- instead of re-deriving from the whole `frame` on every fire: before
/// this, *every* mounted text block's `dyn_stack` content closure below
/// subscribed to the raw `frame` signal directly, so all of them re-parsed
/// their markdown on every streamed token session-wide, not just the one
/// block whose text actually grew.
fn markdown_block_view(
    tone: TranscriptTone,
    expanded: RwSignal<bool>,
    text: RwSignal<String>,
) -> impl IntoView {
    dyn_stack(
        move || {
            if tone == TranscriptTone::Thinking && !expanded.get() {
                Vec::new()
            } else {
                text.with(|text| markdown_lines(text))
            }
        },
        move |line| (line.index, line.kind, line.text.clone()),
        move |line| markdown_line_view(line, tone),
    )
    .style(move |s| {
        if tone == TranscriptTone::Thinking && !expanded.get() {
            return s.hide();
        }

        s.width_full()
            .flex_col()
            .gap(3)
            .padding_horiz(14)
            .padding_vert(10)
    })
}

fn markdown_line_view(line: MarkdownLine, tone: TranscriptTone) -> impl IntoView {
    label(move || line.text.clone()).style(move |s| {
        let mut s = s
            .width_full()
            .min_width(0.0)
            .font_family(font_family().to_string())
            .line_height(1.42)
            .color(block_text_color(tone));

        s = match line.kind {
            MarkdownLineKind::Heading => s.font_size(14).padding_top(5).padding_bottom(3),
            MarkdownLineKind::Bullet => s.font_size(12).padding_left(8),
            MarkdownLineKind::Code => s
                .font_size(12)
                .padding_horiz(8)
                .padding_vert(3)
                .background(theme::surface_base())
                .border(1.0)
                .border_color(theme::border_subtle()),
            MarkdownLineKind::Blank => s.font_size(6).height(6),
            MarkdownLineKind::Paragraph => s.font_size(12),
        };

        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{
        Message, MessageDelta, MessageRole, SessionState, ToolCallId, ToolCallRequest,
        ToolCallResult,
    };
    use crate::agent::frame::AgentFrameItem;
    use transcript::transcript_blocks;

    fn text_of(block: &TranscriptBlock) -> &str {
        match &block.kind {
            BlockKind::Text { text, .. } => text,
            BlockKind::Tool(_) => panic!("expected a text block, got a tool block"),
        }
    }

    #[test]
    fn transcript_blocks_keep_full_assistant_text() {
        let text = "long assistant response ".repeat(80);
        let frame = AgentFrame {
            state: None,
            items: vec![AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
                text: text.clone(),
            })],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 1);
        assert_eq!(text_of(&blocks[0]), text);
        assert_eq!(blocks[0].tone, TranscriptTone::Assistant);
    }

    // `transcript_blocks` no longer appends the ephemeral status line (leg 1
    // hardening: see that function's doc comment) -- its show/hide logic is
    // now tested directly against `status_block`/`should_show_initial_
    // reply_status` in `transcript.rs`'s own tests, and the live reactive
    // wiring that replaces the old per-item append is tested below in
    // `status_text_recomputes_on_a_state_only_transition_with_no_new_item`.

    #[test]
    fn omitted_summary_pluralizes_the_item_count() {
        assert_eq!(omitted_summary(1), "1 earlier item hidden");
        assert_eq!(omitted_summary(2), "2 earlier items hidden");
    }

    // --- reactive gating sufficiency (agent-ui-performance over-tracking fix) -
    //
    // These build the exact reactive-graph shapes `agent_frame_view` wires up
    // as standalone graphs -- floem's reactive runtime is a plain
    // thread-local, so `create_effect`/`create_memo`/`RwSignal` work outside
    // any mounted view. This proves the fixes actually cut recompute
    // frequency (not just that the underlying pure functions are safe to
    // gate this way), and pins each mechanism's *sufficiency*: an event that
    // must not be missed always shows up as a recompute, and streamed-token
    // noise never does. Mirrors the convention `changes.rs`'s
    // `streaming_text_deltas_leave_item_count_and_changes_untouched`/
    // `a_tool_call_finishing_always_grows_item_count` set for the
    // `session_changes` fix this one reuses the pattern from.

    /// Leg 1 spike's content-signal bridge, replayed standalone: proves the
    /// full chain (bridge effect -> `RwSignal<ToolBlock>` -> a downstream
    /// consumer, standing in for `tool_view`'s reactive closures) only wakes
    /// the consumer when *this* block's own tool content actually changes,
    /// not on every `frame` write. Unlike `current_tool_block_gating_
    /// recomputes_only_on_items_revision_changes` (the pre-signal version
    /// this replaces), the downstream consumer here reads the *signal*, not
    /// `frame()` -- so this also proves blocks the bridge never touches
    /// (because their id has no live signal) don't spuriously wake either.
    #[test]
    fn tool_content_signal_only_updates_on_a_real_status_change() {
        let frame_signal = RwSignal::new(AgentFrame {
            state: None,
            items: vec![
                AgentFrameItem::ToolCallRequested(ToolCallRequest {
                    call_id: ToolCallId("call-1".to_string()),
                    tool_id: "fs.edit".to_string(),
                    input: serde_json::json!({}),
                }),
                AgentFrameItem::AssistantTextDelta(MessageDelta {
                    role: MessageRole::Assistant,
                    text: "unrelated ".to_string(),
                }),
            ],
        });
        let frame = move || frame_signal.get();

        let tool_content: Rc<RefCell<HashMap<usize, RwSignal<ToolBlock>>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let initial = transcript::current_tool_block(&frame_signal.get_untracked(), 0)
            .expect("block 0 is a tool call");
        let tool_signal = get_or_create_tool_signal(&tool_content, 0, &initial);

        // The bridge effect, wired exactly as `agent_frame_view` wires it.
        let call_id_to_block: Rc<RefCell<HashMap<ToolCallId, usize>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let previous_items_len = Rc::new(RefCell::new(0usize));
        create_effect(move |_| {
            let frame_snapshot = frame();
            let diff = transcript::diff_block_content(
                &frame_snapshot,
                *previous_items_len.borrow(),
                &mut call_id_to_block.borrow_mut(),
            );
            *previous_items_len.borrow_mut() = frame_snapshot.items.len();
            let tool_content = tool_content.borrow();
            for (id, tool) in diff.tool {
                if let Some(signal) = tool_content.get(&id) {
                    set_if_changed(*signal, tool);
                }
            }
        });

        // A downstream consumer standing in for `tool_view`'s reactive
        // closures: reads only the signal, never `frame()` directly.
        let runs = Rc::new(RefCell::new(0));
        let runs_probe = runs.clone();
        create_effect(move |_| {
            tool_signal.get();
            *runs_probe.borrow_mut() += 1;
        });
        assert_eq!(*runs.borrow(), 1, "the initial run");

        // A streamed token growing the *existing* `AssistantTextDelta` in
        // place (`apply_agent_event_to_frame`'s coalescing) -- unrelated to
        // this tool block and leaves `items.len()` unchanged.
        frame_signal.update(|frame| {
            if let AgentFrameItem::AssistantTextDelta(delta) = &mut frame.items[1] {
                delta.text.push_str("more ");
            }
        });
        assert_eq!(
            *runs.borrow(),
            1,
            "a coalesced, unrelated text delta must not wake this tool block's signal"
        );

        // A genuine status-changing item (`ToolCallFinished`) is always a
        // plain push -- must still update the signal.
        frame_signal.update(|frame| {
            frame
                .items
                .push(AgentFrameItem::ToolCallFinished(ToolCallResult {
                    call_id: ToolCallId("call-1".to_string()),
                    output: serde_json::json!({}),
                }));
        });
        assert_eq!(
            *runs.borrow(),
            2,
            "a real status-changing item must still update the signal"
        );
    }

    #[test]
    fn is_thinking_streaming_gating_recomputes_on_items_or_turn_state_changes_only() {
        let frame_signal = RwSignal::new(AgentFrame {
            state: Some(SessionState::Running),
            items: vec![AgentFrameItem::ReasoningDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: "thinking".to_string(),
            })],
        });
        let frame = move || frame_signal.get();
        let items_revision = create_memo(move |_| frame().items.len());
        let turn_in_flight = create_memo(move |_| frame().is_turn_in_flight());

        let runs = Rc::new(RefCell::new(0));
        let runs_probe = runs.clone();
        create_effect(move |_| {
            items_revision.get();
            turn_in_flight.get();
            *runs_probe.borrow_mut() += 1;
            untrack(|| is_thinking_streaming(&frame(), 0));
        });
        assert_eq!(*runs.borrow(), 1, "the initial run");

        // Growing the same `ReasoningDelta` in place (streamed thinking
        // tokens) changes neither `items.len()` nor turn-in-flight status.
        frame_signal.update(|frame| {
            if let AgentFrameItem::ReasoningDelta(delta) = &mut frame.items[0] {
                delta.text.push_str(" more");
            }
        });
        assert_eq!(
            *runs.borrow(),
            1,
            "a coalesced reasoning delta must not re-trigger the auto-expand check"
        );

        // A later item superseding this one as the turn's last item is
        // always a plain push -- `items_revision` alone catches this.
        frame_signal.update(|frame| {
            frame
                .items
                .push(AgentFrameItem::AssistantTextDelta(MessageDelta {
                    role: MessageRole::Assistant,
                    text: "reply".to_string(),
                }));
        });
        assert_eq!(
            *runs.borrow(),
            2,
            "a new item superseding this block as the turn's last item must \
             re-trigger the auto-expand check"
        );

        // The turn ending changes `frame.state` without touching `items` at
        // all -- exactly the dependency `items_revision` alone would miss,
        // which is why `turn_in_flight` is a second, independent proxy.
        frame_signal.update(|frame| frame.state = Some(SessionState::Completed));
        assert_eq!(
            *runs.borrow(),
            3,
            "turn-in-flight ending must still re-trigger the auto-expand check \
             even though no item was pushed"
        );
    }

    /// Leg 1 hardening: the status line is no longer a per-block content
    /// signal (see `status_indicator_view`'s doc comment for why one can't
    /// work here -- there's no `frame.items` entry backing it), so it needs
    /// its own reactive sufficiency check the way `is_thinking_streaming`'s
    /// gating does above. Replays `agent_frame_view`'s exact
    /// `session_state`/`items_revision` -> `status_text` wiring standalone
    /// and proves the two state-only transitions (no item pushed) that a
    /// naive per-block signal would have frozen on stale text --
    /// `Running -> Failed` and `ToolRunning -> Completed` -- both still
    /// update the text/visibility live.
    #[test]
    fn status_text_recomputes_on_a_state_only_transition_with_no_new_item() {
        let frame_signal = RwSignal::new(AgentFrame {
            state: Some(SessionState::Running),
            items: Vec::new(),
        });
        let frame = move || frame_signal.get();
        let items_revision = create_memo(move |_| frame().items.len());
        let session_state = create_memo(move |_| frame().state);

        let status_text = create_memo(move |_| {
            let state = session_state.get();
            items_revision.get();
            untrack(|| {
                let blocks = transcript_blocks(&frame());
                status_block(state, 0, blocks.last()).map(|block| match block.kind {
                    BlockKind::Text { text, .. } => text,
                    BlockKind::Tool(_) => String::new(),
                })
            })
        });

        assert_eq!(status_text.get(), Some("Agent is replying...".to_string()));

        // `Running -> Failed`, no item pushed at all.
        frame_signal.update(|frame| frame.state = Some(SessionState::Failed));
        assert_eq!(status_text.get(), Some("Agent failed".to_string()));

        // `ToolRunning -> Completed`, again no item pushed -- the status
        // line must also disappear, not just change text.
        frame_signal.update(|frame| frame.state = Some(SessionState::ToolRunning));
        assert_eq!(status_text.get(), Some("Running tool...".to_string()));
        frame_signal.update(|frame| frame.state = Some(SessionState::Completed));
        assert_eq!(status_text.get(), None);
    }
}
