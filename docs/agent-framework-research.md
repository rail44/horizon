# Agent Framework Research

This document records the initial Rust agent/LLM framework research for the
Horizon Agent pane and plugin capability model.

Research date: 2026-06-29

## Design Context

Horizon already treats terminal, agent, and plugin surfaces as pane-attached
sessions. Because Horizon also supports wasm plugins, builtin functionality and
plugin-provided functionality should remain peers in the model.

The core design constraint is:

- Horizon core should define the session, pane, command, tool, permission, and
  plugin capability contracts.
- Agent framework crates should be implementation details of an agent provider,
  not part of the Horizon core model.
- The builtin agent should be one provider among others.
- A wasm plugin should be able to provide an agent capability through the same
  Horizon contract, even if it uses a different internal framework.

## Candidates

### `rig-core`

- Crate: <https://crates.io/crates/rig-core>
- Repository: <https://github.com/0xPlaygrounds/rig>
- Version checked: `0.39.0`
- License: MIT

Positioning:

`rig-core` is an opinionated Rust library for LLM-powered applications. Its
README describes agentic workflows, multi-turn streaming and prompting, a
unified provider interface, vector store integrations, and core-library WASM
compatibility.

Useful Horizon properties:

- Closest match to an agentic Rust framework.
- Built-in concepts for agents, tools, streaming, completion, embeddings, and
  provider abstraction.
- Has a `wasm` feature for the core library.
- Broad provider list.
- Good candidate for the builtin agent provider implementation.

Risks:

- More opinionated than a plain LLM client.
- Horizon must avoid leaking Rig-specific tool or agent types into core
  contracts.
- Some integrations and companion crates may be heavier than the MVP needs.

Assessment:

Best candidate when Horizon wants the builtin agent to use a Rust-native
agentic framework without hand-rolling the entire agent loop.

### `genai`

- Crate: <https://crates.io/crates/genai>
- Repository: <https://github.com/jeremychone/rust-genai>
- Version checked: `0.7.0-beta.7`
- License: MIT OR Apache-2.0

Positioning:

`genai` is a native-protocol multi-provider LLM client. Its README describes a
single Rust API over many providers, streaming, OpenAI Responses support, tool
choice, built-in tools, and provider-neutral tool declarations.

Useful Horizon properties:

- Strong fit as a provider-neutral LLM client layer.
- Keeps agent loop ownership in Horizon's builtin provider.
- Supports many providers and custom endpoints.
- Has explicit tool and streaming concepts.
- Less framework-shaped than `rig-core`.

Risks:

- Current latest is a beta release.
- It explicitly does not own the agent/workflow loop; Horizon would need to
  implement that loop.
- WASM compatibility is not as prominent in the crate metadata as `rig-core` or
  `async-openai`.

Assessment:

Best candidate when Horizon wants tighter control over its own agent loop and
only needs a provider-neutral LLM client.

### `async-openai`

- Crate: <https://crates.io/crates/async-openai>
- Repository: <https://github.com/64bit/async-openai>
- Version checked: `0.41.1`
- License: MIT

Positioning:

`async-openai` is an unofficial Rust client for OpenAI APIs based on the OpenAI
OpenAPI spec. Its README describes SSE streaming, granular feature flags,
Responses API, Assistants, Realtime, WASM, and OpenAI-compatible providers.

Useful Horizon properties:

- Strong OpenAI API coverage.
- Granular feature flags can keep surface area controlled.
- WASM support is explicit.
- OpenAI-compatible provider customization is available.

Risks:

- Lower-level than an agent framework.
- Provider-neutrality is limited compared with `rig-core` or `genai`.
- Horizon would own most of the agent loop and cross-provider behavior.

Assessment:

Best candidate for an OpenAI-first builtin provider or for a specialized
provider implementation, not for the Horizon core contract.

### `langchain-rust`

- Crate: <https://crates.io/crates/langchain-rust>
- Repository: <https://github.com/Abraxas-365/langchain-rust>
- Version checked: `4.6.0`
- License: MIT

Positioning:

`langchain-rust` is a Rust implementation of LangChain concepts. Its README
lists LLMs, embeddings, vector stores, chains, agents, tools, semantic routing,
and document loaders.

Useful Horizon properties:

- Has agent and tool concepts.
- Includes many higher-level building blocks.
- Familiar mental model for LangChain users.

Risks:

- Framework shape is large relative to Horizon's desired core boundary.
- Could compete with Horizon's own command/tool/session model.
- Provider and tool abstractions may impose assumptions that are not aligned
  with Horizon's pane/session/plugin design.

Assessment:

Worth tracking, but not the first choice for the builtin provider unless a
LangChain-style agent implementation becomes a deliberate product direction.

### Secondary Candidates

`llm-kernel`

- Crate: <https://crates.io/crates/llm-kernel>
- Repository: <https://github.com/epicsagas/llm-kernel>
- Version checked: `0.10.0`
- License: Apache-2.0
- Notable issue: crate metadata reports Rust `1.92`, which is ahead of the
  current stable toolchain assumption for Horizon.

`rmcp-agent`

- Crate: <https://crates.io/crates/rmcp-agent>
- Repository: <https://github.com/ZBcheng/rmcp-agent>
- Version checked: `0.1.6`
- License: MIT
- It extends `langchain-rust` with MCP tool integration and streaming tool
  execution. This is interesting for future MCP-oriented tooling but too
  specific for the first builtin agent provider decision.

## Comparison

| Candidate | Best Role | Agent Loop | Tool Calling | Streaming | Multi-provider | WASM Signal | Fit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `rig-core` | Builtin agent framework | Provided by framework | Strong | Strong | Strong | Explicit core WASM feature | High |
| `genai` | Provider-neutral LLM client | Owned by Horizon/provider | Strong client support | Strong | Strong | Unclear from metadata | High |
| `async-openai` | OpenAI provider client | Owned by Horizon/provider | Strong OpenAI support | Strong | Limited to OpenAI-compatible | Explicit | Medium |
| `langchain-rust` | LangChain-style agent provider | Provided by framework | Strong | Present | Moderate | Not primary signal | Medium/low |
| `llm-kernel` | Future tracking | Mixed | Unknown from quick pass | Unknown from quick pass | Likely | Unknown | Low now |
| `rmcp-agent` | MCP/langchain extension | Built on langchain-rust | MCP-focused | Present | Inherited | Unknown | Low now |

## Recommendation

Do not choose an agent framework for Horizon core.

Define a Horizon-owned agent capability contract first:

- `AgentProvider`
- `AgentSessionHandle`
- `AgentCommand`
- `AgentEvent`
- `AgentToolDefinition`
- `AgentToolCall`
- `AgentPermissionPolicy`

The builtin agent provider can then choose an internal crate. Initial preference:

1. Use `rig-core` if the goal is to ship a Rust-native agentic builtin quickly.
2. Use `genai` if the goal is maximum control over the agent loop with a
   provider-neutral LLM client.
3. Use `async-openai` only for an OpenAI-first provider.
4. Defer `langchain-rust` unless Horizon intentionally wants LangChain-style
   chains and agents.

## Plugin Implication

The plugin contract should not require plugins to use the same Rust crate as the
builtin agent. A plugin should expose Horizon's protocol-level capability:

```text
plugin manifest
  capabilities:
    view
    commands
    tools
    agent_session
```

For an agent-capable plugin, Horizon should only require:

- session start/stop
- user message input
- event stream output
- tool request output
- approval result input
- frame or transcript rendering data

This keeps builtin and plugin-provided agents as peers while preserving
Horizon-owned permission and command boundaries.

## Next Decision

The Horizon-owned `AgentProvider` contract is recorded in
`docs/agent-provider-contract.md`. Once the contract is implemented, the
builtin provider can be prototyped twice:

- `MockAgentProvider` for lifecycle and UI.
- `RigAgentProvider` or `GenaiAgentProvider` for real LLM behavior.

## Spike Result

Implementation date: 2026-06-29

Horizon now uses the Rig provider bridge as the standard Agent pane provider.
The earlier genai spike has been removed after comparison; genai remains only
as research context.

Implemented files:

- `src/agent_rig_spike.rs`

Registry behavior:

- Default builds register both `MockAgentProvider` and
  `spike.agent.rig-core`.
- `spike.agent.rig-core` is the default Agent pane provider.

Validation commands:

```sh
cargo test
```

Observed fit:

| Candidate | Message Mapping | Tool Call Mapping | Tool Definition Mapping | Agent Loop Ownership |
| --- | --- | --- | --- | --- |
| `rig-core` | `completion::Message` maps cleanly to committed messages and deltas | `AssistantContent::ToolCall` maps cleanly to `ToolCallRequested` | `completion::ToolDefinition` maps directly | Framework can own more of the loop |
| `genai` | `ChatMessage` text parts map cleanly to committed messages | `ToolCall` maps directly to `ToolCallRequested` | `chat::Tool` maps directly | Horizon/provider owns the loop |

Current interpretation:

- Both candidates can sit behind Horizon's provider-neutral contract without
  leaking framework types into core session, pane, or tool models.
- `genai` is the cleaner fit if Horizon wants to keep the builtin agent loop
  explicit and inspectable.
- `rig-core` remains the stronger fit if the builtin provider should lean on an
  agentic framework for orchestration, tools, memory, and future RAG.
- The next meaningful spike is a real provider turn with streaming, using an API
  key or a local OpenAI-compatible endpoint, while preserving the same adapter
  boundary.

## Rig Compatibility Spike

Implementation date: 2026-06-29

Before committing to a DuckDB-backed Horizon agent store, we checked whether the
provider-neutral `AgentEvent` transcript can still participate in Rig's
ecosystem.

Added to `src/agent_rig_spike.rs`:

- `rig_messages_from_horizon_events`
- tests that rebuild Rig `completion::Message` history from Horizon events
- tests that model a Horizon-mediated tool call as Rig-compatible history

Rig memory shape:

- Rig's `ConversationMemory` stores `Vec<rig_core::completion::Message>` per
  conversation id.
- A successful turn may append user messages, assistant messages, tool calls,
  and tool results.
- Tool results are represented as user-side `ToolResult` content.

Spike result:

- Horizon `MessageCommitted(User)` maps cleanly to `Message::user`.
- Horizon `MessageCommitted(Assistant)` maps cleanly to `Message::assistant`.
- Horizon `ToolCallRequested` maps back to an assistant `ToolCall`.
- Horizon `ToolCallFinished` maps to a Rig `Message::tool_result`.
- The resulting sequence is usable as Rig conversation history:
  user message -> assistant tool call -> user tool result -> assistant message.

This means Horizon can persist provider-neutral transcript/tool events and still
derive the Rig message history needed by `ConversationMemory`.

Important loss points:

- Horizon's current `AgentToolCallRequest` stores one call id. Rig has both
  `id` and optional provider-specific `call_id`.
- Rig `ToolCall` also has `signature` and `additional_params`; these are not
  preserved by the current Horizon event model.
- Rig `AssistantContent::Reasoning` should be lowered to `ReasoningDelta`,
  separate from `AssistantTextDelta` and committed assistant messages.
- Non-text tool result content can be flattened if Horizon only stores
  `serde_json::Value` output and converts it to text for Rig history.

Decision implication:

- It is still reasonable for Horizon to persist provider-neutral `AgentEvent`
  records as the durable transcript/audit layer.
- To avoid cutting off Rig ecosystem features, Horizon should add an optional
  provider payload field before long-term persistence is finalized.
- A practical event-store shape is:

```text
agent_events
  event_id
  session_id
  turn_id
  sequence
  event_kind
  horizon_event_json
  provider_id
  provider_payload_json nullable
```

For Rig-backed providers, `provider_payload_json` can retain loss-prone details
such as Rig tool call `id`, `call_id`, `signature`, `additional_params`,
reasoning blocks, and provider-native metadata. Horizon core should not depend
on this payload for normal UI, approval, or tool execution; it exists for
provider replay, advanced memory, and migration safety.

Implementation note:

- The Rig bridge builds a versioned payload with
  `schema = "horizon.rig.provider_payload"` and `version = 1`.
- The payload currently captures Rig tool call `id`, provider `call_id`,
  `signature`, `additional_params`, and the requested function name/arguments.
- The DuckDB store persists this payload as opaque `provider_payload_json`;
  Horizon core does not interpret it.

## DuckDB State MVP

Implementation date: 2026-06-29

The first DuckDB-backed state layer is recorded in
`docs/agent-duckdb-state-design.md`. JSONL is the durable event log and DuckDB
is a derived projection.

## Rig Provider MVP

Implementation date: 2026-06-29

The first Rig-backed provider loop is implemented in `src/agent_rig_spike.rs`.
Horizon uses `spike.agent.rig-core` as the default Agent pane provider.

The MVP intentionally avoids a configuration layer:

- If `OPENAI_API_KEY` is set, the provider creates a Rig OpenAI client and
  runs a real completion turn.
- `HORIZON_RIG_MODEL` can override the model; otherwise the provider uses
  Rig's `openai::GPT_4O_MINI` constant.
- If `OPENAI_API_KEY` is absent, the provider stays in deterministic fallback
  mode so tests and local UI wiring remain usable.

The provider uses Rig's low-level completion request path instead of
`Agent.prompt()`. This keeps tool execution mediated by Horizon: model-returned
Rig `ToolCall` values become `AgentProviderEvent::ToolCallRequested`, Horizon
applies policy and executes known tools, and `AgentCommand::ToolCallResult`
continues the Rig history as a follow-up model turn.

The MVP persists provider-neutral `AgentEvent` records with optional
`provider_payload_json`, then exposes queryable projections for transcript
messages, tool calls, tool results, and approval requests. Default builds remain
unchanged.
