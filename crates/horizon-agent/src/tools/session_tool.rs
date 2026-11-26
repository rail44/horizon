//! Tools a session's client hands it, as an MCP server's tools reach a
//! session named in ACP's `session/new` (`docs/agent-sessions-on-demand-design.md`).
//! They are offered to that session alone, beside the built-in ones: a
//! session's packages carry them (`Packages::with_session_tools`).
//!
//! A tool its server says only reads runs without asking; any other call is
//! put to the verdict stage as a call reaching past the session's
//! containment, so a person or the judge decides first.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use futures_util::FutureExt;
use serde_json::Value;

use super::completion::ToolCompletion;
use super::input::PreparedCall;
use super::output::{Response, error as error_output};
use super::tool::{Dispatch, RunContext};
use crate::contract::{ApprovalKind, SessionId, ToolPermission};

/// Runs one call with its arguments: `Ok` is the result as the model sees
/// it, `Err` a message for the model, the call recorded as failed.
pub type SessionToolCall =
    Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

#[derive(Clone)]
pub struct SessionTool {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    /// Only reads, as the tool's server says: it runs without asking.
    pub read_only: bool,
    pub call: SessionToolCall,
}

impl std::fmt::Debug for SessionTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTool")
            .field("id", &self.id)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

/// The session's tools' catalog entries, in the order they were handed.
pub(crate) fn definitions(packages: &crate::packages::Packages) -> Vec<super::Definition> {
    packages.session_tools().iter().map(definition_of).collect()
}

fn definition_of(tool: &SessionTool) -> super::Definition {
    super::Definition {
        id: tool.id.clone(),
        title: tool.title.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
        permission: if tool.read_only {
            ToolPermission::AutoAllowRead
        } else {
            ToolPermission::RequireApproval
        },
    }
}

/// The session's tool `tool_id` names, as a [`super::tool::Tool`].
pub(crate) fn find(
    packages: &crate::packages::Packages,
    tool_id: &str,
) -> Option<Arc<dyn super::tool::Tool>> {
    let tool = (packages.session_tools().iter())
        .find(|tool| tool.id == tool_id)?
        .clone();
    Some(Arc::new(Handed {
        definition: definition_of(&tool),
        tool,
    }))
}

/// Whether `tool_id` is one of the session's tools.
pub(crate) fn is_session_tool(packages: &crate::packages::Packages, tool_id: &str) -> bool {
    (packages.session_tools().iter()).any(|tool| tool.id == tool_id)
}

struct Handed {
    definition: super::Definition,
    tool: SessionTool,
}

impl super::tool::Tool for Handed {
    fn definition(&self) -> &super::Definition {
        &self.definition
    }

    fn reach(
        &self,
        _state: &super::ToolSessionState,
        call: &PreparedCall<'_>,
    ) -> crate::modules::Reach {
        let containment = if self.tool.read_only {
            crate::modules::Containment::Inside
        } else {
            crate::modules::Containment::Uncontained
        };
        crate::modules::Reach {
            permission: Some(self.definition.permission),
            containment,
            ask: (!self.tool.read_only).then(|| {
                (
                    format!(
                        "`{}` ({}) is a tool the session's client handed it. Allow this call?",
                        call.tool_id, self.tool.title
                    ),
                    ApprovalKind::Standard,
                )
            }),
            uncontained_write: false,
            primary_checkout: None,
            git_metadata: None,
        }
    }

    fn run(&self, cx: &RunContext<'_>, call: &PreparedCall<'_>) -> Dispatch {
        let Some(runtime) = super::state::session_runtime(cx.session_id) else {
            return error_output(format!(
                "`{}` was not run: the session's runtime is gone.",
                call.tool_id
            ))
            .into();
        };
        spawn(cx.session_id, &self.tool, call, runtime.async_results);
        Dispatch::Spawned
    }

    /// An approved call is run here (`tools::approval`), as an automatic
    /// one is.
    fn resolves_approvals(&self) -> bool {
        true
    }
}

/// Runs an approved call to the session's tool `request.tool_id`.
pub(crate) fn spawn_approved(
    session_id: SessionId,
    packages: &crate::packages::Packages,
    request: &PreparedCall<'_>,
    result_tx: crossbeam_channel::Sender<ToolCompletion>,
) -> bool {
    let Some(tool) = (packages.session_tools().iter()).find(|tool| tool.id == request.tool_id)
    else {
        return false;
    };
    spawn(session_id, tool, request, result_tx);
    true
}

fn spawn(
    session_id: SessionId,
    tool: &SessionTool,
    request: &PreparedCall<'_>,
    result_tx: crossbeam_channel::Sender<ToolCompletion>,
) {
    let identity = request.identity();
    let input = request.request.input.0.clone();
    let registration = super::background::Registration::new(
        session_id,
        super::background::Lifetime::Call(identity.clone()),
        super::background::WorkKind::Tool,
    );
    let token = registration.token();
    let call = (tool.call)(input);
    let tool_id = tool.id.clone();
    runtime().spawn(async move {
        let work = AssertUnwindSafe(call).catch_unwind();
        let output = tokio::select! {
            biased;
            _ = token.cancelled() => None,
            result = work => Some(match result {
                Ok(Ok(value)) => Response::external(value),
                Ok(Err(message)) => error_output(message),
                Err(payload) => error_output(format!(
                    "{tool_id} panicked: {}",
                    crate::runtime_panic::panic_message(&*payload)
                )),
            }),
        };
        if registration.finish()
            && let Some(output) = output
        {
            let _ = result_tx.send(ToolCompletion::Finished(identity.finish(output)));
        }
    });
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("horizon-agent-session-tools")
            .build()
            .expect("session tools runtime")
    })
}
