# Agent Provider Contract

This document defines the Horizon-owned contract for agent-capable session
providers. It is a design contract, not yet a Rust API commitment.

## Decision Summary

Agent capability is provided by a `SessionProvider`, not by a special global AI
surface.

Decisions:

- Horizon core owns the agent provider contract.
- Builtin agents and plugin-provided agents use the same contract.
- Agent framework crates such as `rig-core`, `genai`, or `async-openai` are
  provider implementation details.
- Agent sessions are ordinary Horizon sessions with an agent capability.
- The Agent pane renders Horizon agent session state, not provider-native UI
  objects.
- Agents request Horizon operations through tools or commands; providers do not
  mutate `Workspace` directly.
- Permission checks happen in Horizon before a requested operation is executed.

## Provider Shape

Conceptually, an agent provider is a capability-bearing session provider:

```text
SessionProvider
  id
  display name
  capabilities
    view?
    commands?
    tools?
    agent_session?
```

For the builtin agent:

```text
provider id: builtin.agent
capability: agent_session
implementation: MockAgentProvider, RigAgentProvider, GenaiAgentProvider, etc.
```

For a wasm plugin:

```text
provider id: plugin.<manifest id>
capability: agent_session
implementation: wasm module through Horizon's plugin host interface
```

Horizon should treat both as providers of the same capability. The fact that
one is builtin and one is wasm-hosted should affect loading, trust, and
permission defaults, not the session model.

## Core Types

The future Rust API should be shaped around these Horizon-owned concepts:

```rust
struct AgentProviderId(String);
struct AgentSessionId(SessionId);
struct AgentRequestId(String);
struct AgentToolCallId(String);

trait AgentProvider {
    fn provider_id(&self) -> AgentProviderId;
    fn start_session(&self, request: StartAgentSession) -> AgentSessionHandle;
}

struct AgentSessionHandle {
    commands: Sender<AgentCommand>,
    events: Receiver<AgentEvent>,
}
```

The exact transport may differ by provider. A builtin provider may use channels
in-process. A stdio provider may bridge JSON-RPC. A wasm provider may bridge
host calls. The pane and workspace should see the same command/event model.

## Commands Into An Agent Session

`AgentCommand` is Horizon-to-provider.

Initial command set:

```rust
enum AgentCommand {
    Initialize(AgentInitialization),
    UserMessage { text: String },
    Cancel { request_id: Option<AgentRequestId> },
    ApproveToolCall { call_id: AgentToolCallId },
    DenyToolCall { call_id: AgentToolCallId, reason: Option<String> },
    Shutdown,
}
```

Notes:

- `Initialize` gives the provider a Horizon-defined session context.
- `UserMessage` is the fixed-composer MVP input path.
- `Cancel` should cancel the active turn or a specific request when supported.
- `ApproveToolCall` and `DenyToolCall` are responses to pending approval
  requests created by Horizon.
- `Shutdown` terminates the live agent session.

## Events From An Agent Session

`AgentEvent` is provider-to-Horizon.

Initial event set:

```rust
enum AgentEvent {
    StateChanged(AgentSessionState),
    ReasoningDelta(AgentMessageDelta),
    AssistantTextDelta(AgentMessageDelta),
    MessageCommitted(AgentMessage),
    ToolCallRequested(AgentToolCallRequest),
    ToolCallStarted(AgentToolCallId),
    ToolCallFinished(AgentToolCallResult),
    ApprovalRequested(AgentApprovalRequest),
    Error(AgentError),
    Exited(AgentExit),
}
```

Notes:

- `ReasoningDelta` supports streaming thinking/reasoning UI.
- `AssistantTextDelta` supports streaming assistant response text.
- `MessageCommitted` gives Horizon a stable transcript item.
- `ToolCallRequested` is not execution. It is a request for Horizon to evaluate.
- `ApprovalRequested` is a provider-visible reason to block until user or
  policy response.
- `Exited` is runtime lifecycle, distinct from detached pane state.

Provider runtime transport uses an event envelope:

```rust
struct AgentProviderEvent {
    event: AgentEvent,
    provider_payload: Option<serde_json::Value>,
}
```

`event` is the Horizon-owned contract used by UI, policy, tools, and replay.
`provider_payload` is provider-owned opaque JSON for replay or migration
metadata. The Agent pane should render from `AgentEvent`, not from provider
payload. This keeps the agent flow usable without the Horizon frontend while
letting Horizon persist provider-specific details when present.

## Standalone Flow Boundary

Horizon is the frontend and pane host for the agent experience, not the owner
of provider-native execution. A provider should be able to run its agent loop
outside the Horizon UI as long as it can exchange the same normalized commands
and events:

- `AgentCommand` is the input contract from Horizon or another host.
- `AgentEvent` is the portable output contract for UI, policy, tools, and
  replay.
- `AgentProviderEvent.provider_payload` is optional host-persisted metadata,
  not a dependency for normal pane rendering.

This keeps richer Agent pane rendering open while avoiding a PTY-like
input/output constraint. Horizon can render structured messages, approvals,
tool state, and future provider-specific affordances from the normalized event
stream plus optional persisted payloads.

## Session State

Horizon should normalize provider-specific state into a small set:

```rust
enum AgentSessionState {
    Created,
    Running,
    WaitingForUser,
    WaitingForApproval,
    ToolRunning,
    Completed,
    Failed,
    Terminated,
}
```

`Workspace` should store only summary state needed for tabs, panes, palette
items, and overview. Transcript, pending calls, and provider internals belong
to the agent runtime or a session state store owned by the runtime layer.

## Transcript Model

The pane needs provider-neutral transcript items:

```rust
enum AgentTranscriptItem {
    UserMessage(AgentMessage),
    AssistantMessage(AgentMessage),
    ToolCall(AgentToolCallRecord),
    Approval(AgentApprovalRecord),
    Error(AgentError),
}
```

The transcript model should be Horizon-owned even if the provider's internal
framework uses different message types.

This lets:

- builtin providers swap frameworks,
- wasm plugins report agent progress,
- pane rendering stay provider-neutral,
- future persistence avoid framework-specific serialization.

## Tool Boundary

Agent tools are Horizon-owned operations exposed to providers.

Initial tool definition:

```rust
struct AgentToolDefinition {
    id: String,
    title: String,
    description: String,
    input_schema: serde_json::Value,
    permission: AgentToolPermission,
}

enum AgentToolPermission {
    AutoAllowRead,
    AutoAllowUi,
    RequireApproval,
    Deny,
}
```

Initial tool families:

- workspace state read
- list tabs, panes, and sessions
- focus tab or pane
- open terminal
- split active pane
- attach or detach sessions
- run approved Horizon command

Deferred tool families:

- terminal input
- process execution
- file writes
- networked work
- plugin install or load
- persistent workspace mutation

Deferred tools may still be implemented early, but they should require explicit
approval by default.

## Permission Boundary

Providers do not decide whether a Horizon operation is allowed. They can only
request operations.

Permission flow:

```text
provider emits ToolCallRequested
Horizon matches requested tool
Horizon evaluates policy
  auto allow
  request approval
  deny
Horizon executes approved operation
Horizon sends tool result back to provider
```

Provider trust should influence defaults, not bypass the model:

- builtin provider may have more convenient defaults,
- local trusted plugin may have broader permissions,
- untrusted wasm plugin should start constrained,
- destructive operations still require explicit policy.

## Plugin Boundary

The wasm plugin contract should expose capability at the protocol level, not by
sharing Rust framework types.

Manifest direction:

```text
capabilities:
  - view
  - commands
  - tools
  - agent_session
```

Agent-capable plugin host operations:

```text
start_agent_session
send_agent_command
poll_agent_event / subscribe_agent_events
shutdown_agent_session
```

This keeps plugin-provided agents independent from `rig-core`, `genai`, or any
other crate used by the builtin provider.

## Rendering Boundary

The Agent pane should render Horizon's transcript and session state. It should
not render provider-native UI.

Provider-supplied view capability may exist separately:

- `agent_session`: provider runs an agent and emits agent events.
- `view`: provider owns a pane view.

A plugin may provide both, but Horizon should not require both for an agent
session. The builtin Agent pane can render any provider that implements
`agent_session`.

## MVP Contract

The first implementation should include:

- `MockAgentProvider`
- real `SessionId` for `NewAgent`
- `AgentCommand`
- `AgentEvent`
- normalized transcript items
- fixed composer input
- state indicator
- shutdown support
- one read-only workspace tool
- one approval-gated mock tool

This proves the pane/session/provider lifecycle before binding to a real LLM
framework.

## Open Questions

- Should `AgentSessionHandle` live in `SessionRegistry`, or should there be a
  provider registry plus per-session runtime store?
- Should transcript persistence be owned by the runtime layer or a future
  workspace persistence layer?
- Should wasm agent event streaming use polling first or host callbacks first?
- How much provider identity should be visible in the pane header?
- Should provider capability be attached to `PluginManifest` now or introduced
  with the Plugin View MVP?
