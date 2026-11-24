//! Tools a package outside `horizon-agent` adds (`crate::packages`),
//! beside the roles and skills it adds. A package supplies
//! each tool's definition and an executor written against plain types (a
//! session id, the session's roots, JSON in and out), so the package needs no
//! `horizon-agent` types; `horizon-agentd` is where packages are composed.
//!
//! Registering a tool declares it contained: it runs without an approval,
//! and the core takes the package's word for it. There is no approval path
//! for registered tools (`docs/project-wiki-design.md`, "Package layout").

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// The result's type, whose documented fields are rendered after
    /// `description` as built-in tools' legends are
    /// (`tools::output_legend`), so the fields the model reads about are the
    /// fields the type has. `None` only for a tool whose result the model
    /// does not interpret.
    pub output: Option<ExternalOutput>,
    pub execute: ExternalToolExecutor,
}

/// An external tool's result type, as its name and its JSON Schema. Generate
/// the schema with schemars' draft 2020-12 settings and
/// `inline_subschemas = true`, so its fields sit at the top level; each
/// field's doc comment becomes the meaning the legend shows.
#[derive(Debug, Clone)]
pub struct ExternalOutput {
    pub type_name: String,
    pub schema: Value,
}

impl std::fmt::Debug for ExternalTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalTool")
            .field("id", &self.id)
            .field("requires_trusted_project", &self.requires_trusted_project)
            .finish_non_exhaustive()
    }
}

pub(crate) fn requires_trusted_project(
    packages: &crate::packages::Packages,
    tool_id: &str,
) -> bool {
    packages
        .tools()
        .iter()
        .any(|tool| tool.id == tool_id && tool.requires_trusted_project)
}

/// The registered tools' catalog entries, in registration order.
pub(crate) fn definitions(packages: &crate::packages::Packages) -> Vec<super::Definition> {
    packages.tools().iter().map(definition_of).collect()
}

fn definition_of(tool: &ExternalTool) -> super::Definition {
    super::Definition {
        id: tool.id.clone(),
        title: tool.title.clone(),
        description: match &tool.output {
            Some(output) => super::catalog::legend_after(
                &tool.description,
                super::output_legend::legend_from_schema(&output.type_name, &output.schema),
            ),
            None => tool.description.clone(),
        },
        input_schema: tool.input_schema.clone(),
        permission: crate::contract::ToolPermission::AutoAllowUi,
    }
}

/// The registered tool `tool_id` names, as a [`super::tool::Tool`].
pub(crate) fn find(
    packages: &crate::packages::Packages,
    tool_id: &str,
) -> Option<std::sync::Arc<dyn super::tool::Tool>> {
    let tool = packages
        .tools()
        .iter()
        .find(|tool| tool.id == tool_id)?
        .clone();
    Some(std::sync::Arc::new(Registered {
        definition: definition_of(&tool),
        tool,
    }))
}

struct Registered {
    definition: super::Definition,
    tool: ExternalTool,
}

impl super::tool::Tool for Registered {
    fn definition(&self) -> &super::Definition {
        &self.definition
    }

    /// A registered tool runs code its package supplies, which answers for
    /// what it does: the tiers let it run without asking.
    fn reach(
        &self,
        _state: &super::ToolSessionState,
        _call: &super::input::PreparedCall<'_>,
    ) -> crate::modules::Reach {
        crate::modules::Reach {
            permission: None,
            containment: crate::modules::Containment::Package,
            ask: None,
            uncontained_write: false,
            primary_checkout: None,
            git_metadata: None,
        }
    }

    /// A failure is returned in the external adapters' JSON error shape.
    fn run(
        &self,
        cx: &super::tool::RunContext<'_>,
        call: &super::input::PreparedCall<'_>,
    ) -> super::tool::Dispatch {
        let workspace_root = cx.state.workspace_root();
        let external = ExternalToolCall {
            session_id: cx.session_id.as_uuid(),
            workspace_root,
            project_root: workspace_root.and_then(crate::knowledge::main_root),
            input: &call.request.input,
        };
        super::output::Response::external(
            (self.tool.execute)(&external).unwrap_or_else(super::error_output),
        )
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct NoHost;
    impl crate::tools::HostTools for NoHost {
        fn execute_auto(&self, _: &str, _: &Value) -> Option<Value> {
            None
        }
    }

    /// Runs registered tool `id` as an automatic call would, or `None` when
    /// no tool has that id.
    fn run(
        state: &crate::tools::ToolSessionState,
        session_id: crate::contract::SessionId,
        id: &str,
        input: Value,
    ) -> Option<Value> {
        let tool = find(state.packages(), id)?;
        let request = crate::contract::ToolCallRequest {
            call_id: crate::contract::ToolCallId("call".into()),
            occurrence_id: crate::contract::OccurrenceId::new(),
            tool_id: id.into(),
            input: input.into(),
        };
        let call = crate::tools::input::PreparedCall::new(&request).unwrap();
        let cx = super::super::tool::RunContext {
            host: &NoHost,
            state,
            session_id,
        };
        match tool.run(&cx, &call) {
            super::super::tool::Dispatch::Done(output) => Some(output.output.to_json()),
            super::super::tool::Dispatch::Spawned => None,
        }
    }

    /// The packages that add `tools`, or why they are refused.
    fn added(tools: Vec<ExternalTool>) -> Result<crate::packages::Packages, String> {
        crate::packages::Packages::builder()
            .tools(tools)
            .map(|builder| builder.build())
            .map_err(|(_, error)| error)
    }

    fn tool(id: &str, execute: ExternalToolExecutor) -> ExternalTool {
        ExternalTool {
            id: id.to_string(),
            title: format!("{id} title"),
            description: format!("{id} description"),
            input_schema: json!({ "type": "object" }),
            requires_trusted_project: true,
            output: None,
            execute,
        }
    }

    #[test]
    fn an_output_type_renders_its_documented_fields_after_the_description() {
        let mut documented = tool("test_legend", echo());
        documented.output = Some(ExternalOutput {
            type_name: "Echoed".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "echo": {"description": "The input, returned unchanged."},
                    "bare": {}
                }
            }),
        });
        let packages = added(vec![documented]).unwrap();
        let entry = crate::tools::definitions(&packages)
            .into_iter()
            .find(|definition| definition.id == "test_legend")
            .unwrap();
        assert!(
            entry
                .description
                .starts_with("test_legend description\n\nEchoed fields:\n"),
            "{}",
            entry.description
        );
        assert!(
            entry
                .description
                .contains("echo — The input, returned unchanged.")
        );
        assert!(entry.description.contains("\nbare"));
    }

    fn echo() -> ExternalToolExecutor {
        Arc::new(|call| Ok(json!({ "echo": call.input.clone() })))
    }

    #[test]
    fn a_registered_tool_joins_the_catalog() {
        let packages = added(vec![tool("test_catalog", echo())]).unwrap();
        let entry = crate::tools::definitions(&packages)
            .into_iter()
            .find(|definition| definition.id == "test_catalog")
            .expect("registered tool is listed");
        assert_eq!(entry.description, "test_catalog description");
        assert!(requires_trusted_project(&packages, "test_catalog"));
    }

    #[test]
    fn a_built_in_id_is_refused() {
        let error = added(vec![tool("fs_read", echo())]).unwrap_err();
        assert!(error.contains("fs_read"), "{error}");
        assert!(
            !crate::tools::definitions(&crate::packages::Packages::default())
                .iter()
                .any(|definition| definition.id == "fs_read"
                    && definition.description == "fs_read description")
        );
    }

    #[test]
    fn a_repeated_id_refuses_the_whole_batch() {
        let error =
            added(vec![tool("test_twice", echo()), tool("test_twice", echo())]).unwrap_err();
        assert!(error.contains("test_twice"), "{error}");
    }

    #[test]
    fn an_id_a_provider_refuses_refuses_the_whole_batch() {
        let error = added(vec![
            tool("test_valid", echo()),
            tool("test.dotted", echo()),
        ])
        .unwrap_err();
        assert!(error.contains("test.dotted"), "{error}");
    }

    #[test]
    fn a_registered_tool_is_planned_to_run_without_approval() {
        let packages = added(vec![tool("test_plan", echo())]).unwrap();
        let request = crate::contract::ToolCallRequest {
            call_id: crate::contract::ToolCallId("call-ext".to_string()),
            tool_id: "test_plan".to_string(),
            input: json!({ "anything": true }).into(),
            occurrence_id: crate::contract::OccurrenceId("call-ext".to_string()),
        };
        let state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_packages(packages)
            .build();
        assert_eq!(
            crate::policy::plan_tool_call(&state, &request),
            crate::policy::ToolPlan::Automatic
        );
    }

    #[test]
    fn an_untrusted_project_is_not_offered_a_trust_requiring_tool() {
        let mut open = tool("test_open", echo());
        open.requires_trusted_project = false;
        let packages = added(vec![tool("test_gated", echo()), open]).unwrap();
        let offered = |trusted: bool| -> Vec<String> {
            crate::providers::rig::offered_tool_ids(&packages, None, trusted)
        };
        assert!(offered(true).contains(&"test_gated".to_string()));
        assert!(!offered(false).contains(&"test_gated".to_string()));
        assert!(offered(false).contains(&"test_open".to_string()));
    }

    #[test]
    fn execution_passes_the_session_and_roots_and_maps_errors() {
        let packages = added(vec![
            tool(
                "test_roots",
                Arc::new(|call| {
                    Ok(json!({
                        "session": call.session_id.to_string(),
                        "workspace": call.workspace_root.map(|p| p.display().to_string()),
                        "input": call.input.clone(),
                    }))
                }),
            ),
            tool("test_fails", Arc::new(|_| Err("nope".to_string()))),
        ])
        .unwrap();
        let root = std::env::temp_dir();
        let state = crate::tools::ToolSessionBuilder::new(root.clone())
            .with_packages(packages)
            .build();
        let session = crate::contract::SessionId::from_uuid(uuid::Uuid::from_u128(7));

        let ok = run(&state, session, "test_roots", json!({ "a": 1 })).unwrap();
        assert_eq!(ok["session"], uuid::Uuid::from_u128(7).to_string());
        assert_eq!(ok["workspace"], root.display().to_string());
        assert_eq!(ok["input"], json!({ "a": 1 }));

        let failed = run(&state, session, "test_fails", json!({})).unwrap();
        assert_eq!(failed, json!({ "is_error": true, "message": "nope" }));

        assert!(run(&state, session, "test_unknown", json!({})).is_none());
    }
}
