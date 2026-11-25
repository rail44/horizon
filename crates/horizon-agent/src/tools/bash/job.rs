//! Snapshot thread-safe job inputs, enqueue them, and annotate their completion.

use super::exec::{self, Execution};
use super::{ApprovalSource, BashCompletion, HostExecutionApproval, registry};
use crate::config::BashToolConfig;
use crate::contract::{SessionId, ToolCallIdentity};
use crate::tools::ToolSessionState;
use crate::tools::input::{Bash, PreparedCall};
use crate::tools::output::Response;
use crate::tools::output::{
    annotate_approval_source, annotate_auto_approval, annotate_domain_approval,
    annotate_host_execution_approval, annotate_sandboxed,
};
use crossbeam_channel::Sender;
use horizon_sandbox::runtime::Containment;
use horizon_sandbox::{FilesystemGrantAccess, FilesystemGrantScope};
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

/// Why a contained call runs without asking.
#[derive(Clone, Debug)]
pub(crate) enum ContainedOrigin {
    /// Tier 1: inside the sandbox, nothing to ask.
    Automatic,
    /// The hosts it carried were approved, for this call only.
    Domains {
        source: ApprovalSource,
        domains: Vec<String>,
    },
}

/// What one contained call of this session may do
/// (`docs/sandbox-runtime-design.md`, "Per-call containment"): the
/// workspace unless it is a primary checkout the session may not write or
/// the session is read-only; the configured and own-worktree grants, with
/// their excluded subpaths read-only; the workspace's own `.git` pointer
/// file read-only; the session's domains plus `extra_domains`.
pub(crate) fn containment(
    tools: &ToolSessionState,
    workspace_root: &Path,
    extra_domains: &[String],
) -> Containment {
    let mut containment = Containment::default();
    if tools.read_only().is_none() {
        if tools.primary_checkout_writable() {
            containment.writable.push(workspace_root.to_path_buf());
            let dot_git = workspace_root.join(".git");
            if dot_git.is_file() {
                containment.read_only.push(dot_git);
            }
        }
        for grant in tools.effective_sandbox_grants() {
            if grant.access != FilesystemGrantAccess::ReadWrite {
                continue;
            }
            for excluded in &grant.excluded_subpaths {
                let path = grant.path.join(excluded);
                if path.exists() {
                    containment.read_only.push(path);
                }
            }
            if grant.scope == FilesystemGrantScope::File || grant.path.exists() {
                containment.writable.push(grant.path);
            }
        }
    }
    containment.domains = tools.allowed_domains();
    for domain in extra_domains {
        if !containment.domains.contains(domain) {
            containment.domains.push(domain.clone());
        }
    }
    containment.loopback_ports = tools
        .loopback_connect()
        .iter()
        .filter(|endpoint| endpoint.ip().is_loopback())
        .map(std::net::SocketAddr::port)
        .collect();
    containment.mach_services = tools.mach_services();
    containment
}

/// Enqueue a contained run of `request`, with the session's containment
/// as it is now plus `origin`'s approved hosts.
pub(crate) fn spawn_contained(
    session_id: SessionId,
    request: &PreparedCall<'_>,
    tools: &ToolSessionState,
    result_tx: Sender<BashCompletion>,
    workspace_root: &Path,
    origin: ContainedOrigin,
) {
    let extra: &[String] = match &origin {
        ContainedOrigin::Automatic => &[],
        ContainedOrigin::Domains { domains, .. } => domains,
    };
    let mut containment = containment(tools, workspace_root, extra);
    containment.temp_dir = crate::tools::sandbox::session_temp_dir(session_id);
    let execution = Execution::Contained {
        runtime: crate::tools::sandbox::current(),
        containment,
    };
    let job = BashJob::new(session_id, request, tools, result_tx);
    // The runtime is shared and stateless across a caught panic.
    let execution = std::panic::AssertUnwindSafe(execution);
    job.enqueue(move |job, registration| {
        let execution = execution;
        let mut output = exec::run(
            registration,
            &job.input,
            &job.cwd,
            &job.config,
            &execution.0,
        );
        match &origin {
            ContainedOrigin::Automatic => annotate_auto_approval(
                &mut output,
                "contained",
                "ran in the sandbox, confined to the workspace",
            ),
            ContainedOrigin::Domains { source, domains } => {
                annotate_domain_approval(&mut output, domains);
                annotate_approval_source(&mut output, source.label());
            }
        }
        deliver(job, output)
    });
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
/// authority. The elevation is call-scoped: the next bash call starts
/// contained again.
pub(crate) fn spawn_approved_host(job: BashJob, approval: HostExecutionApproval) {
    spawn_host(job, Some(approval));
}

fn spawn_host(job: BashJob, approval: Option<HostExecutionApproval>) {
    job.enqueue(move |job, registration| {
        let mut output = exec::run(
            registration,
            &job.input,
            &job.cwd,
            &job.config,
            &Execution::Host,
        );
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
        deliver(job, output)
    });
}

fn deliver(job: &BashJob, output: Response) -> BashCompletion {
    BashCompletion::Finished(job.identity.finish(output))
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
            let message = crate::runtime_panic::panic_message(&*payload);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentToolsConfig;
    use crate::contract::ToolCallId;
    use crate::tools::{RecallContext, session_tool_work_settled};
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
