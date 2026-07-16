# Theme Settings View Design

Status: decided in-session with the owner, 2026-07-16/17. Self-contained
record; `docs/roadmap.md` only indexes it. Builds on
`docs/theme-design.md` (the seed model; the config surface was narrowed
to exactly the seed on 2026-07-16, which is what makes this view's
scope small).

## Purpose

An interactive view for re-examining the whole color scheme. This is
the gate the owner set for judging derived values they cannot evaluate
in isolation (the ANSI brights foremost): edit the seed with immediate
feedback, inspect every derived color, then persist deliberately.

## Decisions

- **Form: a first-party pane**, not a modal. It opens like any view
  (view chooser / palette command), lands in a tab or split, and is
  Horizon's **first session-less first-party view** (roadmap foundation
  3, "native Rust views first"). The owner noted a modal could also
  suit "the app configuring itself" — the pane's render is
  form-agnostic, so a modal variant stays cheap to try later; judged by
  feel.
- **Edit surface: exactly the seed** — `surface_base` (color), the six
  `[theme.ansi]` hues (colors), `accent` (slot name or hex),
  `text_contrast` (slider). Nothing else is configurable since the
  2026-07-16 narrowing, so the view IS the whole theme surface.
- **Apply/persist: live-apply + explicit Save.** Control changes apply
  to the running app immediately (in-memory scheme swap through the
  existing `theme::reload_from` + `apply_gpui_component_theme` seam —
  terminal repaint is already immediate, see backlog 25's resolution).
  `config.toml` is written only on explicit Save; the file stays the
  source of truth, so unsaved live state is discarded on restart.
  Write-back must preserve the rest of the file (other sections,
  comments): use `toml_edit`, touching only the `[theme]` /
  `[theme.ansi]` entries.
- **Preview: derived-color swatch chips inside the view** (owner's
  pick, option b): the ~25 derived colors — ANSI 16 (10 of them
  derived), the text hierarchy (primary/muted/subtle), surfaces and
  borders, semantic four + diff colors — rendered as chips that update
  with the controls. The app itself doubles as the live preview for
  everything currently visible; the chips cover what happens not to be
  on screen (brights, diff, danger). Colored sample text lines
  (option c) were considered and deferred.
- **Widgets: stock gpui-component as the gallery shows them**
  (ColorPicker, Slider, the searchable select for the accent slot) —
  per the standing stock-components rule. The chips and layout are
  Horizon's own view code (our-component territory).

## Structural notes (first session-less pane)

Terminal and agent panes attach daemon sessions; this pane has none.
Consequences handled in the plumbing slice (slice 1, shipped):

- **Pane-kind shape**: `PaneKind` gained a third variant, `View(ViewKind)`,
  rather than a one-off `ThemeSettings` variant on `PaneKind` itself.
  `ViewKind` is `crates/horizon-workspace`'s own small enum
  (`ThemeSettings` today); a future image/markdown/git-diff viewer adds a
  `ViewKind` variant, not a new `PaneKind` variant, so every place that
  only cares "is this pane session-backed" keeps matching just
  `PaneKind::View(_)`. `PaneKind::session_kind(self) -> Option<SessionKind>`
  is the one seam that used to assume every `PaneKind` maps onto a
  `SessionKind`; it now returns `None` for `View`, and
  `Workspace::ensure_session` (the only caller) already short-circuits on
  a `None` session id before that would matter.
- **Creation flow**: the view chooser's existing `ViewChoice` list gained
  a "Theme Settings" entry (`kind: PaneKind::View(ViewKind::ThemeSettings)`,
  no role). No new `CommandId`: `SplitRight`/`SplitDown`/`NewTab` already
  open the generic chooser, and `WorkspaceShell::create_session` (the
  chooser's existing confirm handler, unchanged entry point) now branches
  on `PaneKind::View` early -- `Workspace::open_tab`/the new
  `Workspace::split_active_tab_with_view` create the pane with
  `session_id: None`, skipping the `pending_terminal_spawns`/
  `pending_roles` bookkeeping entirely (nothing to spawn). The CLI
  `CreateSession` vocabulary (`control_plane.rs`'s `new-terminal`/
  `new-agent`/`new-config-agent`) is **not** extended in this slice --
  `external_new_session` always mints a `SessionId` and spawns a
  process, which doesn't fit a session-less pane without its own
  parallel path; left for later if a CLI-driven Theme Settings pane is
  ever wanted.
- **Persistence**: decided **surviving** (the doc's preferred option).
  The `workspace.snapshot` persistence DTO (`persistence.rs`'s
  `WorkspaceState`) gained a `PaneKindState::View(ViewKindState)` variant,
  serialized as `{"view": "theme_settings"}` (serde's default
  externally-tagged newtype-variant shape) alongside the unchanged bare-
  string `"terminal"`/`"agent"`. A session's persisted kind
  (`SessionKindState`) stays a separate, still session-only enum --
  splitting it from the pane-kind DTO avoided ever needing a
  `SessionKind` conversion for a kind that has no session-kind
  counterpart. `WorkspaceState::validate`'s pane loop now requires a view
  pane to have `session_id: None` (new error: "view pane ... must not
  have a session attachment") instead of requiring every pane to resolve
  a session. `WORKSPACE_STATE_VERSION` did not need bumping: the new
  variant is additive to a tagged/`deny_unknown_fields` schema that
  already round-trips structurally-shaped data, and no existing field
  changed shape.
- **Close semantics**: trivially destructive, unchanged code path --
  `CommandId::CloseActivePane`/`CloseActiveTab` already funnel through
  `Workspace::detach_pane`, which returns `Option<SessionId>`; for a view
  pane that's always `None` (nothing to shut down), and
  `WorkspaceShell::reconcile` drops the pane's view entity from its
  `HashMap` the same as any other removed pane. The close-vs-detach seam
  for session panes (`docs/ux-principles.md`) is untouched.
- **View module**: `src/theme_settings/mod.rs` (a directory, mirroring
  `terminal/`/`agent/`'s per-domain convention, so slice 2's controls/
  chips/`toml_edit` save modules have somewhere to land without a
  rename). `ThemeSettingsView` is a minimal `Focusable` + `Render` GPUI
  entity; `WorkspaceShell`'s `PaneView` enum gained one variant,
  `ThemeSettings(Entity<ThemeSettingsView>)`, keyed the same way as
  `Terminal`/`Agent` (one `PaneView` variant per view kind, not a single
  generic bucket -- each hosts a genuinely different Rust view).
- **Verification**: `scripts/check-workspace-restore.sh` was not
  extended -- it drives creation entirely through the CLI
  (`new-terminal`), which this slice deliberately doesn't extend for
  Theme Settings (see Creation flow above), so there is no CLI verb to
  add a view pane through. The persistence round trip is instead covered
  by headless unit tests in `crates/horizon-workspace` (`persistence.rs`:
  a view pane survives `to_persisted_json`/`from_persisted_json`
  unchanged and without gaining a session; `validate` rejects a view pane
  carrying a session attachment) plus model-level tests in `tests.rs`
  (creating a view pane via `open_tab`/`split_active_tab_with_view`
  registers no session; closing one detaches nothing). The existing
  script was run unchanged as a regression check that ordinary
  session-backed restore still works.

## Out of scope

- base16/24 import (separate, deferred).
- Colorful-expression usage of the hue set (separate future theme).
- The modal variant (cheap later; judged by feel).
- Sample text lines in the preview (option c, deferred).

## Slices

1. **Session-less first-party pane plumbing** — workspace model + view
   chooser + persistence, with a placeholder view body. Headless
   testable.
2. **The settings view itself** — controls, swatch chips, live apply,
   toml_edit save.
