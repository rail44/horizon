use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::contract::ToolPermission;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Definition {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub permission: ToolPermission,
}

pub(crate) fn definitions() -> Vec<Definition> {
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
            description: "Read a known text file or a relevant line window, with line numbers. \
                Requires an absolute path. Use fs.grep to locate specific content before \
                reading a large file, and fs.glob when the file path is unknown. Pass \
                offset/limit to continue through a file; the result stops at 50,000 content \
                characters and returns next_offset. Each line is truncated at 2,000 \
                characters. Read independent known files in parallel, and prefer one useful \
                window over many tiny adjacent slices."
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
                        "maximum": 2000,
                        "description": "Maximum number of lines to return. Defaults to 500; maximum 2000.",
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
            description: "Find where text occurs. Search one file or all files under a \
                directory with a regular expression, optionally restricted by glob. Returns \
                one `path` + `line_number` per match — locations, not content — plus the \
                total match count. Read what a location says with fs.read, passing offset \
                and limit around the reported line. For an open-ended exploration that \
                would take several rounds of searching and reading, call task instead and \
                keep only its report. Requires an absolute base \
                path. Traversal stops at 64 MiB of scanned file bytes or 20,000 files."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["base_path", "pattern"],
                "properties": {
                    "base_path": {
                        "type": "string",
                        "description": "Absolute file to search or directory to search under.",
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
                        "description": "Maximum number of match locations to return. Defaults to 100.",
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
            description: "Apply one or more string replacements in a single call. Batch related \
                edits — several files, or several hunks of one file — into one list instead of \
                one call each. Each `old_string` must match exactly once unless \
                `replace_all: true` is set for that edit, and every file must have been read in \
                this session with no changes on disk since. Edits apply in list order and later \
                edits see earlier ones' effects. If an edit fails, the call stops there: earlier \
                edits stay applied, the rest are not attempted, and the result reports every \
                edit's outcome (applied / failed / not_attempted) in order plus the failing \
                index, so you can fix that edit and resend from it."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["edits"],
                "properties": {
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Replacements to apply, in order. Put every related edit in this one list rather than issuing a call per edit.",
                        "items": {
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
                                    "description": "Exact text to replace. Must match exactly once in the file unless `replace_all` is true.",
                                },
                                "new_string": {
                                    "type": "string",
                                    "description": "Replacement text.",
                                },
                                "replace_all": {
                                    "type": "boolean",
                                    "description": "If true, replace every occurrence of `old_string` in the file. Defaults to false (exactly one match required).",
                                },
                            }
                        },
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
                code is a normal result, not an error. When you need a different slice of a \
                command's output, read or grep the spilled file (`output_file`) instead of \
                re-running the command."
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
                        "maximum": crate::config::DEFAULT_BASH_TIMEOUT_MAX_SECS,
                        "description": format!(
                            "Optional wall-clock timeout in seconds. Omit this normally: the \
                             default is {} seconds and the hard cap is {}. Use a shorter value \
                             only when deliberately bounding a known quick probe; builds, tests, \
                             hooks, and Git operations commonly exceed 60 seconds.",
                            crate::config::DEFAULT_BASH_TIMEOUT_DEFAULT_SECS,
                            crate::config::DEFAULT_BASH_TIMEOUT_MAX_SECS,
                        ),
                    },
                }
            }),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "web_search".to_string(),
            title: "Search the Web".to_string(),
            description: "Search the public web through Horizon's fixed Exa adapter. Returns a \
                bounded list of titles, URLs, publication metadata, and relevant excerpts. \
                Requires EXA_API_KEY in Horizon's environment."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 2048,
                        "description": "Natural-language web search query.",
                    },
                    "num_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Number of results. Defaults to 5.",
                    },
                    "max_characters": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 4000,
                        "description": "Maximum excerpt characters per result. Defaults to 2000.",
                    },
                }
            }),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "web_fetch".to_string(),
            title: "Fetch a Web Page".to_string(),
            description: "Fetch one public HTTP(S) URL with SSRF protection and bounded \
                redirects/body size. HTML is reduced to readable Markdown; text and JSON pass \
                through. A session must approve each exact destination host before contact."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["url"],
                "properties": {
                    "url": {
                        "type": "string",
                        "maxLength": 8192,
                        "description": "Public http:// or https:// URL on the standard port to fetch.",
                    },
                    "max_characters": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50000,
                        "description": "Maximum returned content characters. Defaults to 20000.",
                    },
                }
            }),
            permission: ToolPermission::RequireApproval,
        },
        #[cfg(any(test, feature = "test-fixtures"))]
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
        #[cfg(any(test, feature = "test-fixtures"))]
        // Test-only, mirroring `mock.approval_required` above: this fixture
        // exercises the judge's human-gated boundary path independently of
        // the production web tools and their transport setup
        // approval gate at the
        // `policy::horizon_events_for_provider_event` seam.
        Definition {
            id: "mock.boundary_crossing".to_string(),
            title: "Mock Boundary Crossing".to_string(),
            description: "Test tool that exercises the judge's boundary-crossing \
                path."
                .to_string(),
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
                context around a hit. Hits carry outcome labels: a tool_result hit has \
                is_error, and every hit has turn_outcome (how the turn it belongs to ended, if \
                it has). Use turn_outcome to find how past work ended -- e.g. search with \
                turn_outcome: \"halted\" for doom-looped turns, or \"failed\" for turns that \
                errored out. `query` can be omitted if `turn_outcome` is given, for listing \
                mode: instead of matching a substring, this lists every hit with that outcome \
                (still newest-first, still capped by limit) -- e.g. list how recent work ended \
                with turn_outcome: \"halted\" and no query, to cluster halted turns before \
                digging into any one of them with recall.read. At least one of `query`/ \
                `turn_outcome` is required."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Substring to search for, case-insensitive. May be \
                            omitted if turn_outcome is given (listing mode).",
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
                    "turn_outcome": {
                        "type": "string",
                        "enum": ["completed", "cancelled", "failed", "halted"],
                        "description": "Restrict hits to events whose turn ended this way. \
                            \"halted\" surfaces doom-looped turns; \"failed\" surfaces turns \
                            that errored out.",
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
        // `task` (`tools::explore`, `docs/agent-explore-design.md`) is
        // auto-allowed like every other read tool -- the session it spawns
        // can only read, and only inside the requester's own workspace root.
        // Since the 2026-07-28 asynchronous cutover
        // (`docs/agent-async-task-design.md`) the *call* finishes at once,
        // returning only a launch receipt; the report arrives later as a
        // notification injected into a later provider round.
        //
        // The description is in the register the two production models were
        // measured against (`docs/research/agent-delegation-and-batching-
        // probes-2026-07-27.md`, cells C3 and C5): the generic task-tool
        // wording plus the self-orientation clause. Its routing counterpart
        // is `prompt::DELEGATION_ROUTING_SECTION`; both were measured
        // together, so change them together.
        //
        // The read-only sentence is a later, additive amendment (2026-07-27)
        // that leaves the measured wording intact: a dogfooded session
        // delegated the *implementation* to task children twice, which the
        // read-only whitelist turns into another exploration that burns the
        // child's turn budget. The generic `task` name reads as
        // write-capable in these models' training distribution, so the
        // constraint has to be stated rather than implied.
        //
        // The asynchronous register (2026-07-28) replaces only the
        // return-shape sentence: "runs in the background, you will be
        // notified, keep working, up to 3 at once" is the wording
        // mainstream harnesses use, which is the whole reason the design
        // chose this shape over a join-first one
        // (`docs/agent-async-task-design.md`'s "Why", third bullet).
        //
        // The decomposition sentence (2026-07-28, later the same day)
        // matches the routing section's third amendment: the first
        // validation run of the async loop launched one monolithic task,
        // so both surfaces now ask for several narrowly scoped launches in
        // one response. To be measured on the next dogfood run.
        Definition {
            id: "task".to_string(),
            title: "Delegate a Task".to_string(),
            description: "Launch a read-only agent in a parallel session sharing this workspace \
                to handle multi-step investigation autonomously. For open-ended codebase \
                exploration or multi-file search, prefer task instead of running searches \
                yourself — this keeps intermediate output out of your context. Describe the \
                question and the exact deliverable (paths, line numbers, facts, a step plan) in \
                the prompt. The task agent does its own orientation inside its own session; do \
                not orient with bash/ls first. For one to three known files, read them directly \
                instead. Task agents are read-only — they investigate, locate, and plan, but \
                cannot write files or run commands that modify state; implementation happens in \
                this session after the report returns. Runs in the background — you will be \
                notified when it completes; keep working in the meantime; up to 2 may run \
                concurrently. Prefer several narrowly scoped tasks launched in parallel in one \
                response over a single broad one. Returns immediately with the task session's \
                id, which is also how you re-read its report later with task_output."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["description", "prompt"],
                "properties": {
                    "description": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 200,
                        "description": "A short (3-5 word) summary of the task, for display \
                            while it runs.",
                    },
                    "prompt": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 16384,
                        "description": "The question and the exact deliverable you want back. \
                            The task session sees this and nothing else from your conversation, \
                            so restate whatever context it needs.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        // `task_output` (`docs/agent-async-task-design.md` decision 3) is
        // the pull complement to push delivery, not the primary channel:
        // the report already arrived as a notification, capped at
        // `tools::explore::INLINE_REPORT_CAP_CHARS`, and this is how the
        // rest of a long one — or a re-read much later in the session — is
        // fetched. Advertised only alongside `task` itself; see
        // `providers::rig::completion::rig_tool_definitions`.
        Definition {
            id: "task_output".to_string(),
            title: "Read a Task's Report".to_string(),
            description: "Read the full report of a background task you launched with task, by \
                its session id. Use it when a completion notification says the report was \
                truncated, or to re-read a report from earlier in this session. A task that is \
                still running reports as such — you do not need to poll it; you will be notified \
                when it completes."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["session_id"],
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The task session id returned by the task call that \
                            launched it.",
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

pub(crate) fn permission_for_tool(tool_id: &str) -> Option<ToolPermission> {
    definitions()
        .into_iter()
        .find(|definition| definition.id == tool_id)
        .map(|definition| definition.permission)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(id: &str) -> Definition {
        definitions()
            .into_iter()
            .find(|definition| definition.id == id)
            .unwrap_or_else(|| panic!("`{id}` must be in the catalog"))
    }

    /// The 2026-07-27 rename and two-field input shape
    /// (`docs/research/agent-delegation-and-batching-probes-2026-07-27.md`
    /// cells C3/C5): the model-visible id is `task`, and both `description`
    /// and `prompt` are required -- the shape every winning probe cell ran
    /// with.
    #[test]
    fn task_is_cataloged_with_a_required_description_and_prompt() {
        let task = definition("task");

        assert_eq!(task.permission, ToolPermission::AutoAllowRead);
        assert_eq!(
            task.input_schema["required"],
            json!(["description", "prompt"])
        );
        assert_eq!(
            task.input_schema["properties"]["description"]["type"],
            "string"
        );
        assert_eq!(
            task.input_schema["properties"]["description"]["minLength"],
            1
        );
        assert_eq!(task.input_schema["properties"]["prompt"]["type"], "string");
        assert_eq!(task.input_schema["properties"]["prompt"]["minLength"], 1);
        assert_eq!(
            task.input_schema["additionalProperties"],
            json!(false),
            "the old `session_id` follow-up field must stay unrepresentable"
        );
        assert!(
            !definitions().iter().any(|d| d.id == "agent.explore"),
            "the pre-rename id must be gone from the catalog"
        );
    }

    /// The description carries the self-orientation clause the probe's C5
    /// cell measured, and `fs.grep`'s routing tail names the tool by its
    /// current id rather than a stale one.
    #[test]
    fn task_and_grep_descriptions_route_consistently() {
        let task = definition("task");
        assert!(
            task.description
                .contains("does its own orientation inside its own session"),
            "{}",
            task.description
        );
        assert!(
            task.description
                .contains("do not orient with bash/ls first"),
            "{}",
            task.description
        );

        // The 2026-07-27 amendment: the generic `task` name reads as
        // write-capable, so the read-only constraint and where
        // implementation happens are stated outright.
        assert!(
            task.description.contains("Task agents are read-only"),
            "{}",
            task.description
        );
        assert!(
            task.description
                .contains("implementation happens in this session after the report returns"),
            "{}",
            task.description
        );

        // The 2026-07-28 decomposition amendment, matching the routing
        // section's third amendment (`prompt::DELEGATION_ROUTING_SECTION`).
        assert!(
            task.description.contains(
                "Prefer several narrowly scoped tasks launched in parallel in one response over \
                 a single broad one"
            ),
            "{}",
            task.description
        );

        let grep = definition("fs.grep");
        assert!(
            grep.description.contains("call task instead"),
            "{}",
            grep.description
        );
        assert!(
            !grep.description.contains("agent.explore"),
            "{}",
            grep.description
        );
    }
}
