# Preview pane

Status: v1 implemented 2026-09-21.

A preview pane shows a gpui view that is still being developed inside a pane
of the running Horizon, without merging the branch and without restarting
the app. The view is compiled into a WebAssembly plugin; Horizon loads the
plugin, draws it with its own renderer, and reloads it when the file on disk
changes.

## Mechanism

The plugin runs a complete GPUI `App` inside a wasmtime component
(`wasm32-wasip2`). Its windows do not rasterize anything: each frame is
shipped to the host as a display list, and the host-side
`embedded_gpui::Surface` replays that list as native GPUI primitives. Text
is shaped and rasterized by the host's text system. A previewed view is
therefore painted by the same renderer, with the same fonts, as a native
view in the neighbouring pane.

This is [embedded_gpui](https://github.com/zed-industries/embedded_gpui),
an experimental Zed project. Horizon depends on a fork,
`rail44/embedded_gpui` branch `gpui-pre`, pinned by revision. The fork
exists because upstream takes `gpui` from the zed repository while Horizon
and gpui-component build against the crates.io `gpui-pre` snapshot family;
those are different packages, and a second gpui in the graph would make
the plugin surface unusable in Horizon's element tree. The fork's delta is
the dependency retarget, the API follow-ups the snapshots need, and
`Surface::scene_summary()`, which the windowless check reads. When
`gpui-pre` is bumped in Horizon, the fork has to build against the new
snapshot too.

## Where the code lives

The root package has a library target (`src/lib.rs`) next to the binary.
`src/main.rs` only calls `horizon::run()`; the former body of `main.rs` is
`src/entry.rs`. The plugin links this library, so the library must build
for `wasm32-wasip2`:

- Dependencies that cannot build there are declared under
  `[target.'cfg(not(target_family = "wasm"))'.dependencies]` in the root
  `Cargo.toml`.
- Modules that need them carry `#[cfg(not(target_family = "wasm"))]` at
  their declaration in `src/lib.rs`. `theme`, `preview` and `board_pane`
  build for both targets; every other module is native-only. A module that
  carries previews of its own view is gated inside itself instead, so the
  view and the native-only halves it uses can live in one directory.
- `scripts/check-preview-wasm.sh` checks that the library still builds for
  the plugin target. It is part of the quality gate.

`src/preview/` holds the feature:

| Module | Target | Contents |
| --- | --- | --- |
| `schema` | both | The two root interfaces the ends talk through. |
| `registry` | both | The list of named previews. |
| `sample` | both | The one seeded preview; the template for new ones. |
| `guest` | wasm | The plugin's entry point. |
| `host`, `pane`, `watch` | native | Theme publication, the pane entity, the file watcher. |

`preview-plugin/` is the plugin crate: a `cdylib` in its own sub-workspace,
excluded from the root workspace so `cargo build --workspace` never builds
it for the host. Its source is one line registering
`horizon::preview::guest::PreviewGuest`; every preview lives in the root
library. Its `Cargo.lock` is tracked and seeded from the root lockfile so
the shared packages resolve identically.

## Named previews

A preview is a name plus a constructor that builds the view with sample
data:

```rust
pub struct Preview {
    pub name: &'static str,
    pub build: fn(&mut Window, &mut App) -> AnyView,
}
```

`registry::PREVIEWS` lists them. A preview is written next to the view it
shows, the way a story sits next to a component. One plugin contains every
registered preview; the host selects one by name.

For a view to be previewable, the code that goes into the plugin must not
reach sockets, subprocesses, or the filesystem directly — a wasm32-wasip2
guest has none of them. Whatever the view uses to reach the outside world
has to be replaceable with sample data in the preview constructor. The
shape is not prescribed: a view backed by a session entity is handed a
sample entity; a view that reads a store is handed a store with sample
contents. The replaceable seam belongs in the store or session entity
rather than in a new layer inside the view.

## Host and guest vocabulary

Host root (`PreviewHost`):

- `theme() -> Ref<PreviewThemeApi>` — the object the guest observes for
  scheme changes. `PreviewThemeApi::theme_json()` returns the current
  `[theme]` input as JSON of `horizon_config::RawThemeConfig`.
- `selected_preview() -> String`.

Guest root (`PreviewPlugin`):

- `preview_names() -> Vec<String>`.
- `mount(surface: Ref<SurfaceApi>)` — the guest reads `selected_preview()`
  and opens that preview on the surface.

The guest applies the theme with the same two calls the shell uses,
`theme::reload_from` and `theme::apply_gpui_component_theme`, at mount and
on every change notification. On the host, `theme::live::apply_scheme` is
the single place the live scheme changes (`Reload Config`, the theme
settings pane); it also publishes the new theme to open preview panes.
The guest platform reports a fixed dark window appearance, which does not
matter here because Horizon's colors never come from the OS appearance.

## The pane

`ViewKind::Preview` is a session-less view kind like Board. The workspace
model carries only the kind; the artifact path and preview name live in
the shell (`WorkspaceShell::preview_targets`, keyed by pane id), so
nothing path-shaped enters the persisted schema. After a UI restart a
preview pane is restored empty; an empty preview pane under the cursor
takes the next `horizon preview`.

The pane owns a `Surface`, a `PluginHost`, a status line (loading, loaded,
or the load error), and a `notify` watcher on the artifact's directory.
Only events that can change the artifact's content count (create, remove,
data or name modification): inotify also reports opens and closes of a
file that is merely read, and a load reads the artifact, so reacting to
access events would make every load schedule the next one. Change events
are debounced by 300 ms because cargo unlinks and re-links the artifact.
A reload drops the old host and loads the new one; a load that fails
shows its error and keeps watching. The plugin gets no WASI grants beyond
the defaults, a 5 s per-turn budget, and a 512 MiB memory limit.

The platform text system is published once at startup as the
`PreviewTextSystem` global. `entry::build_application` constructs the
platform with `gpui_platform::current_platform(false)` and
`Application::with_platform` — what `gpui_platform::application()` does
internally — because gpui exposes the platform text system only on the
`Platform` object.

`embedded_gpui::Surface` keeps its focus handle private, so the pane owns
its own focus handle and forwards key events to the guest while that
handle is focused. The pane declares no key context, and the
workspace-mode chord is bound without a context and handled on the shell
root. That the chord still works while a preview pane has focus is not
covered by a test; the shell has no window-level test harness.

## Commands

```
horizon preview <path-to-wasm> [--name <preview>] [--split [<session-id>]] [--active]
```

Without `--active` the pane does not take focus. A path that already has a
pane reloads that pane; otherwise an empty preview pane under the cursor
takes it; otherwise a new tab or split opens. The path is made absolute
against the caller's working directory in the CLI. `--name` defaults to
the sample preview. The request travels as the control plane's existing
`invoke` message.

`Reload Preview` (keybinding id `reload-preview`, no default chord)
reloads the active preview pane.

An agent's own checks are windowless. Opening a preview in the running
Horizon is how an agent asks a person to look at a view, so the CLI is the
primary entry point and the view chooser does not list the preview kind.

## Checks

- `scripts/check-preview-wasm.sh` — the library builds for the plugin
  target. Seconds when warm; part of the quality gate.
- `scripts/check-preview-plugin.sh` — builds the plugin with the `quick`
  profile and runs the `#[ignore]`d windowless tests through real
  wasmtime. The sample's: it paints its label, a host theme change changes
  what is painted, a reload from the same path re-attaches to the same
  surface with exactly one live instance, and a broken artifact fails the
  load without disturbing the pane. The board's: `board-list` paints row
  titles from its sample store and the rows' activity icons reach the host
  as images, `board-list-empty` paints its chrome and no row, and
  `board-detail` paints the item's body and its comment thread. A board
  preview's surface is tall, because gpui culls primitives outside the
  content mask and an assertion on text that scrolled out of view is an
  assertion on nothing. Minutes when cold; not part of the gate.

## Costs measured at introduction

One run each on a loaded desktop; order of magnitude only.

| | before | after |
| --- | --- | --- |
| cold `cargo build --workspace` | 764 s | 799 s |
| `target/debug/horizon` | 482 MB | 595 MB |
| root `Cargo.lock` packages | 1084 | 1176 |

Plugin, `quick` profile (release-derived, `opt-level = 2`, no LTO): cold
build 131 s; rebuild after an edit in `src/preview/sample.rs` about 4 s;
component about 14 MiB; load to first frame about 1.4 s. Load time is
almost entirely wasmtime compiling the component and tracks its size, so a
debug component loads several times slower.

## Not forwarded to a guest

The fork's host/guest interface carries resize, mouse, key, and cursor
style. It does not carry IME composition, clipboard, modifier-key state,
file drops, or native prompts. Latin text entry works; CJK composition
does not reach a guest. Upstream lists IME as open work in its `TODO.md`;
the others are not mentioned there.

A font crosses the boundary as family, weight, and italic only; a
fallback chain does not. The host resolves an unknown family by trying
`.SystemUIFont` and `Helvetica` and then falls back to font id 0, the
first font the host text system resolved. A preview that sets no family
lands there; when checked on a virtual display that was the shell's UI
font, so the sample matched the chrome, but nothing guarantees the order.
Glyphs missing from the primary font are left to the host text system's
own fallback, not to the configured chain.

`PluginInstance::new` in the fork starts its epoch-ticker thread before it
compiles the component, and the thread is not stopped when compilation
fails, so each failed load leaves one idle thread behind until the app
exits.
