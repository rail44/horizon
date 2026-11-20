# Image viewer

An image a program in a terminal names by a link is looked at without
leaving Horizon: Claude Code, for one, writes each image it reads as an
OSC 8 hyperlink to its file (`[image] ~/.claude/…/focus.png`). The link,
clicked with Ctrl held (Cmd on macOS), opens the image over the
workspace, in the same sheet the palette and the list of sessions use;
a press outside it or Escape takes it away.

The viewer is an extension, `plugins/image`. What it is made of is four
parts, each of which is general and not the viewer's alone.

## A link followed is the shell's to open

The terminal view hands every link clicked with Ctrl held (Cmd on
macOS) to whoever placed it (`TerminalView::open_link`). Not Super off
macOS: the desktop takes Super with the pointer for itself (GNOME moves
the window with it).

Only an OSC 8 hyperlink is a link; text that reads as a URI is not.
Claude Code, like other programs that ask `supports-hyperlinks`, writes
one only to a terminal it recognizes by its environment, and Horizon's
is none it knows by name: `horizon-terminald` sets `FORCE_HYPERLINK=1`
for what a terminal runs, unless the environment it inherited already
sets it.

The shell (`src/workspace/links.rs`) opens:

- an `http`/`https` URI with the desktop, as before;
- a `file://` URI that names a file on this machine -- no host,
  `localhost`, or this machine's name -- with the first extension that
  opens files of its media type;
- nothing else. A `file://` URI is not handed to the desktop: OSC 8 lets
  the far side of an ssh connection name any URI, and the terminal is not
  a way for it to run what a file manager would run
  (`crates/horizon-shell-runtime/src/terminal_links.rs`). An extension
  that opens a file is code of Horizon's or the owner's choosing, run in
  its sandbox.

## An extension says what it opens

A file's media type is told from its name (`src/media_type.rs`,
`mime_guess`'s table); nothing is read to tell it. What an extension
takes is said in media types: a type (`image/png`), a kind
(`image/*`) or any (`*/*`).

An extension that opens files exports `horizon:ui/opener`, one function,
`open: func(file: file)`, and its manifest says the types it opens
(`opens`). Written in Rust it is one function:

```rust
#[open(types = ["image/png", "image/jpeg"])]
pub fn open(file: horizon_ui::guest::File) { .. }
```

The two are held to each other when the component is loaded. The shell
looks among the extensions in the order they are read -- those
`config.toml`'s `extensions` names, in its order, then those Horizon
comes with -- and runs `open` of the first that opens the file's type in
an instance made for it, as an extension's own command is run: the
instance lasts as long as what it put over the workspace.

The file a new pane of an extension is made on (`create`'s file
parameter) is chosen by the same media types (`file(types = [..])`;
the Markdown view's is `text/markdown`).

## The document reads the files it names

The document fetches what its elements name through a provider of
`horizon-ui`'s own (`crates/horizon-ui/src/files.rs`): a `file://` URL
is read on a thread of its own, and blitz-dom decodes what it is handed
there too and takes the image in at its next layout. Any view's
`<img src="file://…">` is drawn this way, whichever extension wrote it;
a view cannot read an image's pixels back, and what it can learn is the
size it is laid out at. `horizon-ui` turns on the formats blitz-dom
decodes with (png, jpeg, gif, webp; svg is blitz-dom's own).

blitz-dom keeps every image it fetched by its whole URL for as long as
the document lasts, and has no way to let one go. The provider reads the
file without the URL's query and fragment, and the viewer puts the time
it is opened in the query, so a file written again at the same path is
read again; what each opening decoded stays in memory until Horizon
ends (about 4 MB for a 1280×800 screenshot).

## The view

The image at its own size, or made smaller to fit the window with its
shape kept (`plugins/image/src/view.rs`). It opens png, jpeg, gif, webp
and svg: the types the document decodes.

Tested through a pane of the shell (`tests/image.rs`): a link in a
terminal to an image opens it over the workspace and Escape takes it
away, a large one fits and is drawn, a link to a file no extension
opens or on another machine is left alone, and a web page's goes to the
desktop.
