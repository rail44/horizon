use serde::{Deserialize, Serialize};
use serde_json::json;

use super::synchronous::SynchronousTool;
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
    catalog().to_vec()
}

fn catalog() -> &'static [Definition] {
    static DEFINITIONS: std::sync::OnceLock<Vec<Definition>> = std::sync::OnceLock::new();
    DEFINITIONS.get_or_init(build_definitions)
}

fn build_definitions() -> Vec<Definition> {
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
        SynchronousTool::ReadFile.definition(
            "Read File".to_string(),
            "Read a known text file or a relevant line window, with line numbers. \
                Requires an absolute path. Use fs.grep to locate specific content before \
                reading a large file, and fs.glob when the file path is unknown. Pass \
                offset/limit to continue through a file; the result stops at 50,000 content \
                characters and returns next_offset. Each line is truncated at 2,000 \
                characters. Read independent known files in parallel, and prefer one useful \
                window over many tiny adjacent slices."
                .to_string(),

        ),
        SynchronousTool::Glob.definition(
            "Find Files".to_string(),
            "Find files under a directory matching a glob pattern (e.g. \
                `**/*.rs`). Requires an absolute base path; results are capped, with the \
                total match count reported."
                .to_string(),

        ),
        SynchronousTool::Grep.definition(
            "Search File Contents".to_string(),
            "Find where text occurs. Search one file or all files under a \
                directory with a regular expression, optionally restricted by glob. Returns \
                one `path` + `line_number` per match — locations, not content — plus the \
                total match count. Read what a location says with fs.read, passing offset \
                and limit around the reported line. For an open-ended exploration that \
                would take several rounds of searching and reading, call task instead and \
                keep only its report. Requires an absolute base \
                path. Traversal stops at 64 MiB of scanned file bytes or 20,000 files."
                .to_string(),

        ),
        SynchronousTool::WriteFile.definition(
            "Write File".to_string(),
            "Create or overwrite a file with the given content, creating parent \
                directories as needed. Overwriting an existing file requires it to have been \
                read in this session with no changes on disk since."
                .to_string(),

        ),
        SynchronousTool::EditFile.definition(
            "Edit File".to_string(),
            "Apply one or more string replacements in a single call. Batch related \
                edits — several files, or several hunks of one file — into one list instead of \
                one call each. Each `old_string` must match exactly once unless \
                `replace_all: true` is set for that edit, and every file must have been read in \
                this session with no changes on disk since. Edits apply in list order and later \
                edits see earlier ones' effects. If an edit fails, the call stops there: earlier \
                edits stay applied, the rest are not attempted, and the result reports every \
                edit's outcome (applied / failed / not_attempted) in order plus the failing \
                index, so you can fix that edit and resend from it."
                .to_string(),

        ),
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
            input_schema: super::input::schema("bash").expect("registered tool input"),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "web_search".to_string(),
            title: "Search the Web".to_string(),
            description: "Search the public web through Horizon's fixed Exa adapter. Returns a \
                bounded list of titles, URLs, publication metadata, and relevant excerpts. \
                Requires EXA_API_KEY in Horizon's environment."
                .to_string(),
            input_schema: super::input::schema("web_search").expect("registered tool input"),
            permission: ToolPermission::RequireApproval,
        },
        Definition {
            id: "web_fetch".to_string(),
            title: "Fetch a Web Page".to_string(),
            description: "Fetch one public HTTP(S) URL with SSRF protection and bounded \
                redirects/body size. HTML is reduced to readable Markdown; text and JSON pass \
                through. A session must approve each exact destination host before contact."
                .to_string(),
            input_schema: super::input::schema("web_fetch").expect("registered tool input"),
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
        // `policy::plan_tool_call` seam.
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
        // `skill.read` is different: every session can call it, role-less
        // or not -- see `skills`' module doc.
        SynchronousTool::ReadConfig.definition(
            "Read Horizon Config".to_string(),
            "Read Horizon's config file: the resolved path and its current \
                contents, or an explicit \"does not exist yet\" result (with the path still \
                reported) if nothing has been written there yet. Takes no arguments."
                .to_string(),

        ),
        SynchronousTool::WriteConfig.definition(
            "Write Horizon Config".to_string(),
            "Replace Horizon's config file with the given complete content \
                (validated as well-formed TOML before writing). Preserve every entry the user \
                didn't ask to change -- this replaces the whole file, not just one section. \
                Overwriting an existing file requires it to have been read in this session \
                (via config.read) with no changes on disk since."
                .to_string(),

        ),
        SynchronousTool::SearchRecall.definition(
            "Search Persisted History".to_string(),
            "Search committed conversation text and tool calls/results across \
                persisted history (including turns no longer in your context window). \
                Case-insensitive substring match. Streaming deltas/reasoning are not included, \
                only what was actually committed. Default scope is this session; pass \
                scope: \"all\" to search every persisted session, or session_id to search one \
                other session (the two cannot be combined). Use recall.read to pull full \
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

        ),
        SynchronousTool::ReadRecall.definition(
            "Read Persisted History Window".to_string(),
            "Read an ordered window of committed messages, tool calls, and tool \
                results for a session starting at a given sequence number -- use after \
                recall.search to pull full context around a hit. Defaults to this session if \
                session_id is omitted. Output is capped in total size; call again with a later \
                from_sequence to continue."
                .to_string(),

        ),
        // `task` (`tools::explore`, `docs/agent-explore-design.md`) is
        // auto-allowed like every other read tool -- the session it spawns
        // can only read, and only inside the requester's own workspace root.
        // Since the 2026-07-28 asynchronous cutover
        // (`docs/agent-async-task-design.md`) the *call* finishes at once,
        // returning only a launch receipt; the report arrives later as a
        // notification injected into a later provider round.
        //
        // This description and `prompt::DELEGATION_ROUTING_SECTION` state
        // the same constraints from two surfaces; change them together.
        // The read-only sentence is stated rather than implied because the
        // generic `task` name reads as write-capable in these models'
        // training distribution, and a child asked to implement can only
        // explore again.
        Definition {
            id: "task".to_string(),
            title: "Delegate a Task".to_string(),
            description: "Launch a read-only agent in a parallel session sharing this workspace \
                to handle multi-step investigation autonomously. For open-ended codebase \
                exploration or multi-file search, prefer task instead of running searches \
                yourself — this keeps intermediate output out of your context. Describe the \
                question and the exact deliverable (paths, line numbers, facts, a step plan) in \
                the prompt. The task agent does its own orientation inside its own session. For \
                one to three known files, read them directly \
                instead. Task agents are read-only — they investigate, locate, and plan, but \
                cannot write files or run commands that modify state; implementation happens in \
                this session after the report returns. Runs in the background — you will be \
                notified when it completes; keep working in the meantime. \
                Prefer several narrowly scoped tasks launched in parallel in one \
                response over a single broad one. Returns immediately with the task session's \
                id, which is also how you re-read its report later with task_output."
                .to_string(),
            input_schema: super::input::schema("task").expect("registered tool input"),
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
            input_schema: super::input::schema("task_output").expect("registered tool input"),
            permission: ToolPermission::AutoAllowRead,
        },
        SynchronousTool::ReadSkill.definition(
            "Read Skill".to_string(),
            "Read one of this session's available skills by id (see the skills \
                listed in the system prompt) and return its full instructions."
                .to_string(),

        ),
        SynchronousTool::ReadKnowledge.definition(
            "Read Knowledge".to_string(),
            "Read one of this project's knowledge entries by id (see the system \
                prompt's project-knowledge section) and return its full frontmatter and body."
                .to_string(),

        ),
        SynchronousTool::WriteKnowledge.definition(
            "Write Knowledge".to_string(),
            "Create or update a knowledge entry for this project. Upserts by id: \
                an existing entry's `created` date is preserved while `updated` is refreshed. \
                No approval — the tool-event recording is the audit."
                .to_string(),

        ),
        Definition {
            id: "board.read".to_string(),
            title: "Read Board".to_string(),
            description: "Read the task board. If `id` is given, show that item with its \
                full comment thread; otherwise list all items in rank order, optionally \
                filtered by status. Items include their hierarchy, priority, dependencies, \
                completion, associated sessions, and conversation."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Show this item with its comments. If omitted, lists all items.",
                    },
                    "status": {
                        "type": "string",
                        "description": "Filter the list by status (e.g. proposed, ready, in-progress, review, done, blocked). Ignored when `id` is given.",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "board.update".into(),
            title: "Update Board Task".into(),
            description: "Create or edit ordinary tasks, set parent/dependencies, reorder among siblings, or record project-defined status and closure. For move, provide position and relative_to for before/after. Use close with is_closed=true for finished or withdrawn work, or false to reopen; optional status is updated atomically. Status text alone never changes is_closed.".into(),
            input_schema: super::board::update_schema(),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "board.session".into(),
            title: "Work on Board Task".into(),
            description: "consult: send a task session a request whose final answer goes to that task's board conversation. implement: request a worktree at explicit base for your assigned task, retaining this session. Wait for the environment outcome before editing. review: ask the task's separate reviewer to inspect base..tip with checks; its final answer returns to you. send: deliver text to a session, optionally requesting a reply to a session UUID. Inputs never cancel a running tool.".into(),
            input_schema: super::board::session_schema(),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "board.comment".to_string(),
            title: "Add Board Comment".to_string(),
            description: "Add a comment to a board item. The comment author is set \
                automatically from this session's id — you cannot set it. Comments are \
                append-only; the board event log is the audit trail. No approval required."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "text"],
                "properties": {
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Item id to comment on.",
                    },
                    "text": {
                        "type": "string",
                        "description": "Comment text (markdown).",
                    },
                }
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        SynchronousTool::UpdateMemory.definition(
            "Update Memory Document".to_string(),
            "Update your memory document — the structured summary of project \
                state that carries your context across turns. Each call edits individual \
                fields incrementally (set/append/clear); never regenerate the whole document. \
                Every turn must end with either a memory.update call or a `no_update` \
                declaration — the harness enforces this checkpoint. Fields: goal, decisions, \
                completed, in_progress, stuck, next_step, related (files and symbols). \
                `folded_log_range` optionally records the raw event-log sequence range \
                this update condenses, so recall.read can fetch the originals."
                .to_string(),

        ),
    ]
}

pub(crate) fn permission_for_tool(tool_id: &str) -> Option<ToolPermission> {
    catalog()
        .iter()
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

    /// The description says the task session orients itself, and
    /// `fs.grep`'s routing tail names the tool by its current id rather
    /// than a stale one.
    #[test]
    fn task_and_grep_descriptions_route_consistently() {
        let task = definition("task");
        assert!(
            task.description
                .contains("does its own orientation inside its own session"),
            "{}",
            task.description
        );
        // The delegation-routing section bans nothing the requester may do
        // before delegating, so neither does this description.
        assert!(
            !task.description.contains("do not orient"),
            "{}",
            task.description
        );

        // The generic `task` name reads as write-capable, so the read-only
        // constraint and where implementation happens are stated outright.
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

        // Several narrow launches rather than one broad one.
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
