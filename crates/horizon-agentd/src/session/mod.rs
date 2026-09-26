//! Session hosting: `docs/agent-runtime-split-design.md` steps 3-4. Each
//! `Control::SessionNew` (or a resumed session found in the event log at
//! startup, see [`resume_persisted_sessions`]) spawns a dedicated OS thread
//! that owns the real session loop (the same `providers`/`tools`/
//! `persistence` machinery Horizon used to run in-process), and command/event
//! envelopes are routed to/from that thread by session id.
//!
//! **Why a dedicated thread per session, not an async task.** `LiveState`/
//! `ToolSessionState` are `Rc`-based and `tools::state::SESSION_RUNTIMES` is
//! a `thread_local!` (see their doc comments in the crate) — both assume
//! everything for one session runs on a single, consistent OS thread, the
//! way Horizon's floem UI thread provided in-process. A dedicated thread per
//! session reproduces exactly that: `register_session_runtime` and every
//! later `session_runtime` lookup for the same session id (from
//! `resolve_approval`, driven by an incoming `ApproveToolCall`/
//! `DenyToolCall` envelope) happen on the same thread, so the thread-local
//! registry works correctly without making any of this `Send`. Blocking is
//! also what makes the host-tool round trip simple (see
//! `host_tools::AgentdHostTools::execute_auto`): the session thread genuinely blocks
//! on a channel recv while Horizon answers over the wire, which would
//! deadlock a single-threaded async runtime but is harmless on its own
//! dedicated thread.
//!
//! Sessions outlive their UI connections. Each attachment owns a revocable
//! lease covering its private history bootstrap, live updates and command
//! acceptance. The session owner captures history and installs the subscriber
//! at one event boundary; replacement closes the previous lease without
//! terminating the session. See `docs/agent-attachment-design.md`.
//!
//! **Where things live.** [`state`] holds the process-lifetime registry every
//! other module works through; [`connection`] is one connection's view of it.
//! A session is created by [`spawn`], configured through [`setup`] and
//! [`environment`], and lived by [`run`]; [`resume`] recreates one from the log.
//! [`events`] fans a session's output out to the attached client and to
//! in-process subscribers, [`host_tools`] runs the host round trip,
//! [`input`] owns durable input acceptance and delivery acknowledgements;
//! [`approval`] and [`completion`] own the approval seam and the
//! asynchronous tool folds, [`subscription`] is the "observe another
//! session's stop/blocking events" seam, [`exploration`] implements the
//! `task` tool's daemon seam on top of it, and [`panic`] is the session
//! thread's panic boundary.

mod approval;
mod attachment;
mod board;
mod completion;
mod connection;
mod environment;
mod events;
mod exploration;
mod host_tools;
mod input;
mod model_selection;
mod panic;
mod resume;
mod run;
mod setup;
mod spawn;
mod state;
mod subscription;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use self::attachment::Bootstrap;
pub(crate) use self::connection::Connection;
pub(crate) use self::resume::{resume_persisted_sessions, resume_session};
pub(crate) use self::spawn::spawn_session_thread;
pub(crate) use self::state::AgentdState;
