# Horizon UX Principles

This document captures the current product and UX direction for Horizon. It is
not a final design spec; it is a decision record for keeping implementation
choices aligned while the application is still early.

## Positioning

Horizon is a keyboard-first command workspace: a programmable interface between
the developer and the computer, where terminals, agents, and tools can be
composed into project workflows.

Horizon is not:

- A terminal emulator as the whole product.
- An AI chat app.
- A file-tree-first IDE.
- A GUI clone of tmux.
- A session management screen with panes attached.

The main visible object is the workspace. Commands are the primary way users
operate the workspace. Projects may become a context layer later, but Horizon is
workspace-first for now.

## Primary Objects

- `Workspace`: The visible working area. It owns tabs, panes, layouts, and
  workspace-level state.
- `Tab`: A layout surface inside the workspace.
- `Pane`: A viewport or attachment for displaying a session or tool view.
- `Session`: A live execution unit, such as a terminal, agent, or plugin view.
- `Command`: A named operation exposed by Horizon core, terminals, agents, or
  plugins.
- `Plugin`: A capability provider. A plugin may provide views, commands, agent
  tools, or background tasks.
- `Project`: A possible future context boundary. It is not a primary object for
  the MVP.

## Workspace And Project Scope

Horizon is workspace-first. Users do not begin by opening a formal project; they
work in a workspace and use terminals, agents, and tools with ad hoc context.

Project context is still useful in the future for:

- Agent context.
- Plugin permissions.
- Workspace restore.
- Project-aware commands.

For the MVP, avoid project switchers, project trees, and project-scoped command
palettes. Leave room for a future `ProjectContext` attached to sessions or
workspace metadata.

## Command Model

Horizon is command-oriented.

- Core features, terminals, agents, and plugins expose commands.
- Keyboard shortcuts are bindings to commands.
- Visible buttons and menus are also bindings to commands.
- The command palette is the primary command discovery and execution surface.
- Agents should use the same command/tool model when operating Horizon.

### What Becomes A Command

A command is a discrete operation that:

- Can take a target as an argument — a pane, tab, session, pending tool call, etc.
- Could meaningfully be exposed to agents under permission.
- Makes sense to invoke from the palette or bind to a key.

Not commands:

- Continuous or positional input: typing, scrolling, selection drags, IME composition.
- Pure display state: what's highlighted, expanded, or hovered.
- The palette's own argument-collection mechanics: query text, list navigation, selection index.

Persistent UI should not become a complete command surface. It should show
workspace state and expose only a small set of contextual affordances.

## Close, Detach, And Terminate

Close and terminate are different concepts.

- `Close Pane`: Remove a pane from the current layout. This does not necessarily
  terminate the session.
- `Close Tab`: Remove a tab surface. This does not necessarily terminate the
  sessions shown in that tab.
- `Detach`: Keep a session alive while removing one of its visible attachments.
  This may remain an internal concept or a less prominent UI term.
- `Terminate Session`: End the live session.

Current MVP behavior may temporarily terminate sessions when panes or tabs are
closed because detached session management does not exist yet. Treat that as a
temporary implementation constraint, not the long-term UX model.

## Agent Model

Agents are sessions.

- An agent session can be attached to a pane.
- Agents appear in the same workspace model as terminals and plugin views.
- Agent capabilities may be workspace-aware, but agent presence is
  pane/session-scoped.
- Agents may use Horizon commands/tools, subject to their permissions.
- A global assistant sidebar is not part of the default model.

This keeps AI integrated without making AI the visible center of the
application. Detailed Agent pane decisions are recorded in
`docs/agent-pane-design.md`.

## Plugin Model

Plugins are capability providers, not necessarily panes.

A plugin may provide:

- A view.
- Commands.
- Agent tools.
- Background tasks.

A plugin-provided view can run as a session and be attached to a pane. A plugin
that only provides commands or tools does not need a pane.

For the MVP, focus on view-providing wasm plugins as sessions. Keep the model
open for command and tool providers.

## Command Palette Direction

The command palette is the primary command surface.

For the MVP:

- Start with a command-first palette.
- Use two-step target selection for commands that need targets.
- Example: `Attach Session to Split` then choose a session.

For the long term:

- Use a typed result model.
- Allow commands, sessions, tabs, plugins, and recent items to appear in the
  same palette.
- Keep the palette extensible without forcing an object-manager UI into the main
  workspace.

The top-level toolbar used during the MVP is scaffolding. It can be removed or
moved to a debug/development mode once the command palette is available.

## Persistent UI Requirements

Persistent UI should show workspace state first, expose only minimal contextual
actions, and avoid becoming a full command surface.

Requirements:

- Show the current state clearly: active tab, active pane, input target, and
  visible workspace structure.
- Show what else exists: other tabs, visible panes, and eventually detached or
  running sessions when needed.
- Keep operations minimal and contextual. Common actions like new tab, split,
  and close may have visible affordances, but complete operation coverage
  belongs in the palette.
- Preserve keyboard-first flow. Mouse actions should update the same active
  state and command model used by keyboard actions.
- Keep the workspace dense and clean. Avoid large decorative UI and avoid making
  any single capability, such as terminal or AI, visually dominate the product.
- Make active state obvious. A user should always know where keyboard input will
  go.
- Separate hierarchy:
  - Tab bar: workspace surfaces and active tab.
  - Pane header: pane type, pane state, and local affordances.
  - Palette: command discovery and execution.
  - Inspector: optional state inspection and advanced management.
  - Status bar: lightweight feedback, diagnostics, or temporary state.
- Avoid hidden destructive behavior. Termination should be explicit and visually
  distinct from closing a surface.
- Leave room for growth. Terminal, agent, and plugin views must fit the same
  pane model without forcing all plugin commands into persistent UI.
