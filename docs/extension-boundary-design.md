# Extension boundary

Status: the shape of the boundary is decided (the last section, and board
#76); it is not built. The sections before it are the comparison it was
decided from.

A view is a function that builds elements once and follows signals
afterwards (`crates/horizon-ui`: `El`, `Page`). An extension is such a view
in a WebAssembly component, shown in a pane beside the built-in ones. This
document sets out how a view in a guest can run and what can cross between
it and Horizon, to choose one.

What is wanted of the boundary (board #75's follow-up, 2026-11-13):

- Horizon's own views, such as the Markdown view, can move out of the
  binary into extensions.
- Other kinds of view can be added the same way.
- A guest builds any view. What a view can express, and how its code is
  written, do not depend on its being in a plugin.

## Three models

**1. The guest has the signals and the handlers; the document is the
host's.** Each thing a view does to an element is a call to the host
(`crates/horizon-ui/wit/plugin.wit`), which does it in its own document.
The host styles, lays out, paints, takes input and decides where an event
goes, and calls the guest when an event reaches a node the guest listens
at, when effects are to run, and after a layout. This is what the preview
pane does today.

**2. As 1, with the guest's changes gathered and handed over together.**
The guest keeps a list of what it did since it was last called and gives
it to the host in one call.

**3. The guest has the document.** The guest runs the same `horizon-ui` a
built-in view runs on, over a `blitz-dom` document of its own: it styles,
lays out and turns the page into drawing commands itself. What crosses is
input going in, and each frame's commands coming out, which the host draws
in the pane's box.

## The same seven things in each

| | 1: per call | 2: gathered | 3: document in the guest |
| --- | --- | --- | --- |
| A list of 1,000 rows is built | 55,049 calls to the host. Measured: 16–27 ms in the guest, the document's work included, where building the same rows natively is about 2 ms | One call carrying the same changes. The guest has to name nodes before the host has made them. Not measured | Nothing crosses. Built in the guest in 2–3 ms |
| Japanese is typed into a field | The field is the host's: the input method, the caret and editing are `blitz-dom`'s in the host, and the guest hears `input` with the new text | As 1 | The field is the guest's. The host hands on the input method's events, and has to be told where the caret is to place the candidates |
| A list follows its end by reading the layout | After a layout the host calls the guest, which asks for boxes one call at a time, scrolls, and asks for another layout | Reads cannot be gathered: what was gathered has to be handed over before a read | All in the guest, as natively. `windowed` works as it is |
| A file changes and the view shows it | The host tells the guest; a few calls change the document | As 1 | The host tells the guest; the guest lays out and hands over a new frame. The guest has to be able to ask for a frame |
| The view paints itself (a terminal) | Not there. A way to hand drawing commands to a node would be added, and the guest would shape its text itself | As 1 | The paint widget is the view's own code. Its commands are part of the frame |
| A row is dragged and what is under the pointer is asked | The handler says it has questions; the host calls back after the dispatch; the guest asks | As 1 | In the guest |
| Work off the UI thread comes back | The same in all three: see "Who drives" | | |

## By what is wanted

**The same code.** In 1 and 2 everything a view can do has to be in the
WIT, in the guest's implementation of `El`/`Page` and in the host's: three
places for each thing. What is missing today: a paint widget, text for an
element that is not a field, pastes and the clipboard, the page's
selection and where text is, `windowed`, wheel and input-method events. In
3 the guest compiles the implementation the built-in views use, and there
is nothing to mirror; what differs is at the edges (fonts, the clipboard,
opening a URL, the input method's window).

**Speed.** Measured on this machine, release builds, wasmtime 50.0.1
(`spikes/plugin-boundary/` on the branch `plugin-wasi3-spike`):

| 1,000 rows | native | in a wasm guest |
| --- | --- | --- |
| building the rows | 2.3 ms | 2.9 ms |
| the first layout | 53 ms | 34 ms (other fonts are found; the same order) |
| layout after one row changes | 0.44 ms | 0.16 ms |
| the frame's commands | 0.27 ms | 0.20–0.25 ms |

A frame of that list is 164 commands, 670 glyphs, about 26 KB as data
when a font is named and not carried. A full terminal of 200 by 60 would
be some 12,000 glyphs, about 150 KB.

For 1, the 55,049 calls took 16–21 ms on wasmtime 36 and 20–27 ms on 50:
about 0.4 µs a call with the document's work. A step of a selection is 5
calls and about 25 µs, against 7 µs natively.

**Size and start.** The preview plugin (model 1) is 0.8 MB. A guest with
`blitz-dom`, Stylo, Taffy and Parley in it is 13.6 MB, and wasmtime
compiles it in 1.1 s; a compiled component can be kept between runs. A
guest in 3 loads fonts of its own: the CJK font alone is 19 MB of each
guest's memory.

**Isolation.** The limits on memory and on a call's time are the same in
all three. In 1 and 2 the guest's style sheet goes into the host's
document as it is, and its rules apply to the whole page: a plugin's CSS
can restyle the shell. In 3 nothing of the guest's is in the host's
document.

**One shell.** In 1 and 2 there is one document: the theme's variables,
the focus, the page's selection, the input method and what a screen reader
would be told are the host's, and work across panes as they do now. In 3 a
pane is a picture: the theme is handed over as CSS text, the host tells the
guest when it has the keys, a selection does not run from a built-in pane
into an extension's, and an accessibility tree would have to be passed
across.

**The host's work, and what it leans on.** In 1 and 2 the host implements
every operation (`src/preview.rs`), and grows with what a view can do. In
3 the host draws a frame's commands, hands on input and sizes, and lends
fonts; it leans on `anyrender`'s recorded commands staying data (its
`serde` feature) and on `blitz-dom` continuing to build for
`wasm32-wasip2`, which it does at the pinned revision and which upstream
does not promise.

**How the boundary ages.** What crosses in 1 and 2 has counterparts in
standards that have not moved in decades: making elements and setting
attributes (DOM), CSS as text, events (UI Events), asking where a box is
(`getBoundingClientRect`, `elementFromPoint`, scroll offsets). An
extension built against a subset of those keeps working while the subset
means what it meant, and the boundary grows by adding. `plugin.wit` has
that shape already, with names and details of its own. In 3 what crosses
is `anyrender`'s vocabulary, a crate before 1.0, with glyphs numbered by
one font file; and each extension carries a Blitz of its own version,
rebuilt to take a fix.

## A view that paints itself, by a standard

No format for vector drawing commands is a standard: Skia's pictures,
Vello's scene encoding and `anyrender`'s recording are each their
implementation's. What standards have is a vocabulary for painting a box
from isolated code:

| | What crosses | State (2026-11) | Here |
| --- | --- | --- | --- |
| Canvas 2D (HTML) | state, transforms, paths, fill and stroke styles, rectangles, clips, images, shadows, compositing, text as a string and a font's name | long stable | all but text maps onto `anyrender`'s commands one to one; text is shaped by the host |
| CSS Painting API (Houdini) | a subset of Canvas 2D, called on to paint an element's box from a worklet | Editor's Draft, 2026-02-25 | the precedent for this use; it leaves text and image data out |
| SVG | shapes and text as elements | stable | crosses as elements already; too heavy for a surface repainted every frame |
| WebGPU (`wasi:webgpu`) | GPU commands; the guest brings a renderer | W3C CR; the WASI proposal is moving to 0.3 and to conformance | not in Wasmtime itself |
| pixels (`wasi-gfx:frame-buffer`) | what the guest rasterized | moved out of WASI's namespace in 2026-06, to be versioned like a library | the guest carries text rendering and fonts |

Blitz's `<canvas>` at the pinned revision is a box for a paint source the
host registers; it has no 2D context. Taking Canvas 2D's commands and
drawing them would be the host's own code.

With text as a string and a font's name, no glyph or font file is at the
boundary. Not known: what shaping a terminal's screenful of text in the
host costs each frame, and what it takes for a built-in view to paint
through the same calls, so that its code is the same in a plugin.

## Who drives

Today the guest runs only while the host calls it: it has no thread and no
timer, and work it starts "off the UI thread" runs in place
(`Background::run`).

WASI 0.3 (ratified 2026-06-11; Wasmtime 46 and later) gives components
`async` functions, `stream` and `future`: a guest can have tasks of its
own that wait for a file to change, a timer, or work in the host, with the
host running them from its own loop (`Store::run_concurrent`). Rust guests
build for `wasm32-wasip2` with `wit-bindgen`'s `async` feature. Whether
that loop can be run from the UI thread's, a frame at a time, has not been
tried. This question is the same for all three models.

## Not yet known

- For 3: how fonts reach a guest and stay the same as the shell's; the
  form of a frame's commands with fonts and images named (the command
  types take them as parameters; nothing converts them yet); the input
  method across the boundary.
- For 2: what handing changes over together saves, measured.
- For all: running a guest's tasks from the UI thread (above); keeping a
  compiled component between runs.

## Decided

The decisions and who made them are on board #76. What they come to:

- **The layer.** The boundary carries operations on the document (model
  1): the guest has the signals and the handlers, and the document is the
  host's. What crosses is named and means what the DOM, CSS and event
  standards say, and types are written in WIT the way `webidl2wit` writes
  WebIDL's. No existing protocol is taken whole: `browser.wit` (the DOM
  converted mechanically) has nothing written against it yet, wasm-bindgen's
  convention is JavaScript glue, and remote-dom is JavaScript's and has no
  way to read the layout.
- **One operation, one call.** A read between writes is then answered from
  what was written. For making many of the same thing, a template is made
  once and cloned (the DOM's `<template>` and `cloneNode`). Handing changes
  over together is looked at again if a case is measured to need it.
- **Nodes are WIT resources.** The runtime knows whose a node is and when
  it is let go. A view copies its `El` freely, so the guest's `El` stays a
  number that names a resource the guest holds.
- **The host delivers events.** It decides where an event goes and calls
  the guest's handler, which can stop the default or the propagation
  there and then. `browser.wit` has the guest subscribe and wait; this
  departs from it on purpose.
- **The document is synchronous; the world outside may not be.**
  Operations on the document and questions about the layout are plain
  calls. Waiting for a file to change, a timer, or work off the UI thread
  is to be WASI 0.3's `async`, if a guest's tasks can be run from the UI
  thread's loop, which is to be tried first. If they cannot, the host
  calls the guest to tell it, as it does today.
- **wasmtime 50.** Done.
- **The first extension is the Markdown view**, taking the built-in one's
  place once it passes the same tests. It reads files through WASI, with
  `/` lent read-only. Extensions are named in `config.toml`.
- **A view that paints itself comes later**, through a subset of Canvas
  2D if at all.

Open: an extension's life and Horizon's own calls (loading, a pane's state
kept and given back, what the shell asks of a view, watching a file,
opening a URL); keeping a plugin's CSS to its own tree; how the boundary
is versioned; where extensions live and how they are built; the preview
pane as one of them.
