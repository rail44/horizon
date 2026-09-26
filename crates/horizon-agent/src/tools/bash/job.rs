//! Snapshot thread-safe job inputs, enqueue them, and annotate their completion.

use super::{
    exec, git, registry, ApprovalSource, BashCompletion, HostExecutionApproval,
    SandboxedApprovalOrigin,
};
use crate::config::BashToolConfig;
use crate::contract::{SessionId, ToolCallIdentity};
use crate::tools::input::{Bash, PreparedCall};
use crate::tools::network::SessionNetworkProxy;
use crate::tools::output::{
    annotate_auto_approval, annotate_domain_approval, annotate_filesystem_grant_approval,
    annotate_git_operation_approval, annotate_host_execution_approval, annotate_sandboxed,
};
use crate::tools::output::{error as error_output, Response};
use crate::tools::ToolSessionState;
use crossbeam_channel::Sender;
#[cfg(test)]
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Values that can cross from the session thread into its bash FIFO. The cwd
/// handle stays shared so a queued command observes earlier commands' `cd`.
pub(crate) struct BashJob {
    session_id: SessionId,
    identity: ToolCallIdentity,
    input: Bash,
    cwd: Arc<Mutex<PathBuf>>,
    config: BashToolConfig,
    result_tx: Sender<BashCompletion>,
}

impl BashJob {
    pub(crate) fn new(
        session_id: SessionId,
        request: &PreparedCall<'_>,
        tools: &ToolSessionState,
        result_tx: Sender<BashCompletion>,
    ) -> Self {
        Self {
            session_id,
            identity: request.identity(),
            input: request.input.bash().clone(),
            cwd: tools.bash_cwd_handle(),
            config: tools.bash_config(),
            result_tx,
        }
    }

    /// Count queued work before enqueueing and hold it through result delivery.
    /// Both execution modes use this same FIFO and panic/completion boundary.
    fn enqueue(
        self,
        work: impl FnOnce(&Self, &registry::Registration) -> BashCompletion
            + Send
            + std::panic::UnwindSafe
            + 'static,
    ) {
        let registration =
            registry::Registration::for_execution(self.session_id, self.identity.clone());
        registry::enqueue(
            self.session_id,
            Box::new(move || {
                if registration.is_cancelled() {
                    return;
                }
                run_job_body(
                    self.session_id,
                    self.identity.clone(),
                    &self.result_tx,
                    std::panic::AssertUnwindSafe(|| work(&self, &registration)),
                    || registration.finish(),
                );
            }),
        );
    }
}

/// Per-run confinement captured after approval has updated session grants.
/// Git metadata is revalidated when the FIFO actually starts this job.
pub(crate) struct SandboxedRun {
    workspace_root: PathBuf,
    network: Option<Arc<SessionNetworkProxy>>,
    loopback_connect: Vec<std::net::SocketAddr>,
    filesystem_grants: Vec<horizon_sandbox::FilesystemGrant>,
    origin: SandboxedApprovalOrigin,
    git_metadata_roots: Option<Vec<PathBuf>>,
}

impl SandboxedRun {
    pub(crate) fn new(
        tools: &ToolSessionState,
        workspace_root: &Path,
        origin: SandboxedApprovalOrigin,
        git_metadata_roots: Option<Vec<PathBuf>>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.to_path_buf(),
            network: tools.network_proxy(),
            loopback_connect: tools.loopback_connect(),
            filesystem_grants: tools.effective_sandbox_grants(),
            origin,
            git_metadata_roots,
        }
    }

    fn validated_grants(&self) -> Result<Vec<horizon_sandbox::FilesystemGrant>, String> {
        let mut grants = self.filesystem_grants.clone();
        if let Some(roots) = &self.git_metadata_roots {
            for grant in git::validated_metadata_grants(&self.workspace_root, roots)? {
                if !grants.contains(&grant) {
                    grants.push(grant);
                }
            }
        }
        Ok(grants)
    }
}

fn run_sandboxed_job(
    job: &BashJob,
    registration: &registry::Registration,
    sandbox: SandboxedRun,
) -> BashCompletion {
    let mut completion = match sandbox.validated_grants() {
        Ok(grants) => exec::run_sandboxed(
            registration,
            &job.identity,
            &job.input,
            &job.cwd,
            &sandbox.workspace_root,
            sandbox.network.as_deref(),
            &sandbox.loopback_connect,
            &grants,
            &job.config,
        ),
        Err(error) => {
            let mut output = error_output(format!(
                "Git metadata grant validation failed before execution: {error}"
            ));
            annotate_sandboxed(&mut output, false);
            crate::tools::ToolCompletion::Finished(output)
        }
    };
    if let (Some(roots), Some(result)) = (
        sandbox.git_metadata_roots.as_deref(),
        completion.result_mut(),
    ) {
        annotate_git_operation_approval(result, roots);
    }
    // Denial variants already carry their evidence; origin markers apply only
    // to Finished, matching the approval audit contract.
    if let crate::tools::ToolCompletion::Finished(result) = &mut completion {
        sandbox.origin.annotate(result);
    }
    completion.map_result(|output| job.identity.finish(output))
}

impl SandboxedApprovalOrigin {
    fn annotate(&self, output: &mut Response) {
        match self {
            SandboxedApprovalOrigin::Tier1Auto => annotate_auto_approval(
                output,
                "contained",
                "isolated worktree session with an engaged sandbox",
            ),
            SandboxedApprovalOrigin::ManualDomainRetry { domains } => {
                annotate_domain_approval(output, domains)
            }
            SandboxedApprovalOrigin::ManualGitOperation => {}
            SandboxedApprovalOrigin::FilesystemGrant {
                source,
                grants,
                trigger_paths,
            } => {
                annotate_filesystem_grant_approval(output, source.label(), grants, trigger_paths);
                if *source == ApprovalSource::Judge {
                    annotate_auto_approval(
                        output,
                        "judge",
                        "judge approved a scoped filesystem grant for a \
                                     sandboxed retry",
                    );
                }
            }
            SandboxedApprovalOrigin::MachServiceGrant { services } => {
                crate::tools::output::annotate_mach_service_grant_approval(output, services);
            }
        }
    }
}

/// Test entry point for a host job without a session's approval machinery.
/// Uses the same FIFO, cwd tracking, and completion boundary as approved jobs.
#[cfg(test)]
pub(super) fn spawn(
    session_id: SessionId,
    call_id: crate::contract::ToolCallId,
    input: Value,
    cwd: Arc<Mutex<PathBuf>>,
    config: BashToolConfig,
    result_tx: Sender<BashCompletion>,
) {
    spawn_host(
        BashJob {
            session_id,
            identity: crate::test_support::tool_identity(&call_id),
            input: serde_json::from_value(input).expect("valid bash fixture"),
            cwd,
            config,
            result_tx,
        },
        None,
    );
}

/// Runs one explicitly approved call with the host process's ordinary
/// authority. The elevation is call-scoped: the next bash call starts from
/// the normal sandbox policy again.
pub(crate) fn spawn_approved_host(job: BashJob, approval: HostExecutionApproval) {
    spawn_host(job, Some(approval));
}

fn spawn_host(job: BashJob, approval: Option<HostExecutionApproval>) {
    job.enqueue(move |job, registration| {
        let mut output = exec::run(registration, &job.input, &job.cwd, &job.config);
        // Honest either way (`docs/agent-approval-design.md`'s
        // "Audit"): this path never engages the sandbox. Host
        // execution is not represented as sandboxed merely because
        // it followed an approval. Explicitly mediated calls receive
        // additional scope and source markers below.
        annotate_sandboxed(&mut output, false);
        if let Some(approval) = &approval {
            annotate_host_execution_approval(&mut output, approval.source.label());
            if approval.source == ApprovalSource::Judge {
                annotate_auto_approval(
                    &mut output,
                    "judge",
                    "judge approved one call with host execution authority",
                );
            }
        }
        BashCompletion::Finished(job.identity.finish(output))
    });
}

/// Enqueue a sandboxed run with its captured session grants and approval origin.
/// The child receives the workspace root and explicitly granted paths; host
/// output spilling does not grant the child access to the host's temp dir.
/// `SandboxedRun::network` selects the session proxy or disabled networking.
/// Denials retain their evidence; only Finished receives origin annotations.
pub(crate) fn spawn_sandboxed(job: BashJob, sandbox: SandboxedRun) {
    // The proxy's accessors synchronize internally; no proxy lock is held
    // across a caught panic. The following job can safely reuse the proxy.
    let sandbox = std::panic::AssertUnwindSafe(sandbox);
    job.enqueue(move |job, registration| {
        let sandbox = sandbox;
        run_sandboxed_job(job, registration, sandbox.0)
    });
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
pub(super) fn run_job_body(
    session_id: SessionId,
    identity: ToolCallIdentity,
    result_tx: &Sender<BashCompletion>,
    work: impl FnOnce() -> BashCompletion + std::panic::UnwindSafe,
    accept: impl FnOnce() -> bool,
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
            eprintln!(
                "bash worker panicked (session {session_id:?}, execution {identity:?}): {message}"
            );
            BashCompletion::Finished(identity.finish(exec::panic_output(&format!(
                "bash worker panicked: {message}"
            ))))
        }
    };
    if accept() {
        let _ = result_tx.send(completion);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentToolsConfig;
    use crate::contract::ToolCallId;
    use crate::tools::{session_tool_work_settled, RecallContext};
    use crossbeam_channel::unbounded;
    use std::time::{Duration, Instant};

    #[test]
    fn queued_jobs_retain_work_and_observe_cwd_changes_even_after_a_panic() {
        let dir = std::env::temp_dir().join(format!("horizon-bash-job-{}", uuid::Uuid::new_v4()));
        let next = dir.as_path().join("next");
        std::fs::create_dir_all(&next).unwrap();
        let tools = ToolSessionState::for_root(
            dir.as_path().to_path_buf(),
            AgentToolsConfig::default(),
            RecallContext::default(),
        );
        let session_id = SessionId::new();
        let request = |id: &str| crate::contract::ToolCallRequest {
            call_id: (ToolCallId(id.into())).clone(),
            occurrence_id: crate::contract::OccurrenceId((ToolCallId(id.into())).0.clone()),
            tool_id: "bash".into(),
            input: serde_json::json!({"command": "pwd"}).into(),
        };
        let (results, completed) = unbounded();
        let (started, running) = unbounded();
        let (release, blocked) = unbounded();
        let first = BashJob::new(
            session_id,
            &PreparedCall::new(&request("first")).unwrap(),
            &tools,
            results.clone(),
        );
        first.enqueue(move |job, _registration| {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            *job.cwd.lock().unwrap() = next;
            panic!("first job failed after changing cwd");
        });
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = BashJob::new(
            session_id,
            &PreparedCall::new(&request("second")).unwrap(),
            &tools,
            results,
        );
        spawn_approved_host(second, HostExecutionApproval::new(ApprovalSource::Human));
        assert!(!session_tool_work_settled(session_id));
        assert!(completed.try_recv().is_err());
        release.send(()).unwrap();
        let BashCompletion::Finished(first) =
            completed.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("panic must produce a finished result");
        };
        assert_eq!(first.call_id.0, "first");
        assert!(first.is_error());
        let BashCompletion::Finished(second) =
            completed.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("second job must still run");
        };
        assert_eq!(second.call_id.0, "second");
        assert_eq!(second.output["exit_code"], 0);
        assert_eq!(
            second.output["output"].as_str().unwrap().trim(),
            dir.as_path()
                .join("next")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert_eq!(second.output["sandboxed"], false);
        assert_eq!(second.output["host_execution_approved"], true);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !session_tool_work_settled(session_id) {
            assert!(
                Instant::now() < deadline,
                "work guard survived job completion"
            );
            std::thread::yield_now();
        }
        assert!(completed.try_recv().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
