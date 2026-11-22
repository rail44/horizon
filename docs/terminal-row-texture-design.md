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

- The key is what the row's pixels are drawn from, so a row found by it
  draws the same pixels whether drawn now or reused.
- Wherever a row moved, it is found: output scrolling it up, scrolling back
  moving it down (`docs/terminal-scrollback-design.md`), a program
  scrolling part of the screen. Rows with the same content (blank rows)
  share one slot.
- **Hashing.** A row's hash is taken once, when the row is shaped. A frame
  combines it with the row's selection.
- **The hash function.** foldhash's fast variant, seeded at random once for
  the process. Hashes are compared only within the process. A hit is
  checked against the slot, so a collision costs a comparison and draws
  nothing wrong, and the random seed keeps a program from aiming for
  collisions.
- **Matching.** Each shaping has a number of its own, kept with the slot.
  A row shaped once and shown again is matched by that number, not by
  comparing its whole content. A row found only by its hash is compared
  with what the slot holds.
- libghostty-vt's row ids are not the key. An id stays with a row whose
  content is written over in place, and a program scrolling part of the
  screen gives the rows it moves new ids
  (`engine_observations.rs::what_the_engine_says_of_rows_as_output_scrolls`).
  The view's shaped rows are found the same way, by their line's hash
  (`Screen::shape_rows`).

### 3.4 Slots

The store's pixels are one texture of fixed-size slots, each `row_px` tall and
the grid's width wide. It holds the visible rows, plus one (the part row of a
fractional scroll), plus a quarter of a screen for rows just scrolled away and
back. Every visible terminal's store has to fit in the renderer's one image
atlas (§7). A slot
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

1. For each visible row that is not blank, one fill. A blank row draws
   nothing but the ground: no background of its own, no ink, no shapes, no
   lines, no selection. It is left to the ground the screen is filled with,
   and takes no slot. The fill:
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

### 4.4 Rows new to a frame

Drawing rows into the store is a Vello render of its own, and it costs about
the same for one row as for three. So a frame does not draw its new rows
into the store when they are few. It paints them into its own scene with the
one-row function, beside the fills of the rows the store holds, and they
take no slot.

- **How few.** The new rows are painted into the frame while they have at
  most 1024 cells with something drawn in them (`PAINTED_IN_THE_FRAME` in
  `src/terminal/store.rs`). A row painted into the frame is drawn again
  each time the page is, until it is put in the store, so a frame does so
  only while that costs less than one render of the store. Measured in a
  window at 48000 cells, the render took about 1 ms, and rows of text took
  about as long at a little over 1000 cells.
- **When they are put in the store.** The next time the page is drawn with
  nothing about the screen changed, the screen does not reuse its last
  scene. It draws the rows painted into the frame into the store, and
  composes every row from the store. Without anything else drawing the page
  again, that time never comes, and the scene with them painted in stays
  as it is.
- **While the screen keeps changing.** A row painted into one frame and
  still shown in the next is new to that one too: it is counted again,
  with the rows new to that frame. Once they are over the limit, all of
  them are drawn into the store, as before.
- **The same pixels.** The rows are painted with the same function either
  way, and the store anti-aliases as the window does (16 samples a pixel),
  so a row looks the same painted into the frame and from the store.

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

The recording scene the bench paints into by default lends no device, so
its `scene` column times the screen drawn whole either way. `--gpu` paints
each frame with `VelloImageRenderer` and reads it back, so the store is
used. `terminal/idle` times frames that change nothing in a full screen:
the case in §1. `--window` (a real window on the desktop) is run only with
the owner's consent each time.

### Measured 2026-11-22

Debug build, `--gpu` on the machine's own GPU (radv), the median in
milliseconds.

**On the CPU.** `paint_scene`, the commands built for a frame, at 48000
cells:

| scenario | before | after |
| --- | --- | --- |
| idle, a full screen | 25.3 | 0.2 |
| typing, an almost empty screen | 0.4 | 1.3 |

**The whole frame.** This includes Vello's encoding, the GPU and the read
back, and could not be measured to a few milliseconds here, for two reasons:

- The GPU is shared with the desktop, where a running Horizon draws every
  frame.
- The machine was low on memory (its swap full), and its CPU slowed as it
  heated.

How the measurements went:

- **Separate runs.** Runs of the before and after binaries minutes apart
  differed by more than any effect: the same before binary took 43, 64 and
  69 ms a typing frame at 48000 cells.
- **Alternated.** To compare under the same conditions, temporary code
  switched the way of drawing every frame within one run:
  - the rows from the store;
  - the store's strips filled with one color;
  - no strips at all;
  - the screen drawn whole, with and without a clip per row.
- **What the alternated runs showed.**
  - A full screen (`lines`) was faster from the store: 100 → 91 at 48000
    cells, 28.4 → 24.5 at 12000.
  - On an almost empty screen (`typing`) the order changed from one run to
    the next: from the store 7 ms slower in one, 22 ms faster in another.
  - The store, one-color strips and no strips came out the same within the
    noise. So copying the store into Vello's atlas, which `anyrender_vello`
    asks for every frame, and filling from it, are not a cost these runs
    can see.

### Measured in a window, 2026-11-22

Debug build, `--window` (run with the owner's consent), the before and
after binaries in turn, twice each. `render` is the median in milliseconds,
for the scene, the GPU's work and the presentation. The display refreshes
every 16.67 ms.

| scenario | 1920 cells | 12000 cells | 48000 cells |
| --- | --- | --- | --- |
| idle | 2.8–3.2 → 1.2–1.3 | 9.5–9.8 → 2.3–3.9 | 35–36 → 5.9–7.3 |
| lines | 2.4–2.5 → 2.4–2.7 | 11.3–11.9 → 3.7–3.9 | 37.6–39.4 → 13.2–13.7 |
| typing | 1.3–2.2 → 2.5–2.6 | 1.3–1.5 → 2.9–3.3 | 2.4–3.6 → 6.2–6.9 |
| scrollback | 1.4–1.6 → 2.4–3.0 | 2.3 → 3.1–3.5 | 3.9–4.0 → 7.3–7.4 |

What changed:

- **idle.** At 48000 cells every frame was over a refresh before; none was
  after.
- **lines.** Twice as many of the output's frames were taken in (42 → 82
  at 48000 cells).
- **scrollback at 48000 cells.** The wheel's own handling takes 9–11 ms.
  With `render` up by 3.4 ms, the frames over a refresh went from 5–11 of
  200 to 100.

#### Where typing's and scrollback's added cost goes

Temporary code switched the way of drawing every frame within one window
run. The ways were:

- the screen drawn whole without a clip per row, as before;
- the rows from the store;
- the store without drawing new rows;
- the store's strips filled with one color.

Typing at 48000 cells:

| way | median ms |
| --- | --- |
| drawn whole | 4.1 |
| the store's last scene reused | 5.1 |
| the store, no new rows | 6.3 |
| the store | 6.6 |

At 12000 cells the same four were 2.2, 2.3, 2.9 and 3.4.

- **Drawing the new rows** (one when typing, three per wheel step) costs
  0.3–0.5 ms. It is not where the cost goes.
- **Filling the grid from the store image on the GPU** costs about 1 ms at
  48000 cells over drawing an almost empty screen whole. This is reading
  from Vello's image atlas across the grid, after the copy of the whole
  store into the atlas that `anyrender_vello` asks for every frame.
- **The per-frame work on the CPU** in a debug build makes up the rest:
  hashing and placing the rows, and the composition.

Two things found on the way:

- **Vello's image atlas is rebuilt after a frame without images.** In a
  frame with no image, Vello's resolver reports an atlas of no size, and
  `vello/src/render.rs` replaces the atlas it keeps with a 1×1 one. The
  next frame that uses the store allocates the atlas again and copies the
  whole store into it. Alternating the store with frames drawn without it
  took 26 ms a frame at 48000 cells.
- **The store lives in Vello's atlas, which is at most 8192 × 8192.** A
  store is twice the grid's rows, and a 48000-cell grid's store is 3200 ×
  4356. Every visible terminal's store has to fit in the one atlas. An
  image that does not fit is not drawn.

#### What followed

Three changes followed these measurements:

- a blank row is left to the ground and takes no slot (§4.1);
- the store holds the visible rows and a quarter of a screen, not twice the
  rows (§3.4);
- a row's hash is taken when it is shaped, with foldhash, and a row shown
  again is matched by its shaping (§3.3).

Measured again in a window, three binaries in turn, twice each: before the
store, the store before these changes, and the store with them. `render` is
the median in milliseconds, and *late* counts frames over a refresh.

| scenario | before | the store | with the changes |
| --- | --- | --- | --- |
| idle, 48000 cells | 39.3–39.9, all late | 5.8–6.3 | 5.4–5.7 |
| typing, 12000 cells | 1.5–1.6 | 3.0 | 2.5–2.9 |
| typing, 48000 cells | 3.1–3.5 | 6.5–6.6 | 4.2–4.3 |
| lines, 48000 cells | 38.9–39.0, 39–43 frames taken in | 13.1–13.3, 76–80 | 7.9, 98–100 |
| scrollback, 12000 cells | 1.8–2.2 | 3.1 | 2.9 |
| scrollback, 48000 cells | 3.9, 3–4 late of 200 | 7.3–7.4, 100 late | 6.5–6.6, 100 late |

**Open.** Typing on an almost empty screen is still about 1 ms over before,
and scrollback at 48000 cells about 2.6 ms. In scrollback the rows are not
blank, so leaving blank rows to the ground does not help there. Its frames
are late because the wheel's own handling takes 9–11 ms at that size,
before and after alike, and the added `render` takes the frame past a
refresh. Where the 2.6 ms goes is not measured yet.

### Measured 2026-11-23

Temporary timers in `paint()` (offscreen GPU renderer, debug build, 48000
cells) split scrollback's frame:

- **Shaping every row again: 1.5 ms.** The view looked for a row it had
  shaped only at its own place and a few rows below, and stopped looking
  after two rows it did not find. Scrolling back moves rows down and brings
  new ones in at the top, so all 120 rows were shaped again for three new
  ones. The rows are now found by their line's hash wherever they moved
  (`Screen::shape_rows`), and shaping takes 0.17 ms.
- **Drawing the new rows into the store: 0.7–1.0 ms**, about the same for
  one row as for three. This is what led to §4.4.
- **Hashing, placing and composing: 0.15 ms.**

Window runs alternating ways of drawing within one run put the store's
remaining cost over drawing an almost empty screen whole at 0.5–0.8 ms.
That cost is filling the grid from the store image, after the copy of the
whole store into the atlas.

After §4.4, measured in a window, three binaries in turn, twice each:

- before the store;
- the store with the shaping fix;
- the store with §4.4 as well.

Medians in milliseconds:

| scenario | before | shaping fix | with §4.4 |
| --- | --- | --- | --- |
| typing, 12000 cells | 1.1 | 1.8–2.2 | 1.8 |
| typing, 48000 cells | 2.3–3.2 | 5.0–5.2 | 4.2–4.3 |
| scrollback, a wheel step, 12000 cells | | 2.6–2.7 | 2.2 |
| scrollback, a wheel step, 48000 cells | | 6.4–6.5 | 5.7–5.9 |
| scrollback, the page drawn again unchanged, 48000 cells | | 5.1–5.3 | 6.0–6.3 |
| lines, 48000 cells | 38.3–39.1 | 8.0 | 8.0–8.1 |

Notes on the table:

- **Scrollback.** In the scrollback scenario, a frame with nothing changed
  follows each wheel step. It is where the new rows are put in the store,
  so it costs about 1 ms more than reusing the last scene did. In a window
  where nothing else draws the page, that frame does not come. The
  scenario's median over both kinds of frame was 5.9–6.2 ms with §4.4 and
  6.0 with the shaping fix, against 3.7–4.2 before the store.
- **Lines.** Each frame brings two or three long rows, over §4.4's limit,
  so they are drawn into the store as before.

**Open.** A frame that paints the screen from the store is still about
1.5 ms over drawing a screen of short rows whole at 48000 cells. That
is the case of the shaping fix's frames with nothing changed (5.1–5.3 ms)
against before. The cost is filling from the store image, after
`anyrender_vello`'s copy of every registered texture into Vello's atlas each
frame. Neither is code of this repository's.

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
