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

Measured again 2026-11-16, release builds, a thousand rows of fifty-five
writes built three ways: natively through `El`, 13–15 ms, of which
12–14 ms is inside Blitz's document; in an extension, 25–29 ms, 14–17 ms
of it in the document; on Blitz's document directly, 12–20 ms, the same
whether one mutator is kept or one is made for each write. So the
document's own work is most of it either way, and what an extension adds
is 11–13 ms: the crossing (6.7 ms, below) and the guest's and the host's
code around it. (The "about 2 ms natively" above is not what building
these rows through `El` costs now.)

What handing the writes over together would save
(`spikes/plugin-boundary/guest-bulk`, `host-bulk`, with a document of
the host's own in place of Blitz's so that only the crossing is
compared): a call for each write, 6.7 ms; together as values of a WIT
type, the guest numbering the nodes, 4.6 ms; together as bytes, 2.1 ms.
At most 4.5 ms of an extension's 25–29, and tens of microseconds for an
ordinary update: the boundary stays one write, one call. A call into a
guest that does nothing is 0.3 µs as a plain export and 2.1 µs through
the long-lived future the shell calls an extension through.

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
build for `wasm32-wasip2` with `wit-bindgen`'s `async` feature.

Tried (`spikes/plugin-boundary/guest-async`, `host-tasks`; wasmtime
50.0.1, wit-bindgen 0.63): a guest's async export starts a task and comes
back; the task waits on a timer of the host's and calls the host between
waits. The host is one thread with no async runtime. When its waker is
woken it gives the store's tasks one turn
(`run_concurrent` around a future that is done on its second poll), and
between turns it calls the guest plainly.

- The task goes on across turns and ends; plain calls in between see what
  it has done.
- A task that waits one second asks for 2 turns in 250 frames of 4 ms,
  and once it has ended none are asked for: a guest that waits costs
  nothing.
- A wake must not be dropped. One that arrives during a plain call into
  the guest is for the tasks: every wake is answered with a turn.
- A store with an async import takes every call into the guest as an
  async one (`call_async`, on a fiber): about 1 µs a call.
- A plain call out of the guest costs 125 ns with a number, 219 ns with
  two short strings (the copies and the allocation are about 90 ns of
  that), 191 ns with a list of eight coming back.

That shape did not hold in Horizon. A plain call into the guest is a
future polled to its end, and the runtime wakes the waker it is polled
with to be polled on. The turns were a second future polled with the
same waker, so a wake meant either "poll this call on" or "a task wants
a turn", and nothing told the two apart: taken for a task's, a call's
wakes woke the page, whose effects call the plugin again. A window with
an extension in it was asked to draw without end and stopped answering
(2026-11-15). The spike had met it and stepped around it, clearing the
note of a wake after a plain call.

Tried instead (`spikes/plugin-boundary/host-driver`, the same versions):
one future per guest that goes on for as long as the guest does. It is
the store's `run_concurrent`, around a loop that takes the host's calls
from a queue and makes them through the accessor; it owns the store. The
guest's tasks go on whenever it is polled. A wake means one thing, "poll
it". A call's answer is waited for where the call is made, by polling
the future, and by parking the thread when the answer waits on another
thread.

- A task goes on when the future is polled, and once it has ended, or
  while it only waits, nothing asks for a poll.
- A call is answered there and then while a task waits: a hundred calls,
  none of which waited.
- A call makes its plain calls out to the host as before.
- A task that a plain call asks for is started and goes on, and so does
  a task woken by another task: neither needs a turn handed to it.
- A call that reads a file through WASI is answered. WASI's own
  functions hold the store while they wait (bytecodealliance/wasmtime
  #11869, #11870, both open), so the guest's tasks do not go on during
  that call; the thread is parked until the read wakes it.
- A call that does not come back is ended by the engine's epoch, the
  future ends with what the guest said, and what is asked of it
  afterwards is refused at once.
- A call into the guest costs 1.7 µs through the future, where the plain
  call cost 0.9 µs; a plain call out of the guest costs the same as
  before (86 ns with a number, 137 ns with two short strings, 241 ns
  with a list of eight coming back).

`src/plugin_host.rs` is that shape. Two things it adds:

- The thread that is polling a plugin polls it again before it leaves,
  so a wake that comes meanwhile does not wake the page; from another
  thread (a file changed), it does, and the page's effects poll.
- A plugin's effects run when the host calls for them
  (`events.run-effects`), which it does where the document may be
  changed, and nowhere else. The SDK runs the signals' effects on an
  executor of its own (`guest::Effects`): an effect whose signal was
  written waits, the host is told that some wait
  (`document.effects-wait`, once until it has run them), and the host
  runs them with the page's own. So whoever writes a signal, and
  wherever -- a handler, work after a layout, a task that waited on a
  file -- what it leads to is drawn at the same place, and a view never
  runs effects itself. (A task did: the watch reran the effects from
  the task, which goes on from wherever the host is. Reading a file is
  a WASI call the host goes on to something else during, and when the
  task went on in the host's call for the work after a layout, the
  document was not to be changed and the plugin trapped. Measured
  2026-11-16 under load, the extensions' three test files run together:
  a trap in two runs of twenty-five before, in none of forty after.)
- The host calls a plugin only for what it has: for its effects when it
  said some wait, and for its work after a layout while it has any
  (`document.layout-work`). A pane with nothing happening is not
  called.

One engine serves every pane, so a store's epoch callback ends its wasm
only once that store's own poll has run past the limit. `events.tasks`
comes back at once and leaves a task behind that starts the tasks a view
asks for, so they outlive the export's task.

`tests/extension.rs` holds a pane of an extension, and a preview pane of
its component, to having nothing to draw once nothing happens, as the
window would ask it.

Not tried: `stream`; a trap inside a task rather than a call.
`wit-bindgen`'s `spawn_local` needs the crate that spawns and the crate
that exports to be one version, and a spawned task lives as long as the
export's task that spawned it.

## Not yet known

- For 3: how fonts reach a guest and stay the same as the shell's; the
  form of a frame's commands with fonts and images named (the command
  types take them as parameters; nothing converts them yet); the input
  method across the boundary.
- For 2: what handing changes over together saves, measured.
- For all: keeping a compiled component between runs. Reading an
  extension is compiling it: measured 2026-11-16 for the three the shell
  comes with, 0.95 s for the components of a debug build (35 to 57 MB
  each) and 0.47 s for those of a release build (0.7 to 1.8 MB). The
  shell reads them once it is drawn, off the UI thread, so the window
  does not wait for it; a pane of one waits for it.

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
  calls. Waiting on the world outside is a component-model `async`
  function, and a guest's tasks are run from the UI thread's loop ("Who
  drives"): a plugin is one future that goes on for as long as it does,
  which makes this shell's calls into it and lets its tasks go on; a
  wake wakes the page, whose effects poll it (`src/plugin_host.rs`). The
  guest's tasks
  are started by `events.tasks`, which the SDK implements; a view starts
  one with `horizon_ui::guest::spawn`.
- **What the host offers beside the document is grouped by what it is
  about**, one interface each in `horizon:ui`: `desktop` (`open-url`),
  `pane` (`set-title`), `files` (the `file` and `working-directory`
  resources, and `watch`), `theme` (the seed the shell's theme is derived
  from: `current`, `apply`, which draws the theme of a seed and writes
  nothing, `save`, which writes one and is `async`, and `path`). A file's changes are watched by the host:
  `wasi:filesystem` 0.3 has no way to wait for one, and the host already
  watches files for its own views (`src/watch.rs`). A watch
  is a resource the plugin holds (`files.watch`): the file is watched
  for as long as it is held, and `changed` comes back for a change that
  came while it was waited in or since it last came back. (It was a
  function that watched for the length of one wait, and a change
  between two waits was lost.) A view
  watches one with `horizon_ui::guest::watch`, which reruns the plugin's
  effects after each change.
- **A view reads what is selected of its own text.** The selection is the
  page's, as the DOM's is the document's: `document.selection` gives, for
  each element of the plugin's whose own lines of text it covers part of,
  the bytes it covers, and `document.inline-text` that text. What is
  selected in another view's elements is not said. Setting the selection
  does not cross.
- **wasmtime 50.** Done.
- **The first extension is the Markdown view**, which has taken the
  built-in one's place: `markdown` is the extension, and the shell has no
  plugin of its own for it. The view's code is the extension's too
  (`plugins/markdown`): it is built for a guest only, over `horizon-ui`,
  `horizon-components` (what views are made of: the chips, the controls,
  Markdown as elements) and `horizon-markdown` (the front matter, the
  links), and nothing of the shell's. It is tested through a pane
  (`tests/extension.rs`), in the gate. It reads files through WASI, with
  `/` lent read-only.
- **The extensions are read once the shell is drawn.** At startup as at
  `Reload Config`: off the UI thread, and the panes that show one are
  made again when they have been read. Until then a pane of one kept
  from the last run says it is being read, and one asked for by name
  (`horizon new <extension>`) is refused with that said.
- **Where an extension's component is.** The ones Horizon comes with are
  beside its executable, as its daemons are, or where cargo builds them
  for the executable's profile (`target/wasm32-wasip2/<profile>`);
  `cargo build --workspace` does not build them, and one that is not
  there is said at startup and left out. The others are named in
  `config.toml`, and one of those with the name of one Horizon comes with
  takes its place. Both are read at startup and by `Reload Config`; an extension whose
  component is built again is read again by itself, and the panes that
  show an extension read again are made again on the state they keep
  (`src/workspace/extensions.rs`). One being written on a branch is
  looked at in a preview pane, from its component's file, beside the
  one in use (`docs/preview-pane-design.md`).
- **A view that paints itself comes later**, through a subset of Canvas
  2D if at all.
- **The boundary has no version: an extension is held to it by type,
  when it is loaded.** What a component imports is held to what the
  host offers (`plugin_host::offered`, from `Extension::load`), and what
  it exports to what an extension must (`shape_of`), at startup and at
  `Reload Config`; one that does not fit is said on stderr with what did
  not, and left out, before any pane is made of it. A component asks
  only for what it uses, so the host offering more -- a function, an
  interface -- breaks nothing; a function taken away or given another
  type is what is refused (`instance export `open-url` has the wrong
  type: .. expected `string` found `u32``). A change of what a function
  means with its type kept is not seen. A version on the WIT package
  (`horizon:ui@0.1.0`) is the component model's own way to say that, and
  to offer two versions side by side; it is left until there is an
  extension that is not rebuilt with the shell.
- **A plugin's CSS is kept to its own elements by how it is written.**
  A sheet goes into the host's document as it is (`adopt-sheet`), beside
  the shell's own views', which are written the same way: with
  `css_module!`, which gives every class a name no other sheet has and
  refuses a rule whose selector names none of them (`div { .. }`), so a
  rule is over the elements its view put the class on. It refuses
  `@layer` too. The host does not look into a sheet, so this holds for a
  guest written with the SDK and not for one that calls `adopt-sheet`
  with CSS of its own. What the engine has for keeping such a sheet in
  (Blitz at the pinned revision, Stylo 0.22, measured 2026-11-16): a
  rule inside `@scope` is not applied at all; a sheet nested under a
  class (`.pane { <the sheet> }`) is kept to that class's subtree, with
  `@media` inside it working and `:root` inside it matching nothing, but
  a sheet wrapped as text gets out by closing the block, so the host
  would have to parse it; an `@layer` beside unlayered sheets fails a
  debug assertion in Stylo's rule tree. A sheet cannot reach the
  network: the shell resolves only `data:` URLs.

- **What an extension uses of the host's beyond the document is said in
  its manifest, and it has nothing else of those** (board #95). The
  manifest's `uses` names an interface, all of it, or one function or
  resource of one: `#[horizon_ui::plugin(.., uses = ["sessions",
  "session-control.show", "modal"])]`. The host refuses a name it has not
  when it loads the extension, and a call to what is not named traps
  with `<interface>.<item> is not in this plugin's uses`, whoever shows
  the extension (`Uses` in `src/plugin_host.rs`). Held to it are the
  theme, the sessions, what is done to a session, and a view over the
  workspace; the document, its events, the pane, the desktop and the
  files it is lent every extension has. One function at a time lets an
  extension go to a session without ending one, say. What is lent still
  depends on who shows it too: a preview pane lends none of these. The
  directories lent are said in the manifest's `access`
  (`access(read = ["/"])`). The component's imports are only held to
  what the host offers (`offered`): a component imports what its code
  calls, and that alone is not a statement of what it is let do.
- **The theme settings are the second extension** (`plugins/theme-settings`),
  and the first whose panes keep nothing: an extension without a
  `struct State` exports no `state`, and its `mount` takes nothing. It
  derives the colors it shows from the seed itself (`horizon-theme`, which
  builds for a guest), the same derivation the shell draws from.
- **The wiki is the third extension** (`plugins/wiki`), and the first
  that writes: `access(write = ["{data}/wiki"])` lends it the wiki's
  directory of Horizon's own data, `{data}` being wherever the
  environment puts that, with `HORIZON_DATA_HOME` in its environment so
  that `horizon-wiki` finds the directory as it does in the shell's own
  processes. The host has nothing of the wiki's: the extension reads the
  pages and the comment logs, appends a comment to a log, and finds the
  project a page is of from the repository's files (a guest cannot run
  git). WASI has no file lock (one was taken out of `wasi:filesystem`
  before 0.2 and is in neither 0.2 nor 0.3), so its line is appended
  without the lock the daemon's writers take: a log is only appended to,
  a line at a time. `create` may refuse, in words (`result<state,
  string>`), which is what the command line answers with.
- **The list of sessions is the fourth extension** (`plugins/sessions`),
  and the first that reads and acts on the workspace. Two interfaces of
  `horizon:ui` are for that, apart so that reading the sessions is not
  asking to change them: `sessions` (`all`: each session's kind, title,
  directory, the project the directory is in, what it was derived from
  and what it is doing; and a `watch` resource whose `changed` comes back
  when they have changed) and `session-control` (`show`, `terminate`,
  `terminate-with-derived`, `open-terminal`, each on a session's id).
  The shell says what its sessions are whenever its model changes
  (`src/workspace/session_feed.rs`) and does what is asked with the
  functions its own commands use; it knows nothing of the list. The keys
  in the list (a step, the next that waits, a search, the question before
  a session is ended) are the view's own and are not commands of the
  shell. A project is the repository a directory is in, a linked
  worktree's being the repository it is a worktree of
  (`horizon_workspace::lineage::project_root`). A preview pane is given
  neither interface. `Manage Sessions` is the extension's own command,
  which puts the list over the workspace (`modal`, below): the same
  rows, in a component of its own that Escape and going to a session
  leave.
- **The status bar is an extension** (`plugins/status-bar`, board #84),
  and the first that a pane does not show: it exports
  `horizon:ui/status-bar` (`mount`, its view's root), written
  `#[status_bar]` beside the plugin's module, and the shell shows the
  view of the extension named `status-bar` in its place at the bottom of
  the window, made again when that extension is read again or another of
  its name is used in its place. What it says is read through
  `horizon:ui/keys`: what the elements the focus is in keep of their keys
  (`focused`, each one's `data-keys` and whether it takes every key) and
  whether the focus is in a text field (`typing`), asked after a layout
  as `has-focus-within` is. The shell keeps its own keys the same way, on
  its root (workspace mode's, or the chords the configuration binds) and
  on a sheet over the workspace (which takes every key), so the bar knows
  nothing of the mode or of sheets; what it says of them all is
  `horizon_ui::keys::said`, in the extension.
- **A preview pane shows extensions only.** The preview plugins it also
  showed (views of the shell's own, built into wasm over sample data)
  went on 2026-11-16, and with them the root library's build for a guest
  and the world they exported (`docs/preview-pane-design.md`). The
  components' gallery is an extension the shell does not come with
  (`plugins/gallery`), and what the host does for a guest is tested with
  a fixture extension (`plugins/fixture`).
- **A pane's state is the extension's type.** The extension declares it
  in WIT, says when it changed (`state-changed`), and gives it when asked
  (`state()`). The workspace saves it under the extension's name and
  hands it to `mount` when the pane is made again; another state is a
  new `mount`, not a change to the mounted view.
- **What the shell asks of a view is a function the extension exports.**
  A command is an exported function. Which it is of is said by the
  interface it is exported in: `pane-commands` are the commands of a
  pane that shows the extension's view, carried out in that pane's
  instance when the cursor is on it, and `commands` are the extension's
  own, which need no pane. The view itself is the interface `pane`
  (`mount`, `state`, `create`); an extension has a `pane`, a command of
  its own, or both, and one without a `pane` is not among the views to
  choose from. The shell lists its own commands, the cursor's pane's and
  every extension's own as one (`Shell::commands`), which is what the
  palette, the key bindings and the command line read; a binding keeps a
  command's name and the command is looked for when the chord is pressed.
- **An extension's own command is carried out in an instance made for
  it.** The instance lasts for as long as it has a view over the
  workspace: it is let go of when the command has ended with nothing
  there, when its view is taken away, or when it stops, and what it had
  there goes with it. Nothing is kept from one carrying out to the next.
  What it said if it stopped is said on stderr.
- **An extension puts a view over the workspace by holding a resource**
  (`modal.modal`): the view is in the sheet the shell's own lists are in
  (`src/workspace/over.rs`), with the keys, for as long as the resource
  is held, and `closed` comes back once it is there no longer -- the
  resource dropped, a press outside the sheet, Escape the view did not
  stop, or something else put over the workspace (one thing is there at
  a time). The view stays the extension's: the shell makes the sheet
  around it and removes the sheet from around it. An instance of either
  kind may hold one: a pane's, whose view over the workspace goes if the
  pane does, or a command's. `horizon_ui::guest::modal` is the SDK's way
  to it.
- **A command's parameters are read from its type.** The host reads the
  exported function's parameter types at run time and asks for each in
  the palette by its type (a choice for an `enum`, a field for a
  string or a number). A file is a resource the host defines in its own
  WIT, since a type's name does not survive to run time; the palette asks
  for one with a file chooser, and the CLI takes a path, made absolute
  where the CLI runs. The CLI asks the running Horizon for the commands
  and their types over the control socket.
- **A new pane's state is made by a function the extension exports**
  (`create`), whose parameters the host asks for the same way. Making a
  pane is the host's one command, the plugin its argument: the view
  chooser inside Horizon, `horizon new <plugin> <arguments>` on the
  command line (`docs/cli-control-plane-design.md`). An extension's own
  commands are only what is its own, and reach the command line by
  their names. What `create` can take, each read from its type: a
  `file` (at most one), asked for with the file finder or taken as a
  path; a text (`string`, or `option<string>` for one that may be left
  out), the next argument of the command line; and a
  `working-directory`, a resource of the host's as `file` is, which
  nobody is asked for: where the pane was asked for (the directory the
  command line was run in, or the one the session under the cursor
  works in). Where nobody types -- the view chooser -- a text that may
  be left out is, and an extension with one that may not is made from
  the command line only. The
  extension is instantiated for the question, `create` is called with
  what it takes, and the state it gives is what the new pane is mounted on
  (`Extension::create`). The manifest's `create.<parameter>` says how a
  parameter is asked for: for a file, the `extensions` to choose among.
  An extension without `create` gets a pane with no state. The export
  cannot be named `open`: a Rust guest's standard library defines that
  symbol.
- **A file is found by typing** (`src/finder/`): the files under a
  directory, listed off the UI thread with `.gitignore` and `.ignore`
  kept to (`.git` left out, dot files listed), and handed as they are
  found to `nucleo`, which matches them on threads of its own against
  what is typed, the way fzf's path scheme does, and wakes the page when
  it has something new. Every word has to be found, case matters once a
  capital is typed, and among equals the shorter path comes first; with
  nothing typed the files are in the order they were listed, each
  directory's by name. The best hundred are shown, each as its path with
  the directories muted and the typed characters marked; a path too long
  loses its start. The directory is the one the cursor's pane's session
  works in, or the one the shell was started in. A file outside it is
  named on the command line.
- **The built-in views are written to the same shape** (`src/plugin.rs`'s
  `Plugin`: a name, a state type, the commands, `mount`). Every view is
  one, the terminal and the agent included: they are built in and run in
  the shell's process, while their sessions stay in `horizon-terminald`
  and `horizon-agentd`. A pane is a plugin and, for those two, the
  session it shows; the session is the host's and is given to the plugin
  apart from its state. What the host needs of a kind of session -- the
  sessions its runtime runs, and how one ends -- the plugin provides
  (`SessionPlugin`), so listing, ending and the startup reconciliation
  with the runtimes do not depend on the kind.
- **A view that runs in its own process, and a backend a package brings,
  wait.** Neither core feature needs them now. What was found on the way
  (wRPC between a wasm view and a native backend, and why it blocks) is
  in `docs/research/plugin-backend-packaging-2026-11-14.md`.

- **An extension is one file**: a wasm component with its manifest in
  it, as a custom section (`horizon:manifest`, JSON: nobody writes it,
  the SDK's macro does). The component's
  world includes `horizon-ui`'s `imports` world and declares only what
  is its own: the interface `pane` (its state's type, `mount` and
  `state` over it, `create`), `pane-commands` and `commands`. What every
  plugin exports
  (`horizon:ui/events`) the SDK implements and exports. The manifest gives the name, the title,
  the commands' titles and descriptions (WIT has no values, and doc
  comments do not reach a component), how `create`'s parameters are
  asked for, and the directories the extension may read; it is matched
  with the component's commands when the component is loaded
  (`src/extension.rs`). A pane keeps the state as WAVE text. The
  component's file is what is named wherever an extension is: the
  configuration's `extensions`, and `horizon new preview`. One file is also
  what the Wasm registries carry (OCI, `wkg`), should extensions be
  handed out that way.
- **An extension written in Rust says its shape once**
  (`horizon_ui::plugin`, `crates/horizon-ui-macros`): a module with a
  `mod pane` (`struct State`, `fn mount`, `fn state`, a function marked
  `#[create(..)]`, and functions marked `#[command(..)]` for the pane's
  commands) and, beside it, functions marked `#[command(..)]` for the
  extension's own. The macro
  writes the WIT world those amount to, hands it to `wit-bindgen`,
  exports the functions through the bindings beside what every plugin
  exports, and puts the manifest in the wasm. A world, a manifest and
  the Rust behind them, written apart, had to be held to each other
  when the extension was loaded; made from one description they cannot
  differ. What crosses to the host is unchanged (the component's type
  and the manifest's section), so an extension in another language is
  one whose WIT and section are written by hand. The Markdown view is
  the first (`plugins/markdown`).

Open: a sheet from a guest not written with the SDK (above); where extensions that are not this repository's
live; a command's parameters beyond the one file a pane is made on.
