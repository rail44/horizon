//! The MCP servers a client names for a session in ACP's `session/new` or
//! `session/resume` (`docs/agent-sessions-on-demand-design.md`): each is
//! started over stdio for the session's life, and its tools are handed to
//! the session (`horizon_agent::tools::SessionTool`).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent_client_protocol::v2::schema as v2;
use horizon_agent::tools::{SessionTool, SessionToolCall};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{Peer, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;

/// How long a server has to start and answer `initialize` and `tools/list`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// A session's MCP servers: the tools they offer, and the connections that
/// keep them running. Dropping it stops them.
pub struct McpServers {
    tools: Vec<SessionTool>,
    running: Vec<RunningService<RoleClient, ()>>,
}

impl McpServers {
    pub fn tools(&self) -> Vec<SessionTool> {
        self.tools.clone()
    }
}

impl Drop for McpServers {
    /// A server's process is stopped by a task its connection spawns as it
    /// goes, which needs the runtime it runs on.
    fn drop(&mut self) {
        let running = std::mem::take(&mut self.running);
        let _runtime = runtime().enter();
        drop(running);
    }
}

/// Starts the stdio servers in `servers` and lists their tools. A server of
/// another transport is refused: this agent advertises stdio alone.
pub async fn connect(servers: Vec<v2::McpServer>) -> Result<McpServers, String> {
    runtime()
        .spawn(async move {
            let mut connected = McpServers {
                tools: Vec::new(),
                running: Vec::new(),
            };
            for server in servers {
                let v2::McpServer::Stdio(stdio) = server else {
                    return Err("only stdio MCP servers are taken".to_string());
                };
                let name = stdio.name.clone();
                let (running, tools) = tokio::time::timeout(CONNECT_TIMEOUT, start(stdio))
                    .await
                    .map_err(|_| format!("MCP server `{name}` did not start in time"))?
                    .map_err(|error| format!("MCP server `{name}`: {error}"))?;
                connected.tools.extend(tools);
                connected.running.push(running);
            }
            Ok(connected)
        })
        .await
        .map_err(|error| format!("MCP servers could not be started: {error}"))?
}

async fn start(
    server: v2::McpServerStdio,
) -> Result<(RunningService<RoleClient, ()>, Vec<SessionTool>), String> {
    let mut command = tokio::process::Command::new(server.command.into_inner());
    command.args(&server.args);
    for variable in &server.env {
        command.env(&variable.name, &variable.value);
    }
    let transport = TokioChildProcess::new(command).map_err(|error| error.to_string())?;
    let running = ().serve(transport).await.map_err(|error| error.to_string())?;
    let listed = running
        .list_all_tools()
        .await
        .map_err(|error| error.to_string())?;
    let peer = running.peer().clone();
    let tools = listed
        .into_iter()
        .map(|tool| session_tool(&server.name, tool, peer.clone()))
        .collect();
    Ok((running, tools))
}

/// `mcp__<server>__<tool>`, the form other agents name an MCP server's tools
/// in, kept to what a provider takes as a function name
/// (`^[a-zA-Z0-9_-]{1,64}$`).
fn tool_id(server: &str, tool: &str) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let mut id = format!("mcp__{}__{}", clean(server), clean(tool));
    id.truncate(64);
    id
}

fn session_tool(server: &str, tool: rmcp::model::Tool, peer: Peer<RoleClient>) -> SessionTool {
    let name = tool.name.to_string();
    let read_only = (tool.annotations.as_ref())
        .and_then(|annotations| annotations.read_only_hint)
        .unwrap_or(false);
    let title = (tool.title.clone())
        .or_else(|| {
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations.title.clone())
        })
        .unwrap_or_else(|| name.clone());
    let description = (tool.description.as_deref()).unwrap_or(&title).to_string();
    let call_name = name.clone();
    let call: SessionToolCall = Arc::new(move |input: Value| {
        let peer = peer.clone();
        let name = call_name.clone();
        Box::pin(async move {
            let params = CallToolRequestParams::new(name);
            let params = match input {
                Value::Object(arguments) => params.with_arguments(arguments),
                Value::Null => params,
                _ => return Err("the tool's arguments must be an object".to_string()),
            };
            let result = peer
                .call_tool(params)
                .await
                .map_err(|error| error.to_string())?;
            answer(result)
        })
    });
    SessionTool {
        id: tool_id(server, &name),
        title,
        description,
        input_schema: Value::Object((*tool.input_schema).clone()),
        read_only,
        call,
    }
}

/// A call's result as the model sees it: its structured content when it has
/// some, else its text.
fn answer(result: CallToolResult) -> Result<Value, String> {
    let text = (result.content.iter())
        .filter_map(|block| block.as_text())
        .map(|text| text.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if result.is_error == Some(true) {
        return Err(if text.is_empty() {
            "the tool failed".to_string()
        } else {
            text
        });
    }
    Ok(result
        .structured_content
        .unwrap_or_else(|| serde_json::json!({ "text": text })))
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("horizon-agent-mcp")
            .build()
            .expect("MCP runtime")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_is_named_after_its_server_as_a_provider_takes_a_name() {
        assert_eq!(tool_id("board", "board_read"), "mcp__board__board_read");
        assert_eq!(tool_id("my server", "a.b"), "mcp__my_server__a_b");
        assert_eq!(tool_id(&"s".repeat(80), "t").len(), 64);
    }
}
