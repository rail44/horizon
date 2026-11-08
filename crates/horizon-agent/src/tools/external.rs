//! Tools registered from crates `horizon-agent` does not depend on, the tool
//! counterpart of `roles::register_external` and
//! `skills::register_external_skill_sources`. A registering package supplies
//! each tool's definition and an executor written against plain types (a
//! session id, the session's roots, JSON in and out), so the package needs no
//! `horizon-agent` types; `horizon-agentd` is where packages are composed.
//!
//! Registering a tool declares it contained: it runs without an approval,
//! and the core takes the package's word for it. There is no approval path
//! for registered tools (`docs/project-wiki-design.md`, "Package layout").

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde_json::Value;

/// What an external tool's executor is given for one call.
#[derive(Debug)]
pub struct ExternalToolCall<'a> {
    pub session_id: uuid::Uuid,
    /// The session's working root, when it has one.
    pub workspace_root: Option<&'a Path>,
    /// The main repository toplevel of `workspace_root` (a linked worktree
    /// resolves to its main checkout), when it is inside a repository.
    pub project_root: Option<PathBuf>,
    pub input: &'a Value,
}

/// Runs one call. `Ok` is the tool's result as the model sees it; `Err` is a
/// message for the model, and the call is recorded as failed.
pub type ExternalToolExecutor =
    Arc<dyn Fn(&ExternalToolCall<'_>) -> Result<Value, String> + Send + Sync>;

#[derive(Clone)]
pub struct ExternalTool {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    /// Offered only to sessions whose project is in `trusted_projects`, as the
    /// knowledge tools are: what the tool reads can reach the prompt.
    pub requires_trusted_project: bool,
    pub execute: ExternalToolExecutor,
}

impl std::fmt::Debug for ExternalTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalTool")
            .field("id", &self.id)
            .field("requires_trusted_project", &self.requires_trusted_project)
            .finish_non_exhaustive()
    }
}

static EXTERNAL_TOOLS: RwLock<Vec<ExternalTool>> = RwLock::new(Vec::new());

/// Registers tools from another crate. Called at process startup, before any
/// session is spawned. Refuses the whole batch when an id is empty, repeats
/// within it, or is already taken by a built-in or a registered tool.
pub fn register_external_tools(tools: Vec<ExternalTool>) -> Result<(), String> {
    let mut registered = EXTERNAL_TOOLS.write().unwrap_or_else(|e| e.into_inner());
    let mut seen = std::collections::HashSet::new();
    for tool in &tools {
        if tool.id.trim().is_empty() {
            return Err("an external tool has an empty id".to_string());
        }
        let taken = super::catalog::is_builtin(&tool.id)
            || registered.iter().any(|existing| existing.id == tool.id);
        if taken || !seen.insert(tool.id.as_str()) {
            return Err(format!("tool id `{}` is already taken", tool.id));
        }
    }
    registered.extend(tools);
    Ok(())
}

fn with_tool<T>(tool_id: &str, f: impl FnOnce(&ExternalTool) -> T) -> Option<T> {
    let registered = EXTERNAL_TOOLS.read().unwrap_or_else(|e| e.into_inner());
    registered.iter().find(|tool| tool.id == tool_id).map(f)
}

pub(crate) fn is_registered(tool_id: &str) -> bool {
    with_tool(tool_id, |_| ()).is_some()
}

pub(crate) fn requires_trusted_project(tool_id: &str) -> bool {
    with_tool(tool_id, |tool| tool.requires_trusted_project).unwrap_or(false)
}

/// The registered tools' catalog entries, in registration order.
pub(crate) fn definitions() -> Vec<super::Definition> {
    let registered = EXTERNAL_TOOLS.read().unwrap_or_else(|e| e.into_inner());
    registered
        .iter()
        .map(|tool| super::Definition {
            id: tool.id.clone(),
            title: tool.title.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
            permission: crate::contract::ToolPermission::AutoAllowUi,
        })
        .collect()
}

/// Runs a registered tool, or `None` when `tool_id` is not one. A failure is
/// returned in the external adapters' JSON error shape.
pub(crate) fn execute(
    tool_state: &super::ToolSessionState,
    session_id: crate::contract::SessionId,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    let execute = with_tool(tool_id, |tool| tool.execute.clone())?;
    let workspace_root = tool_state.workspace_root();
    let call = ExternalToolCall {
        session_id: session_id.as_uuid(),
        workspace_root,
        project_root: workspace_root.and_then(crate::knowledge::main_root),
        input,
    };
    Some(execute(&call).unwrap_or_else(super::error_output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(id: &str, execute: ExternalToolExecutor) -> ExternalTool {
        ExternalTool {
            id: id.to_string(),
            title: format!("{id} title"),
            description: format!("{id} description"),
            input_schema: json!({ "type": "object" }),
            requires_trusted_project: true,
            execute,
        }
    }

    fn echo() -> ExternalToolExecutor {
        Arc::new(|call| Ok(json!({ "echo": call.input.clone() })))
    }

    #[test]
    fn a_registered_tool_joins_the_catalog() {
        register_external_tools(vec![tool("test.catalog", echo())]).unwrap();
        let entry = crate::tools::definitions()
            .into_iter()
            .find(|definition| definition.id == "test.catalog")
            .expect("registered tool is listed");
        assert_eq!(entry.description, "test.catalog description");
        assert!(is_registered("test.catalog"));
        assert!(requires_trusted_project("test.catalog"));
    }

    #[test]
    fn a_built_in_id_is_refused() {
        let error = register_external_tools(vec![tool("fs.read", echo())]).unwrap_err();
        assert!(error.contains("fs.read"), "{error}");
        assert!(!crate::tools::definitions()
            .iter()
            .any(|definition| definition.id == "fs.read"
                && definition.description == "fs.read description"));
    }

    #[test]
    fn a_repeated_id_refuses_the_whole_batch() {
        let error =
            register_external_tools(vec![tool("test.twice", echo()), tool("test.twice", echo())])
                .unwrap_err();
        assert!(error.contains("test.twice"), "{error}");
        assert!(!is_registered("test.twice"));
    }

    #[test]
    fn a_registered_tool_is_planned_to_run_without_approval() {
        register_external_tools(vec![tool("test.plan", echo())]).unwrap();
        let request = crate::contract::ToolCallRequest {
            call_id: crate::contract::ToolCallId("call-ext".to_string()),
            tool_id: "test.plan".to_string(),
            input: json!({ "anything": true }).into(),
            occurrence_id: crate::contract::OccurrenceId("call-ext".to_string()),
        };
        let state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir()).build();
        assert_eq!(
            crate::policy::plan_tool_call(&state, &request),
            crate::policy::ToolPlan::Automatic(crate::policy::AutomaticTool::Synchronous)
        );
    }

    #[test]
    fn an_untrusted_project_is_not_offered_a_trust_requiring_tool() {
        let mut open = tool("test.open", echo());
        open.requires_trusted_project = false;
        register_external_tools(vec![tool("test.gated", echo()), open]).unwrap();
        let offered = |trusted: bool| -> Vec<String> {
            crate::providers::rig::offered_tool_ids(None, trusted)
        };
        assert!(offered(true).contains(&"test.gated".to_string()));
        assert!(!offered(false).contains(&"test.gated".to_string()));
        assert!(offered(false).contains(&"test.open".to_string()));
    }

    #[test]
    fn execution_passes_the_session_and_roots_and_maps_errors() {
        register_external_tools(vec![
            tool(
                "test.roots",
                Arc::new(|call| {
                    Ok(json!({
                        "session": call.session_id.to_string(),
                        "workspace": call.workspace_root.map(|p| p.display().to_string()),
                        "input": call.input.clone(),
                    }))
                }),
            ),
            tool("test.fails", Arc::new(|_| Err("nope".to_string()))),
        ])
        .unwrap();
        let root = std::env::temp_dir();
        let state = crate::tools::ToolSessionBuilder::new(root.clone()).build();
        let session = crate::contract::SessionId::from_uuid(uuid::Uuid::from_u128(7));

        let ok = execute(&state, session, "test.roots", &json!({ "a": 1 })).unwrap();
        assert_eq!(ok["session"], uuid::Uuid::from_u128(7).to_string());
        assert_eq!(ok["workspace"], root.display().to_string());
        assert_eq!(ok["input"], json!({ "a": 1 }));

        let failed = execute(&state, session, "test.fails", &json!({})).unwrap();
        assert_eq!(failed, json!({ "is_error": true, "message": "nope" }));

        assert!(execute(&state, session, "test.unknown", &json!({})).is_none());
    }
}
