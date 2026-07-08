//! Per-session fine-grained agent-frame storage -- `docs/reactive-store-
//! design.md`'s "foundation 5" applied to the agent half of `session::
//! Frames` (leg 1 of the migration; terminal frames stay on the old
//! whole-map path for now, see that doc's "Ordering of the migration").
//!
//! [`AgentFrameHandle`] is the Lapce-style "handle": a plain `Rc`-held
//! bundle of independent field signals (`state`, `items`, `state_entry`)
//! created together in one per-session child `Scope`, so a session's
//! signals are all disposed at once when it ends (`Frames::remove_session`).
//! `session::Frames` stores these by value in a coarse membership map
//! (`RwSignal<im::HashMap<SessionId, AgentFrameHandle>>`) -- a reader that
//! grabs one session's handle and reads only its own field signals no
//! longer subscribes to any other session's updates, which is the actual
//! over-tracking fix (see the design doc's "gap" section).

use std::rc::Rc;

use floem::reactive::{batch, with_scope, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};

use crate::agent::contract::SessionState;
use crate::agent::frame::{in_place_mutable_item_indices, AgentFrame, AgentFrameItem, StateEntry};

#[derive(Clone)]
pub(crate) struct AgentFrameHandle(Rc<Inner>);

struct Inner {
    scope: Scope,
    state: RwSignal<Option<SessionState>>,
    items: RwSignal<Vec<AgentFrameItem>>,
    /// How long `state` has held its current value -- see [`StateEntry`]'s
    /// own doc comment for why this can't just be folded into `state`
    /// itself. Kept as its own field signal (rather than the flat sidecar
    /// `HashMap` `session::Frames` used to keep alongside its old single
    /// map) so a pane header reading only elapsed time doesn't have to
    /// subscribe to `items` at all.
    state_entry: RwSignal<StateEntry>,
}

impl AgentFrameHandle {
    /// Creates a fresh, empty handle whose three signals live in a new
    /// child scope of `parent` -- always `Frames::agent_scope`, a stable
    /// root chosen so a handle's signals outlive whichever transient
    /// effect scope happened to be running when the session's first frame
    /// arrived (a real case: `agent::agentd_runtime::
    /// fold_agent_session_events`'s own doc comment describes a CLI-
    /// spawned session's fold otherwise going silently dead the moment the
    /// spawning effect re-runs).
    pub(crate) fn new(parent: Scope) -> Self {
        let scope = parent.create_child();
        let (state, items, state_entry) = with_scope(scope, || {
            (
                RwSignal::new(None),
                RwSignal::new(Vec::new()),
                RwSignal::new(StateEntry::initial(None)),
            )
        });
        Self(Rc::new(Inner {
            scope,
            state,
            items,
            state_entry,
        }))
    }

    /// Disposes this handle's scope, dropping all three signals together --
    /// called once, by `Frames::remove_session`, when the session ends.
    pub(crate) fn dispose(&self) {
        self.0.scope.dispose();
    }

    /// The session's current `AgentFrame.state`. Returns an abstract signal
    /// handle, not the raw `RwSignal`, per `docs/reactive-store-design.md`'s
    /// store-swappable accessor boundary: a future store `Binding`
    /// implements the same `floem_reactive` signal traits, so swapping the
    /// field's internal representation later never touches a consumer that
    /// only ever called `.get()`/`.with()`/`.set()`/`.update()` here.
    pub(crate) fn state(
        &self,
    ) -> impl SignalGet<Option<SessionState>>
           + SignalWith<Option<SessionState>>
           + SignalUpdate<Option<SessionState>>
           + Copy
           + 'static {
        self.0.state
    }

    /// The session's current `AgentFrame.items`. See [`Self::state`]'s doc
    /// comment for the accessor-boundary rationale.
    pub(crate) fn items(
        &self,
    ) -> impl SignalGet<Vec<AgentFrameItem>>
           + SignalWith<Vec<AgentFrameItem>>
           + SignalUpdate<Vec<AgentFrameItem>>
           + Copy
           + 'static {
        self.0.items
    }

    /// See [`Self::state`]'s doc comment for the accessor-boundary
    /// rationale; [`StateEntry`] itself for what this tracks.
    pub(crate) fn state_entry(
        &self,
    ) -> impl SignalGet<StateEntry> + SignalWith<StateEntry> + SignalUpdate<StateEntry> + Copy + 'static
    {
        self.0.state_entry
    }

    /// Folds a freshly-computed `AgentFrame` into this handle, writing only
    /// the field signals that actually changed -- the write-side half of
    /// `docs/reactive-store-design.md`'s Frames migration. `state`/`items`
    /// are independent signals, so a pure token stream (which only ever
    /// touches `items` -- see `apply_agent_event_to_frame`'s
    /// `Event::StateChanged` arm being the only one that touches `state`)
    /// never wakes a reader watching only `state`, and vice versa.
    ///
    /// `batch()` covers the multi-field case: `agent::tools::approval::
    /// resolve_approval`'s `Executed`/`Started` outcomes fold several
    /// events (e.g. `ToolCallStarted` + `ToolCallFinished`, or a state
    /// transition alongside a new item) into one `AgentFrame` before this
    /// is ever called, so both fields can legitimately change in the same
    /// `apply_frame` call.
    pub(crate) fn apply_frame(&self, frame: AgentFrame) {
        batch(|| {
            apply_items(self.0.items, &frame);
            if set_if_changed(self.0.state, frame.state) {
                self.0
                    .state_entry
                    .update(|entry| *entry = entry.advance(frame.state));
            }
        });
    }
}

/// Writes `value` into `signal` only if it actually differs from the
/// current value, returning whether it did -- so a state-unchanged fold
/// (the common case: most events don't touch `state` at all) never wakes a
/// reader that only watches `state()`.
fn set_if_changed<T: PartialEq + 'static>(signal: RwSignal<T>, value: T) -> bool {
    let changed = signal.with_untracked(|current| *current != value);
    if changed {
        signal.set(value);
    }
    changed
}

/// What [`apply_items`] should do to bring `items`'s field signal up to
/// date with a freshly-folded frame -- computed by [`plan_items_write`],
/// kept as its own pure, directly testable type so the targeting logic
/// (which reuses [`in_place_mutable_item_indices`], the reducer's own
/// source of truth for what an in-place fold can touch) doesn't have to be
/// exercised through the signal machinery to prove it right.
#[derive(Debug, PartialEq)]
enum ItemsWritePlan {
    /// Nothing changed -- skip the write (and the notification) entirely.
    Unchanged,
    /// `frame.items` grew: only the newly appended tail needs writing.
    Append(Vec<AgentFrameItem>),
    /// `frame.items.len()` is unchanged, but one or more of the indices
    /// [`in_place_mutable_item_indices`] reports actually differ from what
    /// the signal currently holds (a coalesced streaming delta, or a
    /// `ToolCallPreparing` superseded in place) -- write only those slots.
    Patch(Vec<(usize, AgentFrameItem)>),
    /// Defensive fallback for a frame whose `items` got shorter than what
    /// the signal currently holds -- `apply_agent_event_to_frame` never
    /// actually shrinks `items` (see its own doc comment), so this should
    /// be unreachable in production, but replacing the whole vec is a safe
    /// recovery instead of the `Patch`/`Append` index arithmetic panicking.
    Replace(Vec<AgentFrameItem>),
}

/// Compares `current` (what `items`'s signal already holds) against `frame`
/// (a freshly-folded `AgentFrame`) and decides the minimal write needed --
/// pure and signal-free so it's unit-testable directly. Mirrors `agent::
/// view::transcript::diff_block_content`'s own growth-vs-in-place branching
/// (leg 1), which independently arrived at the same two-case split over the
/// same reducer contract; kept as a separate implementation here rather
/// than shared, since that function's job is producing per-block *content*
/// diffs for a view-owned signal map, not a `Vec` write plan for this
/// module's `items` field.
fn plan_items_write(current: &[AgentFrameItem], frame: &AgentFrame) -> ItemsWritePlan {
    let new_items = &frame.items;
    match new_items.len().cmp(&current.len()) {
        std::cmp::Ordering::Greater => ItemsWritePlan::Append(new_items[current.len()..].to_vec()),
        std::cmp::Ordering::Less => ItemsWritePlan::Replace(new_items.clone()),
        std::cmp::Ordering::Equal => {
            let mut patches = Vec::new();
            for index in in_place_mutable_item_indices(frame) {
                let Some(new_item) = new_items.get(index) else {
                    continue;
                };
                if current.get(index) != Some(new_item) {
                    patches.push((index, new_item.clone()));
                }
            }
            if patches.is_empty() {
                ItemsWritePlan::Unchanged
            } else {
                ItemsWritePlan::Patch(patches)
            }
        }
    }
}

/// Applies [`plan_items_write`]'s decision to `signal`.
fn apply_items(signal: RwSignal<Vec<AgentFrameItem>>, frame: &AgentFrame) {
    let plan = signal.with_untracked(|current| plan_items_write(current, frame));
    match plan {
        ItemsWritePlan::Unchanged => {}
        ItemsWritePlan::Append(tail) => signal.update(|items| items.extend(tail)),
        ItemsWritePlan::Patch(patches) => signal.update(|items| {
            for (index, item) in patches {
                items[index] = item;
            }
        }),
        ItemsWritePlan::Replace(items_new) => signal.set(items_new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Message, MessageDelta, MessageRole};

    fn message_item(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    fn delta_item(text: &str) -> AgentFrameItem {
        AgentFrameItem::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    fn frame_with(state: Option<SessionState>, items: Vec<AgentFrameItem>) -> AgentFrame {
        AgentFrame { state, items }
    }

    #[test]
    fn plan_is_unchanged_for_an_identical_frame() {
        let current = vec![message_item("hi")];
        let frame = frame_with(None, current.clone());

        assert_eq!(
            plan_items_write(&current, &frame),
            ItemsWritePlan::Unchanged
        );
    }

    #[test]
    fn plan_appends_the_newly_pushed_tail() {
        let current = vec![message_item("first")];
        let frame = frame_with(None, vec![message_item("first"), message_item("second")]);

        assert_eq!(
            plan_items_write(&current, &frame),
            ItemsWritePlan::Append(vec![message_item("second")])
        );
    }

    #[test]
    fn plan_appends_a_multi_item_tail_from_one_multi_event_fold() {
        // Mirrors `agent::tools::approval::resolve_approval`'s `Executed`
        // outcome, which can fold several events (e.g. `ToolCallStarted` +
        // `ToolCallFinished`) into one `AgentFrame` before `apply_frame` is
        // ever called.
        let current = Vec::new();
        let frame = frame_with(None, vec![message_item("a"), message_item("b")]);

        assert_eq!(
            plan_items_write(&current, &frame),
            ItemsWritePlan::Append(vec![message_item("a"), message_item("b")])
        );
    }

    #[test]
    fn plan_patches_a_coalesced_streaming_delta_in_place() {
        let current = vec![delta_item("hello")];
        let frame = frame_with(None, vec![delta_item("hello world")]);

        assert_eq!(
            plan_items_write(&current, &frame),
            ItemsWritePlan::Patch(vec![(0, delta_item("hello world"))])
        );
    }

    #[test]
    fn plan_is_unchanged_when_the_same_length_frame_carries_no_real_change() {
        // Same `items.len()`, but nothing in `in_place_mutable_item_indices`'s
        // candidate set actually differs (e.g. a `state`-only transition
        // with no item mutation at all).
        let current = vec![message_item("hi")];
        let frame = frame_with(Some(SessionState::Completed), current.clone());

        assert_eq!(
            plan_items_write(&current, &frame),
            ItemsWritePlan::Unchanged
        );
    }

    #[test]
    fn plan_falls_back_to_replace_on_an_unexpected_shrink() {
        let current = vec![message_item("a"), message_item("b")];
        let frame = frame_with(None, vec![message_item("a")]);

        assert_eq!(
            plan_items_write(&current, &frame),
            ItemsWritePlan::Replace(vec![message_item("a")])
        );
    }

    #[test]
    fn set_if_changed_reports_whether_it_wrote() {
        let signal = RwSignal::new(Some(SessionState::Running));

        assert!(!set_if_changed(signal, Some(SessionState::Running)));
        assert!(set_if_changed(signal, Some(SessionState::Completed)));
        assert_eq!(signal.get_untracked(), Some(SessionState::Completed));
    }

    #[test]
    fn handle_apply_frame_updates_state_and_items_independently() {
        let handle = AgentFrameHandle::new(Scope::new());

        handle.apply_frame(frame_with(
            Some(SessionState::Running),
            vec![message_item("hi")],
        ));
        assert_eq!(handle.state().get_untracked(), Some(SessionState::Running));
        assert_eq!(handle.items().get_untracked(), vec![message_item("hi")]);

        // A state-only transition must not touch `items`.
        handle.apply_frame(frame_with(
            Some(SessionState::Completed),
            vec![message_item("hi")],
        ));
        assert_eq!(
            handle.state().get_untracked(),
            Some(SessionState::Completed)
        );
        assert_eq!(handle.items().get_untracked(), vec![message_item("hi")]);
    }

    #[test]
    fn handle_apply_frame_advances_state_entry_only_on_a_real_transition() {
        let handle = AgentFrameHandle::new(Scope::new());
        handle.apply_frame(frame_with(Some(SessionState::Running), Vec::new()));
        let first_entered_at = handle.state_entry().get_untracked().entered_at();

        // Re-observing the same state (e.g. a token that doesn't change
        // `state` at all) must not disturb `entered_at`.
        handle.apply_frame(frame_with(
            Some(SessionState::Running),
            vec![message_item("still running")],
        ));
        assert_eq!(
            handle.state_entry().get_untracked().entered_at(),
            first_entered_at
        );

        std::thread::sleep(std::time::Duration::from_millis(5));
        handle.apply_frame(frame_with(Some(SessionState::Completed), Vec::new()));
        assert!(handle.state_entry().get_untracked().entered_at() > first_entered_at);
    }

    #[test]
    fn dispose_drops_the_handles_signals() {
        let scope = Scope::new();
        let handle = AgentFrameHandle::new(scope);
        handle.apply_frame(frame_with(Some(SessionState::Running), Vec::new()));

        handle.dispose();

        // A disposed signal's `try_get_untracked` reports `None` rather
        // than panicking -- confirms the child scope actually owned (and
        // dropped) these signals rather than sharing them with some other
        // scope.
        use floem::reactive::SignalGet as _;
        assert_eq!(handle.state().try_get_untracked(), None);
    }
}
