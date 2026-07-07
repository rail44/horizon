//! SPIKE: opt-in UI-thread frame-timing capture, readable from outside the
//! app via the control plane -- see `docs/roadmap.md` and the review
//! request this landed under for the full writeup.
//!
//! ## Why this measures event-handling time, not paint time
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
//! So this module measures a different, honestly-scoped thing: how long
//! Horizon's own handler took to process one window-level event, timed
//! around the existing global `on_event` chain in `app::view::app_view`
//! (window focus, IME, key-down -- see that module). This does *not*
//! include floem's internal layout/paint pass, which runs afterward as a
//! separate, unobservable step. It's a legitimate proxy for UI-thread
//! responsiveness (how busy Horizon's own code keeps the UI thread per
//! input event), not a substitute for true render-frame timing.
//!
//! ## Substrate
//!
//! Default off ([`is_enabled`], gated by `HORIZON_UI_PROFILE`) so a normal
//! run pays no cost beyond one cached bool check per event. When enabled,
//! each timed event is appended as one JSONL line to a durable log file
//! (`path::log_path`), mirroring the shape of `crates/horizon-agent`'s
//! agent event log (schema/version fields, tolerant reading that skips
//! corrupt lines) but deliberately simpler: no sequence numbers, no
//! multi-writer coordination, no DuckDB projection -- spike quality, one
//! lazily-spawned background thread is enough. `app::external_commands`'s
//! `Query { what: "profile" }` handler reads this same file and reshapes
//! it into `horizon_control::contract::ProfileSnapshot`, so `horizon
//! profile` can print recent frame timings from outside the app, the same
//! "durable file + control-plane command" shape the agent event log's own
//! external readability (`agent-inspect` skill) already established.

mod capture;
mod path;
mod record;

#[cfg(test)]
mod tests;

pub(crate) use capture::{is_enabled, timed};
pub(crate) use path::log_path;
pub(crate) use record::read_recent;
