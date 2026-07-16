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
Consequences to handle in the plumbing slice:

- A new pane kind in `crates/horizon-workspace` and the view registry;
  the view chooser and `CreateSession`-vocabulary seam need a
  first-party-view entry that does not spawn anything in sessiond.
- `workspace.snapshot` persistence: the pane must survive UI restarts
  (or be deliberately dropped on restore — decide in the slice, record
  here; surviving is preferred since it is cheap state).
- Close semantics: trivially destructive (nothing detaches — no
  session). The close-vs-detach seam is untouched for session panes.

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
