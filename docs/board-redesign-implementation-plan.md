# Board redesign: implementation and transition plan

**Status: implemented, integrated, and live cutover verified, 2026-09-16.**
Commit `8e057dd` contains A–E and the verification tooling. The full host quality
gate, isolated daemon flow, native GUI inspection, selected-data rehearsal,
and live transition passed. After explicit owner approval, matching binaries
from main at `eb6c99c` were activated and 47 tasks / 185 messages migrated.
The existing terminal daemon, terminal and ordinary agent session identities,
attachments, and workspace layout were preserved.

### Implementation map

| Package | Current code |
| --- | --- |
| A. Task data and migration | `crates/horizon-board/src/{model,event,store,rank}.rs`, `crates/horizon-logd/src/writer.rs`, board CLI, and the isolated `crates/horizon-board/migration/` importer. |
| B. Inputs and results | `horizon-agent` contract and `providers/rig/session/{input,turn}.rs`; agentd persists explicit sends in the automatic event batch before the successful tool result. The dispatcher delivers from durable records and acknowledges receipts. |
| C. Environment and resume | `providers/rig/session/environment.rs`, `horizon-agentd/src/session/{run,resume}.rs`, and `worktree.rs`. |
| D. Common view | `src/board_pane.rs` and `src/board_pane/`, with ordinary session-history entry through workspace commands. |
| E. Roles and routing | `horizon-board/src/agents.rs`, embedded organizer/task/reviewer skills, and `horizon-agentd/src/board_flow/`. |
| F. Verification and transition | `scripts/check-board-flow.py` and the migration inventory/converter. Build, full host gate, importer tests, isolated flow, native GUI, selected-record rehearsal, and live cutover passed. The original data and matching old binaries are retained locally for recovery. |

Read state stores the **furthest displayed message** per reader and task;
all earlier posts in that conversation count as read. The owner requested this
correction after the initial cutover because invisible unread gaps could not be
located in the simple task-level UI. Existing read records collapse to their
furthest message without a log rewrite or a wire-format change.
`session_id` remains the task's consultation/implementation session. Each review
request creates a fresh ordinary reviewer session with its own worktree pinned
to the requested tip; `review_session_id` is only the latest reviewer reference.
Transport cursors are separate from UI read state. These implementation choices
refine the accepted requirements; they are not a repository-wide development
process.

Project integration policy can be supplied through
`.horizon/skills/board-integration/SKILL.md`, using Horizon's existing skill
discovery. Task roles advertise this optional skill when present. Horizon's
own repository supplies the agreed direct-integration policy there; other
projects can supply their own. The generic board skill does not hardcode main
integration or change how unrelated sessions obtain integration authority.

The historical source audit below used revision
`07ca0c8d87790092d935eed4e845fbf93d7dd1b3`, shared by main and the design
worktree at inspection time. Its path/line findings describe that revision,
not today's replacement code. The audit itself made no product or live-data
changes. This plan applies the accepted choices in
[board-redesign-design.md](board-redesign-design.md).

## 1. Accepted technical direction and scope

The owner accepted the following scope after clarification that this work
includes changes to the existing agent execution machinery as well as the board:

- Rebuild the board data operations and view, migrating required existing
  task content on the board side.
- Extend the existing agent runtime and agentd with working-environment
  activation for the same session, explicit resumption of an ended task
  session, and input/final-result delivery for the board connection.
- Register task-linked sessions in the existing session inventory and connect
  the task detail's history entry to the associated agent session.

Historical board-data migration concerns tasks and board conversations;
it does not migrate the agent conversation-log format or replace session
management. The old board-specific automation in agentd is also retired.
The contracts and work packages below retain the accepted implementation
boundaries. A–F, including the live transition, are complete as summarized above.

Retain the board's append-only storage, ordinary task identity, and native
session-less pane. Replace the September 13 workflow's executable behavior
with ordinary task operations, task-associated sessions, event-directed
answers, and the agreed operating skills.

**Scope correction from the owner:** retain only the functionality discussed
for this redesign and the data needed to support it. Existing code, fields,
automation-generated data, and compatibility with unused features are not
reasons to keep them in the new product.

In response, the implementation proposal narrows legacy decoding to a one-time
migration utility. This revises the earlier proposal to keep reading old
workflow records in the running board. Extract the required task content,
write new-model records, and make normal board operation depend only on the new
model. Preserve the migration source separately while verifying the conversion.
Build and validate the replacement in isolation, then switch matching binaries
and the prepared board data together.

The substantial runtime changes are a coordinated working-environment change
for an existing session, explicit resumption of an ended task session, and a
reliable final-result delivery boundary. The Git worktree helper and existing
notification paths provide starting points; neither supplies the entire new
behavior on its own.

### Explicit removal scope

| Remove from the active product | Replacement within the agreed scope |
| --- | --- |
| Special milestone type, milestone-only view, enable/pause/replan flow controls, and dedicated decision forms. | Ordinary recursive tasks, top-level filter, and the task's ordinary consultation. |
| Fixed planner/worker/verifier pipeline, reservation/replanning rules, scope-conflict scheduler, and automatic integration policy embedded in the coordinator. | Organizer/task/reviewer sessions using the agreed skills and project-specific integration policy. |
| Workflow-specific CLI/tool operations and their commands/keybindings, including `milestone`, `flow`, `answer`, and workflow `pause`/`resume`/`replan`. | Operations needed for ordinary task data and the accepted session connection. |
| Generic assignee/claim features and free-form structured `assignee`/`links` fields outside the agreed task model. | Explicit task/reviewer session references and useful human-readable references in the task body. |
| Workflow plans, decision objects, generation/revision machinery for the old flow, attempt/worker/verifier snapshots, scheduling `before` edges, and integration/achievement flags. | The agreed task relationships, completion fact, messages, and only the worktree/review/delivery metadata required by the new mechanisms. |
| Workflow-specific labels, fixed status-button vocabulary, comment-count/update columns, and history toggles that hide the consultation. | Accepted list/detail content, project-defined state display, and unread indication. |

Delete the corresponding active code and tests for retired behavior. Keep
legacy types only in the migration utility where needed to read its input;
normal task operations and agent roles must not depend on them. Preserve user
task descriptions and consultation content needed by the retained tasks.
Automatic transfer of every historical field or every generated task is not
the migration goal: produce a concrete data inventory before cutover and make
the retained records and references explicit. Keep the original source available
during this verification, without exposing it as another board feature.

## 2. Source findings and change boundaries

**Historical audit:** paths and line numbers in this section refer to revision
`07ca0c8d87790092d935eed4e845fbf93d7dd1b3`. They are preserved as the
evidence for the change boundaries, not a map of current code.

| Area | Verified behavior | Consequence for the plan |
| --- | --- | --- |
| Storage and replay | `horizon-board/src/model.rs:54` folds whole task snapshots from `WorkflowBatch`, including planner-created children. `horizon-board/src/event.rs:194` retains header IDs even for skipped events. | The migration utility must decode these snapshots and retain ID high-water. Reading only `ItemCreated` and ordinary updates loses source data before selection. |
| Ordinary writes | `horizon-logd/src/writer.rs:260` makes moves rewrite workflow ordering/revisions. At `:290`, edits can invalidate verification and request planning. | Replace these write semantics before enabling new task operations. UI changes alone do not isolate ordinary editing from the old flow. |
| Automatic execution | `horizon-agentd/src/main.rs:172` registers milestone roles and starts the coordinator plus the separate keeper wake path. `horizon-agentd/src/milestone/mod.rs:228` restores active workflow attempts and enters the scheduling/integration loop. | Retire these execution entry points during cutover. Preserve the useful keeper role-registration seam when wiring the new organizer. |
| Historical sessions | `horizon-agentd/src/session/resume.rs:183` reconstructs sessions using stored role IDs. | Define how legacy workflow sessions become historical records; removing coordinator startup alone is insufficient. |
| Environment activation | `horizon-agent/src/providers/rig/session/state.rs:47` separates provider history, memory, and clearing state from environment/prompt sections. `horizon-agentd/src/session/run.rs:225` builds the root-bound host/tool resources. | Retain conversation state and prepare/publish replacement environment resources at a serialized boundary. |
| Ended-session resume | `horizon-agentd/src/session/resume.rs:45` skips terminated sessions. `horizon-agentd/src/worktree.rs:255` adopts existing directories; provider reconstruction restores history in `horizon-agent/src/providers/rig/history.rs:55`. | Extract explicit single-session restoration and add missing-worktree reconstruction from the retained branch. |
| Final-result selection | `horizon-agent/src/providers/rig/completion.rs:887` commits interrupted partial text too. `horizon-agent/src/providers/rig/session/turn.rs:131` distinguishes outcomes; `horizon-agentd/src/wake/action.rs:201` waits beyond internal turn endings for settled state. | Persist an explicit terminal outcome and selected final message after internal continuations settle. |
| Notification delivery | `horizon-agent/src/providers/rig/session/turn.rs:63` injects child notifications after tool results; `horizon-agent/src/providers/rig/session/state.rs:195` supports idle wake. `horizon-agent/src/persistence/event_log/writer.rs:227` exposes acknowledged flush. | Generalize these seams and acknowledge persistence before forwarding, within the existing storage guarantee. |
| List and detail | `src/board_pane.rs:770` already keeps list/detail modes inside a session-less pane. At `:378`, filtering prefers milestones and hides closed statuses; at `:565`, workflow labels replace ordinary task status. | Reuse the pane and list machinery, implement the accepted ordinary hierarchy/filter rules, and remove workflow-specific presentation. |
| Detail content | `src/board_pane.rs:1280` renders a fixed status-button vocabulary and workflow content, conditionally hiding comments/composer. | Build the common parent/body/prerequisite/children/consultation detail and display project-defined state text. |
| Updates and navigation | `src/board_pane.rs:617` treats subscriptions as lossy refresh hints. `spawn_show` at `:958` guards the task ID; the comment completion callback at `:1080` lacks the equivalent guard. | Refresh from durable records, keep read state separate, and guard every asynchronous result against navigation. Existing refresh hints are not reliable work-delivery acknowledgments. |
| Session access | `src/workspace/commands.rs:50` imports a daemon session into the shell inventory before attaching it. `src/workspace/modals.rs:107` lists the shell's registered sessions. | Extract the ordinary-session adoption path for task start/resume/history access. Register task-linked sessions without changing the board into a session attachment. |
| Commands | `horizon-workspace/src/commands.rs:59`, `src/keymap.rs:146`, and `src/workspace/commands.rs:193` expose old workflow actions. | Replace the command surface along with the view and CLI/tool writes; accepted operations use explicit task/session targets. |
| Integration fixture | `scripts/check-board-milestone.py:197` isolates repositories, sockets, state, and a deterministic provider, then tests the old workflow. | Reuse its isolation technique and provider-fixture pattern, replacing its scenarios and assertions with the accepted flow. |

Paths beginning with a crate name above are under `crates/`.

The board and agent crates currently remain independent, with agentd composing
their capabilities (`crates/horizon-board/src/keeper.rs:12`). Preserve this
boundary: generic session input and results belong to the agent runtime;
agentd resolves board destinations and invokes board operations.

## 3. Work packages and dependencies

These packages describe reviewable implementation work, not new product stages
or board status names. The table records the original dependency plan; see the
implementation map above for current status.

| Package | Result | Depends on |
| --- | --- | --- |
| A. Data and migration contract | Ordinary task/message/session-reference model, sibling rank operations, recognizable completion alongside project-defined state, and isolated legacy importer. Specify shared input/result contracts with B. | Accepted design |
| B. Session input and result delivery | Durable event destination, queued inputs, explicit sends, and automatic final/failure/interruption delivery using one session mechanism. | Shared contract from A |
| C. Working environment and resumption | Explicit-base worktree activation in the same session, persisted environment identity, and owner-triggered resume of an ended task session. | A and B's session input/outcome boundary |
| D. Common board view | Accepted hierarchy/list/detail, dependencies, sibling reordering, unread/read position, navigation restoration, and ordinary session-history entry. | Task/message interfaces from A; data-only view work can proceed alongside B and C |
| E. Organizer, task, and reviewer wiring | Apply the agreed initial skills and connect registration, consultation, implementation, review, dependency notifications, and project integration policy. | B and C; shares task/session binding with D |
| F. Transition and complete-flow validation | Migrate retained task content; delete retired functionality and active data; verify new behavior, restart recovery, and shell/daemon compatibility; prepare cutover. | A through E |

Start with A's data and session contracts and validate the source-supported
environment approach early in the runtime work. Once the interfaces are stable,
the view work in D can proceed alongside the runtime work. B and C share the
session loop and host setup, so keep them under one implementation owner or
sequence their changes. Integrate those boundaries before E and F. Reuse concise
findings and read only the source relevant to each track instead of passing
every worker the full codebase or design history.

### A. Data and migration

- Limit the active data model to the agreed task fields and required message,
  read-state, session, worktree, review, and delivery records. Do not carry an
  opaque legacy workflow object or unused generic fields into every task.
- Preserve IDs, text, meaningful recorded status, sibling order, relationships,
  and conversation content for retained tasks. Preserve useful references as
  body content where appropriate, rather than retaining an unused links feature.
- Use a pure legacy fold inside the importer to understand whole-item snapshots
  and later edits before extracting the selected content. Write new-model
  records once; normal reads and writes then use only that model.
- Keep existing rank strings when importing; change ordering operations to
  operate among siblings and validate their targets. Improve the fractional
  key generator without requiring a board-wide rank rewrite merely to change
  the priority scope.
- Give board messages stable identities and retain source attribution. Imported
  messages can use deterministic source coordinates. Preserve duplicate text
  as separate messages. If importing useful conversation from old decision
  records, retain its attribution and represent unavailable timestamps honestly.
  Do not import the old decision schema or its machine-generated resolutions
  as new owner agreement. The original log remains the migration source.
- Store an interface-managed read position separately from task/session edits.
  Displaying a later post marks its whole conversation prefix read. Compare
  message order within the task and never regress for delayed earlier requests.
  Derive ancestor unread indication from descendant conversation/read positions;
  reading a parent's posts does not read a child's posts.
- Import does not emit fresh completion notifications, create runnable requests,
  or transfer old workflow results and verification flags into the active model.

### B. Input and result delivery

- Preserve the triggering input, its origin, and its optional destination for
  the duration of the work. Board posts and agent requests have explicit
  destinations; passive notifications may have none.
- Reuse the existing task-notification injection boundary for reading new
  input after a tool result and before the next decision. The current direct
  `UserMessage` path can cancel work and does not implement this behavior.
- Retain the active destination while incorporating consultation additions and
  result notifications. Persist other-destination requests for subsequent
  turns even when their content informs current work.
- Record the actual terminal result of the work, with final-answer identity
  or failure/interruption. `TurnEnded` alone is insufficient: internal
  continuation can end a turn without finishing the requested work. Committed
  assistant text can also contain interrupted partial output. Select the final
  successful provider round and finalize its result only after recovery and
  checkpoint processing has settled.
- Deliver from durable results and use source identity to avoid duplicate
  delivery across reconnects/restarts. Use the same delivery path for explicit
  sends and automatic results. This is transport bookkeeping, not a reply-tool
  obligation tracker or a content-adequacy check.
- Await acknowledged persistence before forwarding. The existing event writer's
  flush acknowledges buffer flushing, not `fsync`; this plan does not claim a
  stronger power-loss guarantee than the storage layer provides.
- Board-directed results become board records; session-directed results become
  session inputs. Task completion remains an explicit, separate board update.

### C. Working environment and resumption

- Extend the existing worktree helper to accept the selected base commit and
  retain the base, branch, worktree path, and repository identity.
- Coordinate activation when current tool work has settled. Rebuild all
  environment-bound resources together while retaining the session identity
  and conversation history: tool confinement, shell environment, skills and
  instructions, trust/approval context, provider environment, and persistence.
  Prepare the replacements before publishing so failure leaves consultation
  usable. Retain provider configuration, history, clearing/memory state, and
  the active answer route. A registry-root update or shell `cd` is insufficient.
- Keep environment activation distinct from ending the agent session so
  the transition does not invoke ordinary shutdown cleanup.
- Add explicit resumption for an ended task session when a new owner post
  arrives. Retain its recorded history and restore its owned environment.
  Serialize resumption against activation/termination for the same ID, and
  record a new lifecycle transition without deleting the old termination.
  Restore authoritative history from the event log or a demonstrably current
  projection; a successful projection query alone does not prove freshness.
  When ordinary cleanup removed a clean worktree, reuse its retained branch
  and recorded identity; preserve dirty worktrees. A missing or inconsistent
  environment is reported instead of silently choosing a new base.
- Validate history and resource consistency before committing to the exact
  refactoring. The corresponding runtime transition is now implemented but
  is covered by automated validation; no broader retention UI or
  configuration is proposed.

### D and E. View and operating flow

Use the existing pane/list/Markdown/input primitives and refresh subscription.
Organize the view by the accepted list, detail, and conversation responsibilities.
Keep plain task data visible independently of old workflow flags. Complete the
child and prerequisite queries, dependency editing, keyboard/drag reordering,
and per-message visibility tracking. Opening a detail must not clear unseen
messages below the viewport or in child tasks.

Reuse ordinary session registration and attachment for the history link. Task
start/resume must update the shell inventory even when the owner never opens
an agent pane. Closing the board keeps the associated agent session alive.

Reshape the keeper's role wiring into the board-wide organizer. Give it the
operations needed for priorities, dependencies, and selecting task sessions.
Use the same ordinary task session for consultation and implementation, plus
a separate reviewer with task/base/tip/check evidence. The current read-only
delegation tool provides a notification pattern, not a ready-made general
reviewer lifecycle. Skills capture the agreed intent and remain adjustable.

## 4. Transition sequence

1. **Prepare an isolated copy and inventory.** Record the board log, tasks,
   historical session links, and outstanding legacy work. Identify retained
   tasks/content and excluded automation-only data. Rehearse conversion and
   new writes against a separate test repository and daemon paths.
   Worktrees share the real board through the Git common directory, so merely
   changing worktree does not isolate a board test.
2. **Quiesce the legacy execution paths at cutover.** Account for the coordinator,
   old keeper wake action, persisted legacy-role resumption, and any already
   running work. Preserve session transcripts, branches, and uncommitted work.
   The replacement daemon must not reserve or integrate old workflow work.
3. **Switch matching binaries and prepared data together.** After the source
   stops changing, convert the retained content and verify the new records.
   Delete the old workflow mutations, CLI/tool paths, command bindings, and UI.
   Normal execution uses only the new model; the importer and its source are
   separate from the running board. Enable writes after migration checks pass.
4. **Enable the new flow.** Wire organizer events and task sessions after the
   read/write and runtime boundaries pass. Historical replay alone must not
   become a stream of new task registrations or requests.
5. **Validate recovery and retain a rollback copy.** Confirm retained task/message
   identities and content against the migration inventory, no old execution,
   and successful new consultation through
   implementation/review. Keep the pre-cutover logs and binaries together;
   older binaries must not be pointed at logs after new writes without a
   compatible reader or restoration of the matching saved data.

Wire compatibility and accurate historical data extraction are separate obligations.
Changing the board RPC types requires the log protocol version pair and schema
artifact to change; agent command/event changes require the corresponding
agent protocol artifacts. A protocol bump requires a full Horizon restart
with matching daemons. No terminal protocol change is currently indicated.

## 5. Validation plan

| Boundary | Meaningful checks for implementation |
| --- | --- |
| Migration | Batch-only child creation, later snapshots/edits/comments, skipped-event ID high-water, selected-record preservation, valid retained references, stable message IDs, and honest handling of missing timestamps. New active records contain no obsolete workflow payload. |
| Task operations | Reorder only siblings without altering dependencies or creating old scheduling edges; reject invalid references and cycles; recognize completion independently of intermediate state text. |
| Input/result routing | Correct board/requester destination; notify during a running tool without cancellation; queued other-destination requests; final-only output despite internal continuations; failure/interruption and restart without duplicate replies. |
| Environment | Consult without a worktree; activate at the chosen base with unchanged session/history; verify all tools use the new environment; restore after termination; fail coherently when restoration is impossible. |
| View/session integration | Top-level and child navigation, selection/position restoration, late asynchronous results after navigation, visible-only read advancement, descendant unread, registered detached sessions, and ordinary history access. |
| Complete flow | Deterministic provider in an isolated repository: registration, automatic priority/dependency organization, owner consultation, same-session implementation, multi-commit review/correction, project-specific integration, and prerequisite notification. |
| Transition | Fresh startup and restart perform no legacy planning, reservations, wake work, or integration; imported content is readable, creates no new work by itself, and is usable without the importer or old log. |

Use existing pure model/navigation tests where relevant and adapt the isolated
daemon/provider fixture. Boundary tests that require sockets or real daemons
belong in the existing sandboxed-profile exclusions with documented reasons;
they must still run in the host integration environment. Verify the native GUI
with representative parent/child tasks and a long consultation, including
scroll jumps that must read through the displayed post while preserving later
posts and unread child conversations.

Implementation validation follows the repository gate: `cargo fmt`, workspace
Clippy with warnings denied, the appropriate workspace nextest profile, and
the wire-schema checker. Build the whole workspace before daemon integration
checks. The original source-only audit ran no new behavior tests.

### Validation results, 2026-09-16

- `cargo build --workspace --locked`: passed with matching shell and daemons.
- Mandatory gate: `cargo fmt`, `cargo clippy --workspace --all-targets --locked
  -- -D warnings`, `cargo nextest run --profile sandboxed --workspace --locked`
  (1,815 passed; 73 host-boundary tests skipped by the existing profile), and
  `./scripts/check-wire-schema.sh`: passed.
- Regenerated agent/log schemas; agent protocol is 20, log protocol is 5.
  The terminal artifact is unchanged. A full app restart with matching binaries
  is required at cutover.
- Isolated importer: two Python unittest cases passed before live migration.
- `python scripts/check-board-flow.py --bin-dir target/debug --keep`: passed
  with 49 requests to a local deterministic provider. It exercised registration,
  priority/dependency updates, consultation without a worktree, daemon restart,
  same-session activation, three commits with correction and two independent
  reviews, fixture-policy integration, and dependency notification. Each reviewer
  inspected its requested tip in a separate worktree while the task worktree
  deliberately contained uncommitted changes. Message IDs remained unique.
- The fixture reproduced a tool-result/environment-activation ordering race;
  the fix drains already-enqueued host commands before releasing the provider's
  next decision. A deterministic regression test checks activation-before-result
  ordering. Stop priority, retained grants, and delivery replay/retry are also
  covered by automated tests. The initial sparse read-state checks were later
  replaced by the owner-requested read-through behavior described above.

- Full default host gate at commit `8e057dd`: formatting, workspace Clippy,
  **1,952 nextest tests passed (11 skipped)**, and wire checker passed. This also
  covers the boundary tests excluded by the sandboxed profile. The host run
  exposed an obsolete logd assertion for `item-created`; it now checks the
  typed current-version `ItemStored` envelope. The full gate passed afterward.
- Native GPUI on isolated Xvfb, inspected as screenshots and driven with real
  keyboard/mouse events: hierarchy/top-level filter, common child detail,
  scroll/selection restoration, prerequisite search/add/remove, free state plus
  independent completion, sibling move/drag, owner consultation, and ordinary
  working history passed. This initial run checked the then-current sparse
  read behavior; the subsequent owner correction replaces that behavior with
  an inclusive read position. Unopened child consultation remains independent.
- Explicit CLI termination followed by a native owner post resumed the same
  task session ID, emitted `SessionResumed`, and displayed both old and new
  inputs/replies in its ordinary history. All isolated fixture processes were
  stopped afterward. Native inspection also found and fixed dark-theme
  Markdown text color in the task body and consultation.
- A shared-lock snapshot of the actual legacy log was inventoried and converted
  in isolation. The real CLI/logd loaded all **47 retained tasks and 185 messages**
  exactly, including 30 owner posts. Edit/reorder/parent/dependency/free-state
  writes passed. The next created task was ID 50, preserving high-water 49.
- Starting the replacement agentd against an import-only fixture produced zero
  provider requests, agent events, or extra board events during the five-second
  observation; imported bytes remained unchanged. Startup and explicit resume
  skip retired keeper/planner/worker/verifier roles without deleting their
  history, branches, or worktrees.

The flow and GUI fixtures use isolated repositories, dedicated daemons/sockets,
and deterministic local providers. They verify runtime behavior and native UI,
not a real model's judgment. Main was fast-forwarded to `8e057dd` and rebuilt;
the documentation-only follow-up `eb6c99c` was the main revision at live cutover.

### Completed live cutover

The retained selection is IDs **1–47**. IDs **48–49** are workflow-generated
children of task 43 with no owner posts or comments; they concern the retired
workflow/structured-links features and are omitted from the active conversion.
The original log keeps their full historical content. Task 25 is preserved
conservatively despite a later task describing it as mistakenly created.
All retained relationship references are closed, and original text, authors,
message order, duplicates, and timestamps are preserved. Historical runtime
references are archived rather than rebound to new execution.

The main checkout's ignored `.horizon/board-redesign-cutover/` directory holds
the source snapshot, selection and exact comparison results, converted log,
native GUI screenshots/action journal, session inventory, and copies of the
actual running old binaries (including the unlinked agentd executable).
The source SHA-256 is
`613a19afe4a14ba94491e48f95cbf3ebffc406be1048c18ad0f9dd7fec02dc91`.

The owner explicitly authorized the live transition after automatic approval
review had blocked the earlier attempt. The cutover ran in an independent
process with file logging, so the terminal hosting Codex was not its lifetime
owner. It rechecked the original source hash, settled agent state, binary
hashes, process identities, and configured paths before stopping the old UI,
agentd and logd. It saved the stable board/agent logs, DuckDB and workspace,
atomically installed the converted board, and started the new UI and agentd.
The runner completed successfully in approximately 4.5 seconds. No rollback
was needed.

Independent post-startup verification confirmed:

- New UI PID 22784, agentd PID 22842, and logd PID 23909 use the prepared
  binaries. Logd starts on demand; a read-only board subscription started it
  and returned the expected cursor 48 (47 task imports plus ID high-water).
- Terminald PID 1863 has the same process start time and binary hash as before
  cutover. The terminal hosting Codex continued running.
- Terminal session `3d81c861-9c71-4cfe-b7e5-22007234e353` and ordinary agent
  session `d511ed71-a3e3-4264-ba41-a6462044746d` are the same two attached
  sessions. The saved and restored tabs and active tab match exactly.
- All 47 complete task records and 185 messages match the prepared conversion.
  The new board SHA-256 is
  `8551d39868dc9fbb9ba279d1c74a9da92c0df63896ce08733dfe2fb7c78b1f0d`;
  it remained unchanged after startup and the read-only subscription.
- Retired-role sessions are absent from the live inventory. All six agent
  events appended during startup belong to the existing ordinary agent session;
  no imported task triggered new board work.

The local transition directory contains `runner-result.json`,
`cutover-result.json`, `live-verification.json`, `live-subscription.json`,
and the stable original data set in `stable-data/`. Its rollback routine was
also checked in six isolated success/failure scenarios using real temporary
files and simulated process/CLI boundaries, including WAL restoration and
failure to write diagnostics. Those checks do not claim a live rollback was
performed. A future rollback must restore the saved data together with its
matching saved binaries.
