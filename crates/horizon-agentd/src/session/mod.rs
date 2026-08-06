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
//! **Sessions are scoped to the process, not the connection (step 4).**
//! `AgentdState::sessions`/`pending_host_tool_requests`/`outgoing` are
//! process-lifetime (built once in `main`, shared via `Arc`) rather than
//! recreated per accepted connection: a session's thread outlives any one
//! connection, and a fresh connection re-targets the *same* running
//! sessions rather than starting over. `outgoing` is the seam that makes
//! that possible — a swappable "current connection's writer channel" cell
//! (`Connection::new` installs it, `Connection::disconnect` clears it) that
//! every session thread sends through by reference, so a session spawned
//! before any connection existed (a resumed session at startup) and a
//! session spawned mid-connection are indistinguishable once they're
//! running: both just send through whatever `outgoing` currently points at,
//! silently dropping events when it's `None` (no client to see them).
//!
//! **Where things live.** [`state`] holds the process-lifetime registry every
//! other module works through; [`connection`] is one connection's view of it.
//! A session is created by [`spawn`], built by [`setup`], and lived by
//! [`run`]; [`resume`] is the startup path that recreates one from the log.
//! [`events`] fans a session's output out to the attached client and to
//! in-process subscribers, [`host_tools`] runs the host round trip,
//! [`approval`] and [`completion`] own the approval seam and the
//! asynchronous tool folds, [`subscription`] is the "observe another
//! session's stop/blocking events" seam, [`exploration`] implements the
//! `task` tool's daemon seam on top of it, and [`panic`] is the session
//! thread's panic boundary.

mod approval;
mod board;
mod completion;
mod connection;
mod events;
mod exploration;
mod host_tools;
mod panic;
mod resume;
mod run;
mod setup;
mod spawn;
mod state;
mod subscription;
#[cfg(test)]
mod test_support;

pub(crate) use self::connection::Connection;
pub(crate) use self::resume::resume_persisted_sessions;
pub(crate) use self::state::AgentdState;
