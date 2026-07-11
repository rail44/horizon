//! Horizon's third surface over the unified command model (after the
//! command palette and keybindings): a Unix-socket control plane external
//! processes (a future CLI binary, or Horizon's own `bash` tool acting on
//! the instance it runs inside) can drive. See
//! `docs/cli-control-plane-design.md` for the settled design and
//! `crates/horizon-control` for the frozen wire contract this module speaks
//! -- that crate only defines shapes; everything that actually binds a
//! socket, dispatches a request, or touches `Workspace` lives here.
//!
//! Module layout:
//! - [`socket`] resolves this process's control-socket path.
//! - [`listener`] owns the dedicated OS thread that accepts connections,
//!   spawning one more thread per connection -- `horizon-agentd`'s "one
//!   connection at a time" simplification is deliberately not inherited
//!   (see the design doc's "Endpoint" decision: the CLI contract assumes
//!   multiple concurrent clients from v1).
//! - [`connection`] speaks the hello handshake and request/response loop for
//!   one connection, generic over [`executor::ControlExecutor`] so it's
//!   fully testable without floem or a live `Workspace`.
//! - [`executor`] defines that abstraction: the trait connection handling
//!   dispatches every request through.
//! - [`bridge`] is the one floem-specific layer: [`bridge::ChannelExecutor`]
//!   (the real [`executor::ControlExecutor`] every connection holds) bridges
//!   a connection thread's request onto the UI thread via the established
//!   `floem::ext_event::create_signal_from_channel` + `create_effect`
//!   pattern (see `agent::agentd_runtime::wire_host_tool_responder`), where
//!   it forwards to `app::external_commands::dispatch_invoke`/`dispatch_query`
//!   (the actual `execute_command`/state-read logic, kept in `app` next to
//!   the command model itself) and sends the result back over a crossbeam
//!   reply channel -- so a listener/connection thread never touches
//!   `Workspace` or any `RwSignal` directly.

mod bridge;

// The transport (socket resolution, listener, per-connection handling,
// the ControlExecutor seam) moved to horizon-control::host, shared with
// shell-gpui (docs/gpui-migration-design.md M3); bridge - the one
// floem-specific layer - stays here.
use horizon_control::host::{listener, socket};

use crate::app::command_actions::CommandActionState;

pub(crate) use socket::default_socket_path;

/// Starts the control plane for this process: binds
/// [`default_socket_path`] on a dedicated OS thread and wires accepted
/// requests to `command_state` via the UI-thread bridge. Must be called from
/// the UI thread (it registers a `floem::reactive::create_effect`) -- see
/// `app::state::AppState::new`'s call site, which runs this before any pane
/// (and so before any child process that might read `HORIZON_SOCKET`) is
/// spawned, closing the connect-before-bind race that would otherwise exist.
///
/// Best-effort: a bind failure (e.g. an unwritable `XDG_RUNTIME_DIR`) is
/// logged to stderr and leaves the control plane unavailable for this run --
/// it never blocks or fails Horizon's own startup. External control was
/// always meant to be optional infrastructure layered on the app, never a
/// dependency of it (unlike `horizon-agentd`, which agent panes hard-depend
/// on).
pub(crate) fn start(command_state: CommandActionState) {
    let socket_path = default_socket_path();
    let (executor, requests) = bridge::channel_pair();
    bridge::wire(requests, command_state);
    listener::spawn(socket_path, executor);
}

/// Removes this process's control socket file, if any -- called from
/// `app::shutdown` on a normal exit, mirroring `horizon-agentd`'s own
/// remove-the-socket-on-exit convention (`crates/horizon-agentd/src/
/// main.rs`'s `run`). Best-effort: nothing downstream depends on this
/// succeeding -- a file left behind after a crash is already handled by
/// [`listener::bind`]'s own stale-socket detection on the next start.
pub(crate) fn shutdown() {
    let _ = std::fs::remove_file(default_socket_path());
}
