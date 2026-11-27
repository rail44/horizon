//! `horizon mcp board --dir <dir>`: the board of the project `dir` is in,
//! as an MCP server over standard input and output, for an agent session
//! a client names it for (`docs/agent-sessions-on-demand-design.md`). A
//! comment's author is the session in `HORIZON_SESSION_ID`, which the
//! agent sets for the servers it starts.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use horizon_board::Store;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;

const USAGE: &str = "usage: horizon mcp board --dir <dir>";

/// Runs `horizon mcp ...` when `args` names it; `None` otherwise.
pub fn try_run(args: &[String], stderr: &mut impl Write) -> Option<u8> {
    if args.first().map(String::as_str) != Some("mcp") {
        return None;
    }
    let dir = match &args[1..] {
        [server, flag, dir] if server == "board" && flag == "--dir" => PathBuf::from(dir),
        _ => {
            let _ = writeln!(stderr, "{USAGE}");
            return Some(2);
        }
    };
    let store = match Store::from_dir(&dir) {
        Ok(store) => store,
        Err(error) => {
            let _ = writeln!(stderr, "error: {error}");
            return Some(1);
        }
    };
    let author = std::env::var("HORIZON_SESSION_ID")
        .map(|id| format!("session:{id}"))
        .unwrap_or_else(|_| "agent".to_string());
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(stderr, "error: {error}");
            return Some(1);
        }
    };
    let served = runtime.block_on(async {
        let running = BoardServer { store, author }
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| error.to_string())?;
        running.waiting().await.map_err(|error| error.to_string())
    });
    match served {
        Ok(_) => Some(0),
        Err(error) => {
            let _ = writeln!(stderr, "error: {error}");
            Some(1)
        }
    }
}

struct BoardServer {
    store: Store,
    author: String,
}

impl ServerHandler for BoardServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "horizon-board",
                env!("CARGO_PKG_VERSION"),
            ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            horizon_board::tools::tools()
                .into_iter()
                .map(|tool| {
                    let schema = match tool.input_schema {
                        Value::Object(schema) => schema,
                        _ => Default::default(),
                    };
                    Tool::new(tool.name, tool.description, Arc::new(schema))
                        .with_title(tool.title)
                        .annotate(ToolAnnotations::new().read_only(tool.read_only))
                })
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let input = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let result =
            horizon_board::tools::call(&self.store, &self.author, &request.name, &input).await;
        Ok(match result {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The board's tools as an agent's MCP client sees them, over an
    /// in-memory pipe: listed with which only reads, and an empty board
    /// read as an empty list.
    #[tokio::test]
    async fn an_agent_sees_the_boards_tools_and_reads_the_board() {
        let dir = tempfile::tempdir().unwrap();
        let server = BoardServer {
            store: Store::at(dir.path().join("events.jsonl")),
            author: "agent".into(),
        };
        let (client_side, server_side) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let running = server.serve(server_side).await.unwrap();
            let _ = running.waiting().await;
        });
        let client = ().serve(client_side).await.unwrap();
        let tools = client.list_all_tools().await.unwrap();
        let read_only: Vec<_> = tools
            .iter()
            .map(|tool| {
                let hint = tool.annotations.as_ref().and_then(|a| a.read_only_hint);
                (tool.name.to_string(), hint)
            })
            .collect();
        assert_eq!(
            read_only,
            [
                ("board_read".to_string(), Some(true)),
                ("board_comment".to_string(), Some(false)),
                ("board_update".to_string(), Some(false)),
            ]
        );
        let result = client
            .call_tool(CallToolRequestParams::new("board_read"))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content[0].as_text().unwrap().text, "[]");
    }
}
