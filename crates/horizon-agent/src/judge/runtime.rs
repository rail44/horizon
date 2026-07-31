//! The dedicated tokio runtime every enforcing-judge call is spawned onto.
//!
//! `horizon-agentd` starts the gate after deriving a typed approval candidate on its
//! plain `crossbeam_channel`-driven session thread. That thread must remain
//! free to handle cancellation while the prompt is held, so completion is
//! sent back through the session's `ToolCompletion` channel. `Runtime::spawn`
//! (unlike `block_on`) works from any calling thread regardless of whether
//! that thread is already inside some *other* tokio runtime's context, so a
//! plain lazily-started shared runtime is all this needs -- mirroring
//! `tools::network`'s own `network_runtime()` for the identical reason
//! (most sessions never reach a boundary crossing at all, so a process that
//! never fires the judge pays nothing).
use std::sync::OnceLock;

pub(super) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("horizon-agent-judge")
            .enable_all()
            .build()
            .expect("failed to build the shared judge runtime")
    })
}
