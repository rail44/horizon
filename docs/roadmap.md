# Horizon Roadmap

This roadmap translates `docs/ux-principles.md` into implementation phases. It
is a living document, not a fixed project plan. Update it when product decisions
or implementation constraints change.

## Phase 1: Command Model Foundation

Goal: make Horizon operations command-oriented so visible buttons, keyboard
shortcuts, the command palette, and future agent tools can call the same
operations.

Tasks:

- Define a minimal command model:
  - `CommandId`
  - command title
  - command category
  - optional description
  - enabled/disabled state
- Define an execution context for commands:
  - workspace state
  - session registry
  - terminal dump and clipboard dump hooks used by tests
- Move existing workspace operations behind commands:
  - New Terminal
  - New Agent
  - Split Active Pane
  - Focus Next Pane
  - Close Active Pane
  - Close Active Tab
- Keep the current toolbar temporarily, but make it execute commands instead of
  owning behavior directly.
- Add tests for command metadata and command execution effects.

Completion criteria:

- Existing toolbar actions go through command execution.
- Command definitions are inspectable by the future palette.
- Core command behavior is covered by unit tests.

## Phase 2: Command Palette MVP

Goal: introduce the primary command surface that can eventually replace the
temporary toolbar.

Tasks:

- Add palette open/close state.
- Open the palette from a keyboard shortcut.
- Render a command-first list of available commands.
- Implement simple filtering; substring matching is enough for the MVP.
- Support keyboard navigation.
- Execute the selected command with Enter.
- Close the palette with Escape.
- Keep the result model extensible for typed results later.

Completion criteria:

- New Terminal, New Agent, Split Active Pane, Focus Next Pane, Close Active Pane,
  and Close Active Tab are executable from the palette.
- Toolbar is no longer the only visible way to discover and run core commands.
- GUI smoke tests cover at least opening the palette and executing one command.

## Phase 3: Toolbar De-scaffolding

Goal: shift persistent UI toward workspace state instead of action listing.

Tasks:

- Hide or remove the top-level Terminal / Agent / Split / Next toolbar.
- Keep or replace only minimal contextual affordances.
- Add a small palette hint if needed.
- Revisit status bar responsibilities.

Completion criteria:

- The primary persistent UI is tab bar, pane headers, and workspace content.
- Common actions remain discoverable through the palette.
- The application remains usable without the temporary toolbar.

## Phase 4: Close Semantics Cleanup

Goal: separate closing display surfaces from terminating live sessions.

Tasks:

- Add detached session state.
- Make pane close remove an attachment rather than terminate the session.
- Make tab close remove a layout surface rather than terminate sessions.
- Add explicit Terminate Session command.
- Add minimal UI or palette access for detached sessions.

Completion criteria:

- Close and terminate are distinct in the model and UI.
- Detached sessions can be found and reattached.
- Destructive termination is explicit.

## Phase 5: Typed Palette Expansion

Goal: grow the palette from command-first MVP into a unified typed surface.

Tasks:

- Introduce typed results for commands, sessions, tabs, plugins, and recent
  items.
- Add session search.
- Add tab search/switching.
- Add plugin entries when plugin support exists.
- Define default and secondary actions for non-command results.

Completion criteria:

- The palette can search more than commands.
- Sessions can be found without a persistent session sidebar.
- The result model stays compatible with command execution.

## Phase 6: Recursive Layout Rendering

Goal: make UI rendering match the workspace layout model.

Tasks:

- Render `LayoutNode` recursively instead of using fixed pane slots.
- Support more than two panes.
- Support horizontal and vertical split axes.
- Preserve active pane behavior.
- Prepare for resize handles.

Completion criteria:

- Three or more panes render correctly.
- The displayed layout matches the workspace model.
- Active pane, keyboard input, and IME targeting remain correct.

## Phase 7: Plugin View MVP

Goal: support view-providing wasm plugins as sessions.

Tasks:

- Define plugin manifest/loading shape.
- Define the host interface for a view-providing plugin.
- Start plugin view instances as sessions.
- Attach plugin sessions to tabs and splits.
- Keep command/tool provider support as a future extension.

Completion criteria:

- A minimal plugin view can be launched and displayed in a pane.
- Plugin view sessions fit the same workspace model as terminal and agent
  sessions.

## Phase 8: Agent Session MVP

Goal: make agent sessions first-class workspace objects.

Tasks:

- Define the agent session model.
- Replace the placeholder agent pane with a live agent session.
- Add agent-related commands.
- Define the first Horizon tools exposed to agents through the command/tool
  model.
- Decide which operations require user confirmation.

Completion criteria:

- Agent sessions can be created, displayed, and terminated.
- Agents can use approved Horizon operations through the same command/tool
  model used by users.

## Parallelization Notes

Implementation should stay mostly sequential until the command and session
foundations stabilize. The strongest dependency chain is:

1. Command Model Foundation
2. Command Palette MVP
3. Toolbar De-scaffolding
4. Close Semantics Cleanup

Parallel work is more appropriate for design spikes or research:

- Floem command palette implementation options.
- Recursive split rendering approach.
- Wasm plugin host investigation.
- Agent tool permission model.

