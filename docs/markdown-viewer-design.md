# Markdown Viewer Design

Status: design decided in-session, 2026-07-22. First-party viewers v1: a
read-only local-Markdown view pane. Out of scope: editing, external URLs,
images, file watching, CLI control-plane verb, and non-Markdown file types.

## Purpose

Open local `.md` files as session-less first-party view panes, reusing the
`PaneKind::View` plumbing introduced for Theme Settings
(`docs/theme-settings-view-design.md`). The pane is read-only, renders with the
existing GPUI markdown renderer, and persists across workspace restarts.

## Decisions

### 1. Where the file path lives

**Chosen: a new `ViewState` payload attached to `Pane`, mirroring
`session_id: Option<SessionId>`.**

`Pane` gains `view_state: Option<ViewState>`. `ViewState` is a small enum that
currently has one variant:

```rust
pub enum ViewState {
    Markdown { path: PathBuf },
}
```

`ViewKind` stays a type-only enum (`ThemeSettings`, `Markdown`) and keeps its
`Copy`/`Eq`/`Hash` bounds. `PaneKind` therefore also keeps `Copy`.

Rationale:
- Keeps the public `ViewKind`/`PaneKind` surface small and stable. A file path
  is pane-instance data, not a kind discriminator, so storing it next to
  `session_id` is the existing pattern.
- Avoids making `PaneKind` non-Copy, which would ripple through every match
  site and the persistence DTO.
- Future first-party views with their own instance state (e.g. an image
  viewer with a path, a git-diff viewer with commit refs) can add a
  `ViewState` variant without touching `ViewKind`.

Rejected alternative: `ViewKind::Markdown(PathBuf)`. Simpler for one view but
mixes kind identity with instance data and removes `Copy` from `PaneKind`.

### 2. How the user opens a Markdown file

**Chosen: a new palette command `Open Markdown File…` (`CommandId::OpenMarkdownFile`).**

The command opens a simple path-input modal built on the same searchable-list
primitive used for the palette and view chooser. The user types or pastes an
absolute path, confirms, and the file opens in a new tab.

Rationale:
- Fits the existing command model exactly: a `CommandId`, palette entry, and
  keybinding id.
- Headless-testable: we can drive the modal creation and model mutation from
  unit tests without a file dialog.
- Out of scope for this slice: native file dialogs, drag-and-drop, and CLI
  `horizon open-markdown <path>`.

Rejected alternatives:
- Extend the view chooser with a "Markdown File…" entry: adds an extra step
  and the chooser is currently a static list, not a path input.
- Add a CLI verb first: useful but not required for the command-model slice.

### 3. Renderer

**Chosen: reuse `gpui_component::text::TextView::markdown`.**

The agent transcript already uses `TextView::markdown(id, text)`. The viewer
reads the file once at open time and renders the contents with a stable
`ElementId`. No new Markdown parser dependency is introduced.

Out of scope: image rendering, live reload, and a custom Markdown AST. If
those become needed, a dedicated parser crate can be evaluated then.

### 4. Persistence

**Chosen: add a `view_state` field to the persisted `PaneState` DTO.**

Serialization shape for a Markdown pane:

```json
{
  "id": "<pane-id>",
  "kind": {"view": "markdown"},
  "session_id": null,
  "view_state": {"markdown": {"path": "/home/owner/notes.md"}}
}
```

The `view_state` field is optional at the schema level and omitted when
`None`. Theme Settings panes omit it. This is an additive change to the
workspace persistence schema; `WORKSPACE_STATE_VERSION` does not need a bump.
Validation ensures a view pane's `view_state` matches its `ViewKind`.

### 5. View module

**Chosen: `src/markdown_viewer/mod.rs` mirroring `src/theme_settings/`.**

`MarkdownViewer` is a GPUI `Focusable` + `Render` entity. It stores the file
path and rendered text. The `WorkspaceShell::PaneView` enum gains a
`Markdown(Entity<MarkdownViewer>)` variant.

### 6. Workspace restore

`reconcile` already binds a `PaneView` for every pane. It will now bind
`PaneView::Markdown` for `PaneKind::View(ViewKind::Markdown)` panes, reading
`pane.view_state` to construct the viewer. Because the markdown viewer has no
daemon session, restore does not wait for sessiond inventory.

## Out of scope

- Markdown editing.
- External URLs or remote files.
- Image rendering (inline images are ignored/degraded to alt text by the
  GPUI renderer).
- File watching / live reload.
- CLI control-plane verb.
- Non-Markdown file types.
- Native file dialogs.
