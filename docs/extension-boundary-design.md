# Extension boundary

Status: under consideration. Nothing here is decided; the last section
lists what is.

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
(`spikes/plugin-boundary/`):

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

Nothing yet.
