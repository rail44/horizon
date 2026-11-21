# Terminal Row Textures

Status: designed 2026-11-22; being implemented on the branch
`worktree-terminal-row-texture-design`.

The terminal view keeps each row it has drawn as pixels in a texture of its
own, and a frame that does not change the terminal's contents costs one image
fill per visible row instead of a full repaint of the grid.

## 1. What prompted this

On 2026-11-22 the shell's UI thread used 60–70 % of a core on the owner's
machine with nothing being typed (debug build, three terminals visible in a
split, the session list pane beside them). Sampled with gdb:

- `blitz_dom::BaseDocument::is_animating()` returned true on every frame,
  and of its terms only `has_active_animations` was set. The one CSS
  animation in the document was the session list's turning icon for a session
  at work (`plugins/sessions/src/view.rs`, `.turning`). So `redraw` in
  `src/application.rs` asked for the next frame at every display refresh.
- Every one of those frames repainted every visible terminal from nothing.
  Of the stack samples that were on the CPU (rather than waiting on the GPU
  fence), about two thirds were in `horizon::terminal::glyphs`: turning
  parley clusters into glyph runs for every row. Most of the rest was in
  Vello's encoding of the whole scene.

The animation is only the trigger. Anything that redraws the window without
changing a terminal pays for the terminal anyway: an update in another pane,
a pointer moving over the chrome, any future animation.

## 2. What the layers below offer

No layer between the view and the GPU keeps anything drawn from one frame to
the next (Blitz at the revision pinned in the root `Cargo.toml`, anyrender
0.15, Vello 0.11, wgpu 30):

- `blitz_paint::paint_scene` walks the whole document every frame. It calls
  every custom widget's `paint()` and appends a clone of the returned scene
  (`blitz-paint/src/render.rs`, `draw_custom_widget`).
- blitz-dom's paint tree (`blitz-dom/src/layout/paint_tree.rs`) caches which
  nodes paint in which order, and the positions of hoisted boxes per geometry
  generation. It does not keep commands or pixels.
- `anyrender::Scene` is a plain list of commands. Vello encodes, resolves and
  rasterizes the whole scene on every render. It has no damage regions.

What survives frames is a texture registered through the custom widget's
resource path. Blitz's own `examples/wgpu_texture` uses this path:

- `RenderContext::renderer_specific_context()` downcasts to
  `wgpu_context::DeviceHandle` (the device and queue) under
  `anyrender_vello`'s window renderer.
- `RenderContext::try_register_custom_resource(Box<wgpu::Texture>)` returns
  a `ResourceId`.
- A fill with `Paint::Resource(ImageBrush { image: id, .. })` draws the
  texture.

## 3. Rows

### 3.1 A row's height is a whole number of display pixels

`row_px = round(line_height × scale)`, where `line_height` is the terminal's
CSS line height (`crates/horizon-theme-css`, `TerminalLook::line_height`) and
`scale` is the display's. Every use of the row height in `src/terminal/` goes
through `row_px`:

- the number of rows that fit;
- a pointer position turned into a row, and the wheel's rows;
- the IME cursor area;
- the baseline (`Shaper::baseline`);
- painting.

`horizon-ui` already rounds the layout to display pixels
(`crates/horizon-ui/src/display_scale.rs`), so the grid's top is on a pixel.
With `row_px` whole, every row boundary is on one too.

At scale 1.5 and the default 18 CSS px, `row_px` is 27, which is what the
owner's machine shows today. At 1.25 it is 22 or 23 (18 × 1.25 = 22.5). Today
such a row's height alternates between 22 and 23 display pixels; with `row_px`
every row is the same, and its height in CSS pixels differs slightly from 18.

Columns are not affected: a row's pixels span the grid's whole width.

### 3.2 What a row's pixels depend on

A row's pixels are a function of:

1. its content (`TerminalLine`: text, cell columns, characters drawn as
   shapes, styled runs);
2. the part of the selection on that row;
3. what applies to the whole grid: the font, the program's palette overrides,
   the theme's terminal look, `scale`, and the grid's width in pixels.

The cursor is not among them: it is drawn over the rows (§4.2). Neither is
the row's position: positions are whole pixels (§3.1).

### 3.3 The row store

The store keys rows by (1) and (2): a hash looks a row up, and a hit is
checked against the stored `TerminalLine`. A change to anything in (3) empties
the store.

- The key is computed in the shell from what it decoded. Under the snapshot
  wire (`docs/remoc-adoption-design.md` §5), the store holds nothing that the
  daemon also believes the shell holds: a row decoded the same way draws the
  same pixels, whether drawn now or reused.
- Live rows and rows of a scrollback window (`TerminalScrollWindow`) are keyed
  the same way. A row that scrolled into history and is scrolled back to is
  still found. Rows with the same content (blank rows) share one slot.
- Hashing runs when a new frame or scrollback window arrives, not on a redraw
  that changes neither. Its cost is of the same order as the row equality
  checks `Screen::paint` already does to reuse shaping.
- libghostty-vt's row ids are not used. They do not cross the snapshot wire.
  They vanish when a program scrolls part of the screen. Scrollback windows
  carry none.

### 3.4 Slots

The store's pixels are one texture of fixed-size slots, each `row_px` tall and
the grid's width wide. It holds at least the visible rows plus one (the part
row of a fractional scroll), with a margin for rows just scrolled away. A slot
not used in the current frame can be taken, least recently used first. The
texture is made again when the grid's width, `row_px` or `scale` changes. When
slots would exceed `max_texture_dimension_2d` in one column, they are arranged
in several.

### 3.5 Drawing rows into slots

The rows of a frame that are not in the store are drawn together:

1. Each row is painted opaque, its background first. It is painted by the
   function that paints one row today (backgrounds, selection, glyph runs,
   shapes, rules), into a `vello::Scene` through
   `anyrender_vello::VelloScenePainter::new`, the rows stacked.
2. One `vello::Renderer::render_to_texture` draws that scene into a scratch
   texture.
3. `copy_texture_to_texture` moves each row into its slot.

Rows cannot be drawn into the slot texture directly: `render_to_texture`
writes its whole target. They cannot be moved within one texture either:
wgpu rejects any copy between regions of the same mip level and layer
(`wgpu-core`, `validate_copy_within_same_texture`).

How rows are drawn:

- **Spacing.** In the scratch texture, rows are a whole number of Vello's
  16-pixel tiles apart. A row then lies the same way across the tiles
  wherever it is drawn, and comes out the same pixels. At 23 pixels apart,
  a row drawn at another place differed by a shade at a few edges.
- **Anti-aliasing.** Rows are anti-aliased as the window's renderer draws the
  rest of the page: `anyrender_vello`'s default, MSAA 16, which
  `src/application.rs` keeps.
- **One Vello renderer.** The stores on one device share one Vello renderer,
  since making one compiles its pipelines. The stores hold it, and a
  thread-local only refers to it. What wgpu keeps is then let go of with
  the stores, never while the thread's locals are being destroyed.
- **Clipping.** Ink a glyph draws outside its row is clipped at the row's
  bounds.

The store goes with the renderer it was registered with. Blitz calls a
custom widget's `destroy_surfaces` before it takes another renderer
(`blitz-shell`'s suspend), and the store is dropped there. wgpu's device
equality compares ids that two wgpu instances can share, so it cannot tell
that the renderer changed.

A document holding a store has to be dropped while wgpu can still let go
of what the store holds. `horizon-ui`'s `App` leaves the thread's current
app when it is dropped, so its document goes with it.

## 4. A frame

### 4.1 What `paint()` returns

1. For each visible row, one fill:
   - the shape is the row's rectangle on the grid, intersected with the
     grid's box (rather than a clip layer);
   - the brush is `Paint::Resource(ImageBrush { image: store, .. })`;
   - the brush transform translates the row's slot to the row's place.

   Every coordinate is a whole display pixel, so the texture is sampled one
   to one.
2. The cursor, drawn as today: a translucent block, an outline, or a bar.
3. The IME preedit while one is open, drawn as today.

### 4.2 Over the rows, and in them

- The cursor is drawn over the rows, so moving it or blinking it changes no
  row.
- The selection is painted under the text, so it is part of the rows it
  touches. A selection change redraws those rows.

### 4.3 Scrolling

- **Output that scrolls the screen.** The rows that remain keep their slots,
  and only the rows that are new are drawn. `paint()` places the same slots
  one row higher.
- **A program scrolling part of the screen.** Handled the same way, since
  rows are found by content.
- **Scrolling back through history.** The rows are drawn shifted by
  `round(fractional_row × row_px)` display pixels:
  - The retained scroll position stays exact, as
    `docs/terminal-scrollback-design.md` has it, and the shift still moves
    by less than a row. Only its drawn amount is rounded, by under one
    display pixel.
  - The rows at the top and bottom edges are clipped by the rectangle
    intersection.

## 5. Without a GPU device

`paint()` sees whether `renderer_specific_context()` yields a `DeviceHandle`.
Without one, it draws every row at its place with the one-row function and
keeps no slots. The CPU renderer the windowless tests paint with is such a
renderer.

## 6. Tests

What the renderers available to a test can do:

- **`anyrender_vello::VelloImageRenderer`**, the GPU offscreen renderer:
  - Each `render` builds the resource-resolving `VelloScenePainter` (with its
    Vello renderer, device handle and texture table) and paints into it.
  - `blitz_paint::paint_scene` passes the scene it paints into to every
    custom widget as `render_ctx` (`PaintScene: RenderContext`).
  - So a widget painted through it receives the `DeviceHandle`, registers
    textures, and has its `Paint::Resource` fills drawn: the path the window
    takes.
- **`anyrender_vello_cpu::VelloCpuImageRenderer`** (0.18): its painter keeps
  the default `RenderContext`. It lends no device, and `Paint::Resource`
  paints transparent.

`VelloImageRenderer` makes its device through `wgpu_context::WGPUContext`,
which takes whichever adapter wgpu offers first.

Measured 2026-11-22 painting a terminal of 60 lines, 900×540:

| adapter | renderer made | first frame | later frames |
| --- | --- | --- | --- |
| Radeon 890M (radv) | 0.26 s | 0.37 s | 4–12 ms |
| llvmpipe (lavapipe; `VK_ICD_FILENAMES` naming its ICD alone) | 0.24 s | 2.4 s | 3–4 ms |

The two drew the same text pixels. So a machine with either a GPU's Vulkan
driver or Mesa's lavapipe (`mesa-vulkan-lavapipe` on Void) runs these tests.
Without any Vulkan device, `WGPUContext` finds no adapter and the renderer
panics.

The tests that paint a terminal live in binaries that open a real PTY
(`horizon::terminal`, `horizon::workspace`). These are outside the
`sandboxed` nextest profile already, so a sandboxed session never needs a
device for them.

What the tests are:

- **GPU path.** The terminal's pixel tests of what it draws paint through
  `VelloImageRenderer`: the blocks with no seam, in one view, in a view
  off whole pixels, and in both panes of a split; the lines under and
  through text. `Terminal::painted_from_kept_rows` says whether a paint
  came from the store.
- **No device.** `the_screen_is_painted_and_not_only_held` paints through
  `VelloCpuImageRenderer`, and the screen is drawn whole.
- **Unit tests.** The store's logic has unit tests without any renderer:
  row height, the quantized scroll offset, keys, slot assignment and
  reuse, and the slots' layout.
- **Pixel comparisons.** The cases are:
  - output that scrolls;
  - a partial-screen scroll;
  - a fractional scroll back through history;
  - a selection change;
  - scales 1.25 and 1.5.

  How each case runs:

  1. The screen before the change is painted through the store.
  2. The screen after the change is painted through the same store, so
     rows are reused.
  3. The document's `destroy_surfaces` is called and a new renderer paints
     the same screen, every row drawn afresh.
  4. The two paints are compared.

  The comparison is between paints through the store, not against the
  screen drawn whole, for two reasons:

  - The offscreen renderer anti-aliases with `Area` and the rows with
    MSAA 16, so text drawn whole and text from the store differ.
  - Two renderers can differ by a shade at a few glyph edges: with the
    glyphs' hinting off the two paints are the same pixels. So a pixel may
    differ by up to 32 in a channel, and fewer than 50 may differ at all.
    A row in the wrong slot differs by a whole row's text.

## 7. Measuring

`cargo bench --bench scenarios -- terminal/` (`docs/performance-scenarios.md`)
before and after, in a debug and a release build, comparing the `scene`
column.

Add a scenario for the case in §1: frames that change nothing in a visible
terminal. `--window` (the `render` column, GPU included) opens a real window
on the desktop and is run only with the owner's consent each time.

## 8. Scope

This covers the terminal view.

It does not change how the redraw itself is triggered: an animation anywhere
still makes every frame lay out and encode the whole document, whose
non-terminal part is not covered here. `round_to_device_pixels`
(`crates/horizon-ui/src/display_scale.rs`) starts a new geometry generation
every frame, which also discards the paint tree's position cache every frame.
That cost is not measured.

## References

- `docs/remoc-adoption-design.md` §5 — the snapshot-valued frame wire.
- `docs/terminal-protocol-goals.md` — the terminal frame path's goals.
- `docs/terminal-row-metrics-design.md` — one baseline per row (written for
  the GPUI shell; the principle carries over).
- Blitz `examples/wgpu_texture/src/demo_renderer.rs` — a custom widget
  drawing into its own registered texture.
