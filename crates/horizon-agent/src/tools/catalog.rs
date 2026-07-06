use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::contract::ToolPermission;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub permission: ToolPermission,
}

pub fn definitions() -> Vec<Definition> {
    vec![
        Definition {
            id: "workspace.snapshot".to_string(),
            title: "Workspace Snapshot".to_string(),
            description: "Read tabs, panes, sessions, and active workspace state.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "fs.read".to_string(),
            title: "Read File".to_string(),
            description: "Read a text file from disk, windowed by line. Requires an absolute \
                path; large files are capped by default (pass offset/limit to page through \
                them)."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file to read.",
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based line number to start reading from. Defaults to 1.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of lines to return. Defaults to 2000.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "fs.glob".to_string(),
            title: "Find Files".to_string(),
            description: "Find files under a directory matching a glob pattern (e.g. \
                `**/*.rs`). Requires an absolute base path; results are capped, with the \
                total match count reported."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["base_path", "pattern"],
                "properties": {
                    "base_path": {
                        "type": "string",
                        "description": "Absolute directory to search under.",
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match file paths against, e.g. `**/*.rs`.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of matches to return. Defaults to 200.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "fs.grep".to_string(),
            title: "Search File Contents".to_string(),
            description: "Search file contents under a directory with a regular expression, \
                optionally restricted to files matching a glob. Requires an absolute base \
                path; results are capped, with the total match count reported."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["base_path", "pattern"],
                "properties": {
                    "base_path": {
                        "type": "string",
                        "description": "Absolute directory to search under.",
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Regular expression to search for, per line.",
                    },
                    "glob": {
                        "type": "string",
                        "description": "Optional glob to restrict which files are searched, e.g. `**/*.rs`.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of matches to return. Defaults to 100.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "fs.write".to_string(),
            title: "Write File".to_string(),
            description: "Create or overwrite a file with the given content, creating parent \
                directories as needed. Overwriting an existing file requires it to have been \
                read in this session with no changes on disk since."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "content"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to write. Parent directories are created if missing.",
                    },
                    "content": {
                        "type": "string",
                        "description": "Full file contents to write, replacing any existing content.",
                    },
                }
            }),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "fs.edit".to_string(),
            title: "Edit File".to_string(),
            description: "Replace one exact, unique occurrence of `old_string` with \
                `new_string` in an existing file. The file must have been read in this \
                session with no changes on disk since; `old_string` must match exactly once."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "old_string", "new_string"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to an existing file that has been read this session.",
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact text to replace. Must match exactly once in the file.",
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text.",
                    },
                }
            }),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "bash".to_string(),
            title: "Run Shell Command".to_string(),
            description: "Run a shell command via `bash -c` in a fresh subprocess — not a \
                persistent shell. The working directory is tracked across calls within this \
                session (a `cd` in the command carries forward to the next call). Requires user \
                approval. Output is stdout+stderr combined, capped in-context with the full \
                output always spilled to a temp file whose path is returned. A non-zero exit \
                code is a normal result, not an error."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["command"],
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to run via `bash -c`.",
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 600,
                        "description": "Wall-clock timeout in seconds. Defaults to 120, capped at 600.",
                    },
                }
            }),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "mock.approval_required".to_string(),
            title: "Mock Approval Required".to_string(),
            description: "Test tool that exercises the approval flow.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": true
            }),
            permission: ToolPermission::RequireApproval,
        },
        // config.read/config.write (`tools::config`) are the config role's
        // only allowed tools (`roles::CONFIG_ROLE`). Cataloging them
        // globally here adds no new *capability* -- `bash` can already
        // read/write this same file with no dedicated tool at all
        // (`docs/agent-tools-design.md`) -- the restriction they exist for
        // happens at the role's `allowed_tool_ids`, not here. See
        // `tools::config`'s own doc comment for the full trust reasoning.
        // `skill.read` (grouped with them below since `tools::config` also
        // executes it) is different: every session can call it, role-less
        // or not -- see `skills`' module doc.
        Definition {
            id: "config.read".to_string(),
            title: "Read Horizon Config".to_string(),
            description: "Read Horizon's config file: the resolved path and its current \
                contents, or an explicit \"does not exist yet\" result (with the path still \
                reported) if nothing has been written there yet. Takes no arguments."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "config.write".to_string(),
            title: "Write Horizon Config".to_string(),
            description: "Replace Horizon's config file with the given complete content \
                (validated as well-formed TOML before writing). Preserve every entry the user \
                didn't ask to change -- this replaces the whole file, not just one section. \
                Overwriting an existing file requires it to have been read in this session \
                (via config.read) with no changes on disk since."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["content"],
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "Full TOML file contents to write, replacing any existing content.",
                    },
                }
            }),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "recall.search".to_string(),
            title: "Search Persisted History".to_string(),
            description: "Search committed conversation text and tool calls/results across \
                persisted history (including turns no longer in your context window). \
                Case-insensitive substring match. Streaming deltas/reasoning are not included, \
                only what was actually committed. Default scope is this session; pass \
                scope: \"all\" to search every persisted session. Use recall.read to pull full \
                context around a hit."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Substring to search for, case-insensitive.",
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["session", "all"],
                        "description": "\"session\" (default) searches only this session's \
                            history; \"all\" searches every persisted session.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum number of hits to return. Defaults to 20.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "recall.read".to_string(),
            title: "Read Persisted History Window".to_string(),
            description: "Read an ordered window of committed messages, tool calls, and tool \
                results for a session starting at a given sequence number -- use after \
                recall.search to pull full context around a hit. Defaults to this session if \
                session_id is omitted. Output is capped in total size; call again with a later \
                from_sequence to continue."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["from_sequence"],
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Session id to read from. Defaults to this session.",
                    },
                    "from_sequence": {
                        "type": "integer",
                        "description": "Sequence number to start reading from (inclusive).",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum number of entries to return. Defaults to 20.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "skill.read".to_string(),
            title: "Read Skill".to_string(),
            description: "Read one of this session's available skills by id (see the skills \
                listed in the system prompt) and return its full instructions."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["id"],
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Skill id, as listed in the system prompt's skills section.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
    ]
}

pub fn permission_for_tool(tool_id: &str) -> Option<ToolPermission> {
    definitions()
        .into_iter()
        .find(|definition| definition.id == tool_id)
        .map(|definition| definition.permission)
}
