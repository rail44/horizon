# Agent Pane Design

This document records the initial product and architecture decisions for the
Horizon Agent pane. It builds on `docs/ux-principles.md`.

## Decision Summary

The Agent pane is a pane-attached view of an agent session. It is not a global
assistant sidebar, and it is not a separate interaction model above terminals,
plugins, or workspace commands.

Decisions:

- Agents are sessions.
- Agent panes are attachments to agent sessions.
- Tabs and splits are session attachments, not session owners.
- Closing a pane or tab removes an attachment; it does not imply terminating the
  agent session.
- Terminating an agent session is an explicit operation.
- Agent capabilities operate through the Horizon command/tool model, subject to
  permissions.
- The MVP Agent pane uses a fixed chat composer for text input.
- Pane-local modes are deferred until transcript navigation, approval queues,
  or tool management require them.

## Role

The Agent pane shows and controls one agent session inside the workspace.

Responsibilities:

- Display the agent transcript.
- Send user messages to the agent session.
- Show agent state such as running, idle, waiting for user input, waiting for
  approval, failed, or terminated.
- Show tool calls, command requests, approval requests, and results.
- Accept keyboard focus as a normal pane.
- Keep local pane affordances minimal and contextual.

Non-responsibilities:

- Acting as a global assistant sidebar.
- Becoming a workspace or session management screen.
- Owning workspace commands outside the shared command model.
- Terminating the agent implicitly when an attachment is closed.

## Session Model

Agent sessions follow the same high-level model as terminal and plugin
sessions.

- `Workspace` owns tabs, panes, layout, attachments, and session metadata.
- `WorkspaceSession` stores durable session summary data such as id, kind,
  display number, title, and lifecycle summary.
- `SessionRegistry` owns live runtime handles.
- An agent runtime owns transcript state, bridge state, tool state, and process
  lifecycle.

Expected lifecycle states:

- `Created`
- `Running`
- `WaitingForUser`
- `WaitingForApproval`
- `ToolRunning`
- `Completed`
- `Failed`
- `Terminated`

Detached agent sessions should continue running by default. Horizon should make
background activity visible through session search, palette results, overview
state, or a lightweight status indicator.

## Input Model

For the MVP, the Agent pane uses a fixed chat composer.

When an Agent pane is active:

- Text input goes to the agent message composer.
- The Horizon command palette remains the primary command surface.
- Approval actions use explicit approve and deny UI.
- Pane switching, splitting, closing, attaching, detaching, and termination stay
  Horizon commands.

This avoids introducing an Agent-specific mode system before there is enough
pane-local complexity to justify it.

Pane-local modes may be introduced later for features such as:

- Transcript navigation.
- Tool call selection.
- Approval queue management.
- Re-running or editing previous agent actions.
- Agent-specific command entry.

## UI And UX

The Agent pane should match the density and hierarchy of other Horizon panes.
It should not visually dominate the workspace.

Pane header should show:

- Agent session title, such as `Agent #1`.
- Session state.
- Minimal local affordances such as close attachment.
- Explicit termination affordance only when needed and visually distinct from
  close.

Pane body should show:

- Transcript messages.
- Tool call and command request records.
- Approval prompts.
- Concise error state.
- Message composer.

## Execution Boundary

The preferred MVP bridge is stdio JSON-RPC to a local agent process. This keeps
the agent runtime isolated while preserving a simple integration surface.

Initial Horizon-to-agent messages:

- `initialize`
- `user_message`
- `cancel`
- `approve_tool_call`
- `deny_tool_call`
- `shutdown`

Initial agent-to-Horizon messages:

- `assistant_delta`
- `assistant_message`
- `tool_call_requested`
- `tool_call_started`
- `tool_call_finished`
- `approval_requested`
- `state_changed`
- `error`

Agents should not mutate `Workspace` directly. Agent operations should request
approved Horizon commands or tools, and Horizon remains responsible for
permission checks and execution.

Initial tool boundary:

- Read workspace state.
- List tabs, panes, and sessions.
- Focus tab or pane.
- Open a terminal.
- Split the active pane.
- Attach or detach sessions.
- Run approved Horizon commands.

Operations likely requiring confirmation:

- Sending input to a terminal.
- Terminating sessions.
- Executing external processes.
- Writing files or persistent state.
- Installing or loading plugins.
- Performing networked or otherwise externally visible work.

## MVP Implementation Shape

The implementation should start with the smallest first-class session path:

- Add an agent runtime handle to `SessionRegistry`.
- Make `NewAgent` create a real `SessionId`.
- Render an Agent transcript view instead of the placeholder body.
- Add a local mock agent or echo transport before wiring stdio JSON-RPC.
- Define `AgentCommand` and `AgentEvent` types.
- Route agent-requested Horizon operations through the command/tool model.
- Add explicit approval UI for operations that require user confirmation.

The provider-level contract is recorded in
`docs/agent-provider-contract.md`.
