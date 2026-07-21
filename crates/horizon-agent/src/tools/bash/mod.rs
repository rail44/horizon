//! The `bash` tool (`docs/agent-tools-design.md`, "Bash Semantics"): a fresh
//! `bash -c` process per call, with the working directory tracked by the
//! harness across calls (not a persistent shell). Unlike `fs.write`/
//! `fs.edit` — the other `RequireApproval` tools Horizon executes app-side —
//! a bash command can run for up to its timeout (120s default, 600s hard
//! cap), so it can never run synchronously on the UI thread the way those
//! do. See `agent::tools::approval::ApprovalOutcome::Started` for the split
//! this forces: approval folds a "running" frame immediately and kicks off
//! this module's `spawn`, whose eventual result is delivered back to the
//! session loop over a channel (`crates/horizon-sessiond/src/session.rs`
//! wires it up) rather than being returned synchronously.
//!
//! Panic safety: a job's work function running to completion without
//! panicking is not something the rest of this module can assume. If it
//! panicked uncaught, two things would break at once -- the approved tool
//! call would never get a `ToolCallFinished` (nothing left to send the
//! `BashCompletion` that would produce one), and `registry`'s per-session
//! FIFO would never `advance` past it (see that module's own panic-safety
//! notes), wedging every later bash call for the session behind it forever.
//! `spawn` catches a panic from its work function (`run_job_body`, below)
//! specifically to prevent the first; `registry::run_job`'s advance-on-drop
//! guard prevents the second independently, as defense in depth for any
//! future job that doesn't route through `run_job_body`.

mod exec;
mod output;
mod registry;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use serde_json::Value;

use crate::config::BashToolConfig;
use crate::contract::{SessionId, ToolCallId, ToolCallResult};
use crate::frame::AgentFrame;
use crate::policy::{
    annotate_auto_approval, annotate_domain_approval, annotate_filesystem_approval,
    annotate_sandboxed,
};
use crate::tools::network::SessionNetworkProxy;

/// A bash call's outcome, delivered from the background thread that ran it
/// back to the session loop. `crates/horizon-sessiond/src/session.rs`
/// registers an unbounded `crossbeam_channel` per session (see
/// `register_session_runtime`) and selects on it alongside provider events,
/// folding a received completion into the session's `LiveState`/`Frames`
/// via `fold_bash_completion`.
#[derive(Clone, Debug)]
pub enum ToolCompletion {
    /// The call actually finished (successfully or not) -- fold
    /// `ToolCallFinished` and forward the result to the provider, exactly
    /// what every bash call did before this type grew a second variant.
    Finished(ToolCallResult),
    /// A sandboxed attempt's network egress was refused by the allowlist
    /// proxy for one or more domains (`docs/agent-approval-design.md` leg
    /// 4b). The call actually ran to completion (`result` is a genuine,
    /// already-computed outcome), but could not reach some host(s). Detected
    /// proxy-side (`SessionNetworkProxy::
    /// drain_denied_hosts`), independent of the sandboxed child's own exit
    /// code -- see `exec::run_sandboxed`'s doc comment for why that matters
    /// (backlog 59). Surfaced as a fresh, differently-named approval offer
    /// ("allow domain X for this session and retry"): approving adds
    /// `domains` to this session's allowlist and reruns the same call,
    /// still sandboxed; denying forwards `result` as-is.
    DomainDenied {
        call_id: ToolCallId,
        domains: Vec<String>,
        result: ToolCallResult,
    },
    /// A host-side web request discovered a valid next hop whose domain has
    /// not been granted to this session. No contact with that domain has
    /// occurred. Sessiond turns this into `ApprovalKind::DomainGrant`, and
    /// an approval retries the same tool call from its original URL.
    DomainGrantRequired {
        call_id: ToolCallId,
        domains: Vec<String>,
    },
    FilesystemDenied {
        call_id: ToolCallId,
        denials: Vec<horizon_sandbox::FilesystemDenial>,
        result: ToolCallResult,
    },
}

/// Compatibility name for the bash module's existing callers. New async
/// Horizon-owned tools use [`ToolCompletion`] directly; both names refer to
/// the same per-session completion channel.
pub type BashCompletion = ToolCompletion;

/// What a [`spawn_sandboxed`] run's eventual `Finished` completion should be
/// annotated with, once it lands -- distinguishes a genuine tier-1
/// auto-approval from a human's domain-denial-retry approval (`docs/
/// agent-approval-design.md` leg 4b), so the audit trail never claims a
/// human decision was auto-approved. Never consulted for a
/// `DomainDenied`/`FilesystemDenied` completion -- both are annotated by
/// `exec::run_sandboxed` itself before this ever applies.
#[derive(Clone, Debug)]
pub(crate) enum SandboxedApprovalOrigin {
    /// `tools::execution::execute_tier1_bash`'s auto-approval path.
    Tier1Auto,
    /// `tools::approval`'s domain-denial-retry approve path -- a human
    /// decision, carrying the domain(s) they just approved for this
    /// session.
    ManualDomainRetry { domains: Vec<String> },
    ManualFilesystemRetry {
        grants: Vec<horizon_sandbox::FilesystemGrant>,
    },
}

/// Kicks off a bash call and returns immediately; the UI thread must not
/// block waiting on this. `cwd` is the session's shared, tracked bash
/// working directory (`ToolSessionState::bash_cwd_handle`) — read at the
/// start of the call and updated in place if the command `cd`s, so the next
/// call in this session picks it up. `result_tx` delivers the finished
/// `BashCompletion` back to the UI thread; see the module doc for the full
/// round trip. `config` carries the timeout/output-cap/drain-grace knobs
/// (`agent::config::BashToolConfig`, `[agent]` in the config file) — a
/// plain `Copy` value rather than the `Rc`-based `ToolSessionState` it was
/// read from, since it has to cross onto the background thread this
/// eventually runs on.
///
/// Bash containment (`docs/agent-tools-design.md`, "Bash Containment"):
/// rather than spawning a fresh thread unconditionally, this hands the call
/// to `registry::enqueue`, which runs it immediately if `session_id` has no
/// other bash call in flight, or queues it (FIFO) behind whatever is
/// already running for that session — a session's bash calls never run
/// concurrently with each other.
pub fn spawn(
    session_id: SessionId,
    call_id: ToolCallId,
    input: Value,
    cwd: Arc<Mutex<PathBuf>>,
    config: BashToolConfig,
    result_tx: Sender<BashCompletion>,
) {
    registry::enqueue(
        session_id,
        Box::new(move || {
            let run_call_id = call_id.clone();
            run_job_body(session_id, call_id, &result_tx, move || {
                let mut output = exec::run(&run_call_id, &input, &cwd, &config);
                // Honest either way (`docs/agent-approval-design.md`'s
                // "Audit"): this path never engages the sandbox, whether
                // it's an ordinary manual approval or a retry-without-
                // sandbox rerun after a tier-1 denial.
                annotate_sandboxed(&mut output, false);
                BashCompletion::Finished(ToolCallResult::new(run_call_id.clone(), output))
            });
        }),
    );
}

/// Kicks off a *sandboxed* bash call (`docs/agent-approval-design.md`'s tier
/// 1: auto-approved `bash` in an isolated, sandbox-engaged session) and
/// returns immediately, same contract as [`spawn`]. `workspace_root` is
/// `horizon_sandbox`'s only writable root. The host's real temp dir is
/// deliberately *not* added as a writable root here -- a 2026-07 dogfooding
/// incident found that doing so made the whole shared host `/tmp` writable
/// from inside every sandboxed call (see `exec::run_sandboxed`'s doc comment
/// for the containment story). `output::spill`'s own write happens
/// host-side, after this call's output has already been captured over a
/// pipe, never from inside the sandboxed child -- it needs no writable-root
/// grant at all. Still goes through the same per-session FIFO
/// (`registry::enqueue`) as [`spawn`] -- a session's bash calls never run
/// concurrently with each other regardless of which path started them.
/// Structured network/filesystem denials are returned as their dedicated
/// completion variants while containment remains enabled.
///
/// `network` (`docs/agent-approval-design.md` leg 4b) is this session's own
/// `SessionNetworkProxy`, if one is running -- `Some` gives the sandbox its
/// exact loopback TCP proxy endpoint, `None` falls back to
/// `NetworkPolicy::Disabled` (see `exec::run_sandboxed`). `origin`
/// says whether this run is a tier-1 auto-approval or a human's
/// domain-denial-retry approve -- see [`SandboxedApprovalOrigin`] -- so the
/// eventual `Finished` result is annotated honestly either way.
#[allow(clippy::too_many_arguments)]
pub fn spawn_sandboxed(
    session_id: SessionId,
    call_id: ToolCallId,
    input: Value,
    cwd: Arc<Mutex<PathBuf>>,
    config: BashToolConfig,
    workspace_root: PathBuf,
    network: Option<Arc<SessionNetworkProxy>>,
    origin: SandboxedApprovalOrigin,
    filesystem_grants: Vec<horizon_sandbox::FilesystemGrant>,
    result_tx: Sender<BashCompletion>,
) {
    registry::enqueue(
        session_id,
        Box::new(move || {
            let run_call_id = call_id.clone();
            // `network` (an `Option<Arc<SessionNetworkProxy>>`) holds its
            // own locks/oneshot channels internally, which makes it
            // interior-mutable enough that rustc's auto `UnwindSafe` check
            // refuses it by default. `run_sandboxed` below only ever reads
            // through it (`network.as_deref()`, then a couple of narrow,
            // self-locking accessor calls) -- it never holds one of
            // `SessionNetworkProxy`'s own locks across a point that could
            // panic mid-mutation, so a caught panic here can't leave it in
            // an inconsistent state for the next call on the same session
            // to observe. Same reasoning `run_job_body`'s own doc comment
            // already gives for why this closure shape is safe to assert.
            let work = std::panic::AssertUnwindSafe(move || {
                let mut completion = exec::run_sandboxed(
                    &run_call_id,
                    &input,
                    &cwd,
                    &workspace_root,
                    network.as_deref(),
                    &filesystem_grants,
                    &config,
                );
                // Audit marker on every finished result this path produces
                // -- `DomainDenied`/`FilesystemDenied` outcomes are
                // already annotated by `exec::run_sandboxed` itself (a
                // `DomainDenied` result already carries `denied_domains`;
                // there is nothing left for `origin` to add there).
                if let BashCompletion::Finished(result) = &mut completion {
                    match &origin {
                        SandboxedApprovalOrigin::Tier1Auto => annotate_auto_approval(
                            &mut result.output,
                            "contained",
                            "isolated worktree session with an engaged sandbox",
                        ),
                        SandboxedApprovalOrigin::ManualDomainRetry { domains } => {
                            annotate_domain_approval(&mut result.output, domains)
                        }
                        SandboxedApprovalOrigin::ManualFilesystemRetry { grants } => {
                            annotate_filesystem_approval(&mut result.output, grants)
                        }
                    }
                }
                completion
            });
            run_job_body(session_id, call_id, &result_tx, work);
        }),
    );
}

/// Runs `work` (in practice, `exec::run`/`exec::run_sandboxed`) and *always*
/// sends a `BashCompletion` on `result_tx` -- even if `work` panics. This is
/// the fix for the "answered -- running..." wedge a bare panic used to
/// cause: without catching it here, a panic on this job's thread would skip
/// the `result_tx.send` below entirely, so the approved tool call never gets
/// a `ToolCallFinished` and stays stuck forever. Catching also means this
/// function itself returns normally, so the job closure `registry::run_job`
/// spawned returns normally too and `advance` still fires on schedule --
/// the FIFO doesn't wedge on the *next* call either.
///
/// `work` must be `UnwindSafe`: both `spawn`'s and `spawn_sandboxed`'s call
/// sites wrap a plain `FnOnce` closure (no shared/interior-mutable state
/// visible to the closure that catching a panic mid-mutation could leave
/// inconsistent), so this is a real guarantee, not an assertion papered
/// over. A panic always resolves to `BashCompletion::Finished` (never a
/// retry-without-sandbox prompt) -- a harness panic isn't a sandbox denial.
fn run_job_body(
    session_id: SessionId,
    call_id: ToolCallId,
    result_tx: &Sender<BashCompletion>,
    work: impl FnOnce() -> BashCompletion + std::panic::UnwindSafe,
) {
    let completion = match std::panic::catch_unwind(work) {
        Ok(completion) => completion,
        Err(payload) => {
            // `&*payload`, not `&payload`: `payload` is a `Box<dyn Any +
            // Send>`, and coercing `&Box<dyn Any + Send>` straight to
            // `&(dyn Any + Send)` unsizes the *Box* itself into the trait
            // object (its own, distinct `Any` impl) rather than derefing
            // through to the payload inside -- every `downcast_ref` would
            // silently miss. Deref first so the trait object is built from
            // the actual payload.
            let message = panic_payload_message(&*payload);
            eprintln!("bash worker panicked (session {session_id:?}, call {call_id:?}): {message}");
            BashCompletion::Finished(ToolCallResult::new(
                call_id,
                exec::panic_output(&format!("bash worker panicked: {message}")),
            ))
        }
    };
    let _ = result_tx.send(completion);
}

/// Extracts a human-readable message from a caught panic's payload. Panic
/// payloads are almost always `&'static str` (a string-literal panic
/// message) or `String` (a formatted one, e.g. from `panic!("{x}")`) --
/// anything else is an unusual payload type (`panic_any` with a custom
/// type), which this reports generically rather than failing to build a
/// completion at all.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Kills the running child for `call_id`, if this session has one in
/// flight, and removes it from the registry. A no-op if `call_id` isn't a
/// currently-running bash call — safe to call unconditionally for every
/// provider-originated `ToolCallFinished` (see `agent::tools::processing`),
/// since a cancelled turn's synthetic `ToolCallFinished` is exactly the
/// signal that a still-running bash child needs to be killed.
pub fn kill_if_running(call_id: &ToolCallId) {
    registry::kill(call_id);
}

/// Whether a finished bash call's result should still be folded into the
/// session's frame — `false` if `call_id` already has a `ToolCallFinished`
/// there. A cancellation racing this completion (see `kill_if_running` and
/// `agent::tools::processing`) can beat it to the frame, in which case the
/// late, genuine result is accepted and discarded — the same idempotence
/// pattern `agent::tools::approval`'s `ApprovalOutcome::AlreadyResolved`
/// uses for a duplicate approve/deny. Called from
/// `horizon_sessiond::session::fold_bash_completion`, on the session loop,
/// right before folding.
pub fn should_fold_completion(frame: &AgentFrame, call_id: &ToolCallId) -> bool {
    !frame.has_tool_call_finished(call_id)
}

#[cfg(test)]
mod tests;
