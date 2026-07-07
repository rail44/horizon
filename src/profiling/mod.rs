//! Opt-in UI-thread timing capture, readable from outside the app via the
//! control plane -- leg 3 of `docs/agent-ui-performance-design.md`'s three
//! complementary defenses against reactive over-tracking (leg 1 is the
//! structural API boundary, leg 2 is the write-time static check; this is
//! the run-time one: "did my change slow a hot path", answerable by an
//! agent via `horizon profile`).
//!
//! ## Why this measures closure time, not paint time
//!
//! The original idea was to reach floem's own built-in profiler (the
//! git-pinned rev's `src/profiler.rs`: `Profile`/`ProfileFrame`/
//! `ProfileEvent`, wired to the inspector window's "Start Profiling"
//! button). That module -- and the `AppUpdateEvent::ProfileWindow`/
//! `add_app_update_event` machinery that drives it -- is entirely
//! `pub(crate)` inside floem: nothing in it is reachable from a downstream
//! crate like Horizon. Confirmed by reading the floem source directly
//! (`mod profiler;` in `lib.rs`, not `pub mod`).
//!
//! floem also gives applications no public hook for "a frame was rendered"
//! at all: `floem::AppEvent` has only `WillTerminate`/`Reopen`, and
//! `floem::event::EventListener` (~30 variants a view's `on_event` can
//! observe) has no `RedrawRequested`/animation-frame equivalent --
//! `WindowEvent::RedrawRequested` is matched entirely inside floem's
//! `app_handle.rs`, invisible to app code.
//!
//! So this module measures a different, honestly-scoped thing: how long an
//! explicitly-wrapped code path took, via [`timed`]. The over-tracking class
//! this leg exists to catch fires in the *reactive graph* while a session
//! streams -- a memo or effect re-running once per token because it reads
//! the raw frame signal instead of a narrower derived one -- not in input
//! handling, so the capture points that matter are the transcript's hot
//! reactive closures: `agent::view::transcript::compute_transcript_window`
//! (the window/revision memo) and the per-fire memos/effects in
//! `agent::view` (`session_changes`, `items_revision`,
//! `is_thinking_streaming`, `current_tool_block`, `latest_user_block_id`).
//! Input-handler capture points (`app::view::app_view`'s global `on_event`
//! chain, `workspace::view::pane`'s per-pane `KeyDown` handler) are kept
//! too, as a low-frequency baseline contrast -- typing fires once per
//! keystroke, not once per streamed token -- but they are not this leg's
//! primary target. None of this includes floem's internal layout/paint
//! pass, which runs afterward as a separate, unobservable step.
//!
//! ## Substrate
//!
//! Default off ([`is_enabled`], gated by `HORIZON_UI_PROFILE`) so a normal
//! run pays no cost beyond one cached bool check per timed call. When
//! enabled, each timed event is appended as one JSONL line (trigger name +
//! duration + timestamp) to a durable log file (`path::log_path`),
//! mirroring the shape of `crates/horizon-agent`'s agent event log
//! (schema/version fields, tolerant reading that skips corrupt lines) but
//! deliberately simpler: no sequence numbers, no multi-writer coordination,
//! no DuckDB projection -- one lazily-spawned background thread is enough.
//! `app::external_commands`'s `Query { what: "profile" }` handler reads
//! this same file and reshapes it into
//! `horizon_control::contract::ProfileSnapshot`, so `horizon profile` can
//! print recent timings -- and, since each line carries its trigger name
//! and timestamp, an agent can spot over-tracking directly as a burst of
//! same-trigger entries with small timestamp deltas -- from outside the
//! app, the same "durable file + control-plane command" shape the agent
//! event log's own external readability (`agent-inspect` skill) already
//! established.

mod capture;
mod path;
mod record;

#[cfg(test)]
mod tests;

/// Test-only: lets another module's own test (e.g.
/// `agent::view::transcript`'s capture-point test) block until every
/// `timed` call it made has actually landed in the JSONL log, the same
/// synchronization this module's own tests (`tests.rs`) already rely on.
#[cfg(test)]
pub(crate) use capture::flush_for_test;
pub(crate) use capture::{is_enabled, timed};
pub(crate) use path::log_path;
pub(crate) use record::read_recent;
