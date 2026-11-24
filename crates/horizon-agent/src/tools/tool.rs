//! One tool the model can call: what it offers the model, and how a call to
//! it runs once its plan lets it run without asking
//! (`docs/agent-core-extension-design.md`, "ツール"). The built-in tools are
//! below; a package's tools come in through [`super::external`].

use std::sync::{Arc, OnceLock};

use super::catalog::Definition;
use super::execution::{HostTools, ToolOutput};
use super::input::{PreparedCall, ToolInput};
use super::output::{Response, annotate_auto_approval, error as error_output};
use super::state::{ToolSessionState, session_runtime};
use crate::contract::SessionId;
use crate::policy::ToolPlan;

/// What a run is given besides the call itself.
pub(crate) struct RunContext<'a> {
    pub(crate) host: &'a dyn HostTools,
    pub(crate) state: &'a ToolSessionState,
    pub(crate) session_id: SessionId,
}

/// How a run ended: with its output, or handed to work that delivers the
/// result later on the session runtime's completion channel.
pub(crate) enum Dispatch {
    Done(ToolOutput),
    Spawned,
}

impl<T: Into<ToolOutput>> From<T> for Dispatch {
    fn from(output: T) -> Self {
        Self::Done(output.into())
    }
}

pub(crate) trait Tool: Send + Sync {
    fn definition(&self) -> &Definition;

    /// Whether a call runs without asking, waits for an approval, or is
    /// refused before it runs.
    fn plan(&self, state: &ToolSessionState, call: &PreparedCall<'_>) -> ToolPlan;

    /// Runs a call whose plan lets it run without asking
    /// (`policy::ToolPlan::Automatic`).
    fn run(&self, cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch;

    /// Whether Horizon carries out an approved or denied call to this tool
    /// itself (`tools::approval`), rather than forwarding the decision to
    /// the provider that owns the call.
    fn resolves_approvals(&self) -> bool {
        false
    }
}

/// The tool `tool_id` names: a built-in, then a registered one.
pub(crate) fn find(packages: &crate::packages::Packages, tool_id: &str) -> Option<Arc<dyn Tool>> {
    builtins()
        .iter()
        .find(|tool| tool.definition().id == tool_id)
        .cloned()
        .or_else(|| super::external::find(packages, tool_id))
}

fn builtins() -> &'static [Arc<dyn Tool>] {
    static TOOLS: OnceLock<Vec<Arc<dyn Tool>>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        super::catalog::builtin_definitions()
            .iter()
            .cloned()
            .map(builtin)
            .collect()
    })
}

/// The built-in tool a catalog entry describes.
fn builtin(definition: Definition) -> Arc<dyn Tool> {
    let run: RunFn = match definition.id.as_str() {
        "fs_read" | "fs_glob" | "fs_grep" | "skill_read" | "recall_search" | "recall_read"
        | "recall_sessions" | "knowledge_read" | "knowledge_write" => run_local,
        "fs_write" | "fs_edit" => run_contained_write,
        "bash" => run_sandboxed_bash,
        "web_search" | "web_fetch" | "mock_boundary_crossing" => run_web,
        "task" => run_task,
        "task_output" => run_task_output,
        "worktree_enter" => |cx, _| super::worktree::enter(cx.host, cx.session_id).into(),
        "board_read" => run_board_read,
        "board_update" | "board_session" => {
            |cx, call| super::board::execute_operation(cx.state, cx.session_id, call).into()
        }
        "board_comment" => {
            |cx, call| super::board::execute_comment(cx.state, cx.session_id, call).into()
        }
        "workspace_snapshot" => run_host,
        // Always planned for approval: nothing runs without asking.
        _ => run_unavailable,
    };
    Arc::new(Builtin { definition, run })
}

type RunFn = fn(&RunContext<'_>, &PreparedCall<'_>) -> Dispatch;

struct Builtin {
    definition: Definition,
    run: RunFn,
}

impl Tool for Builtin {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn plan(&self, state: &ToolSessionState, call: &PreparedCall<'_>) -> ToolPlan {
        crate::policy::plan_builtin(state, call)
    }

    fn run(&self, cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
        (self.run)(cx, call)
    }

    fn resolves_approvals(&self) -> bool {
        super::approval::is_horizon_executed_tool(&self.definition.id)
    }
}

fn run_local(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    super::synchronous::execute(cx.state, &call.input, false)
        .unwrap_or_else(|| error_output(format!("`{}` has no local run", call.tool_id)))
        .into()
}

/// `fs_write`/`fs_edit` inside the workspace root, where the tool's own path
/// check confines the write.
fn run_contained_write(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    let mut output = super::execute_approved(cx.state, &call.input);
    annotate_auto_approval(&mut output, "contained", "confined to the workspace root");
    output.into()
}

fn run_sandboxed_bash(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    let Some(runtime) = session_runtime(cx.session_id) else {
        return no_runtime(call);
    };
    let Some(workspace_root) = cx.state.workspace_root() else {
        return error_output("sandboxed bash requires a workspace root").into();
    };
    super::bash::spawn_sandboxed_call(
        cx.session_id,
        call,
        cx.state,
        runtime.async_results,
        workspace_root,
        super::bash::SandboxedApprovalOrigin::Tier1Auto,
        None,
    );
    Dispatch::Spawned
}

fn run_web(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    let Some(runtime) = session_runtime(cx.session_id) else {
        return no_runtime(call);
    };
    super::web::spawn(
        cx.session_id,
        call,
        cx.state.domain_allowlist(),
        super::web::WebApprovalOrigin::Auto,
        runtime.async_results,
    );
    Dispatch::Spawned
}

fn run_task(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    match &call.input {
        ToolInput::Task(input) => {
            super::explore::start(cx.state, cx.session_id, call, input).into()
        }
        _ => run_unavailable(cx, call),
    }
}

fn run_task_output(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    match &call.input {
        ToolInput::TaskOutput(input) => super::explore::output(cx.session_id, call, input).into(),
        _ => run_unavailable(cx, call),
    }
}

fn run_board_read(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    super::board::execute_auto(cx.state, &call.tool_id, &call.request.input)
        .map(Response::external)
        .unwrap_or_else(|| error_output("board_read is unavailable for this session"))
        .into()
}

/// A tool whose effect lives in the host (`HostTools::execute_auto`).
fn run_host(cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    cx.host
        .execute_auto(&call.tool_id, &call.request.input)
        .map(Response::external)
        .unwrap_or_else(|| {
            error_output(format!(
                "`{}` is unavailable for this session",
                call.tool_id
            ))
        })
        .into()
}

fn run_unavailable(_cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
    error_output(format!(
        "Tool `{}` cannot be executed automatically.",
        call.tool_id
    ))
    .into()
}

fn no_runtime(call: &PreparedCall<'_>) -> Dispatch {
    error_output(format!(
        "{} has no registered session runtime",
        call.tool_id
    ))
    .into()
}
