//! Horizon's agent sessions, hosted: the session threads, their resume
//! from the event log, approvals, worktrees and checkpoints, what starts
//! sessions on its own (the outbox), and the ACP agent side every client
//! speaks to (`docs/agent-stdio-child-design.md`). A process hosts them by
//! building an [`AgentdState`](session::AgentdState) with the
//! [`Extensions`](extension::Extensions) it is composed with, starting
//! [`startup::spawn_resume_task`] and the outbox, and serving connections
//! with [`hub::serve`].

mod checkpoint;
pub mod config_watch;
pub mod extension;
pub mod hub;
pub mod outbox;
pub mod providers;
pub mod session;
pub mod startup;
pub mod worktree;

/// The name every log line of the host starts with.
pub const DAEMON_NAME: &str = "horizon-agent";
