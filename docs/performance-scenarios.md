# Performance scenarios

`benches/scenarios/` times the shell's views a frame at a time, to find
where an input costs more than a frame can take, and where the cost grows
with what is shown. It is run by hand; it is not a test and not in the
gate (a time compared with a threshold on a shared machine fails for
reasons that are not the code's).

```sh
cargo bench --bench scenarios                  # every scenario
cargo bench --bench scenarios -- agent/        # those whose name has `agent/`
cargo bench --bench scenarios -- --list
cargo bench --bench scenarios -- --json out.json
```

The first build is a release build of the workspace's dependencies and
takes minutes.

## What a frame is

A scenario builds a view into a document with `horizon_ui::App`, the way
the tests under `tests/` do, and drives it with the document's own input
events. Around each input it times the three things a window does before
it can draw:

| column | what is timed |
| --- | --- |
| `act` | the input handed to the document, its handlers, and the signals' effects (the writes to the document) |
| `layout` | `App::resolve`: style, layout, text, and the work that follows a layout |
| `scene` | `blitz_paint::paint_scene` into a recording scene: what is drawn, as commands |

These are the same code a window runs. Turning the commands into pixels
is the GPU's in a window, and is not timed this way.

## Drawn into a window

```sh
cargo bench --bench scenarios -- --window
```

opens a window on the desktop and draws every frame into it with the
renderer Horizon's window uses (`benches/scenarios/window.rs`). The third
column is then `render`: the scene, the GPU's work on it and its
presentation. A row is marked `N over a refresh` when N of its frames
took longer than the display shows one for.

The window is a real one: it takes its place on the desktop, changes size
from scenario to scenario, and has to stay in view (a window that is
hidden is not presented to, and the run waits). It is not for a desktop
someone is working on without their say, and never for a test or the
gate. The desktop decides the window's size: a scenario that asks for
more than it gives runs at what it was given, and says so on stderr.

Twice in five runs the desktop (GNOME on Wayland) ended the program's
connection during `terminal/typing`; the cause is not known.

## What the table says

Each scenario runs at several sizes of what it shows (a session's turns,
a board's tasks, a terminal's cells). A row is one scenario at one size:
the median of each part, then the median, the 95th percentile and the
longest of the whole frame, in milliseconds. Two marks:

- `over a frame`: the 95th percentile is longer than 16.7 ms, what a
  display at 60 Hz shows a frame for.
- `grows xN at xM the size`: the median frame at this size is N times
  that at the scenario's smallest size, which is M times smaller. Work
  that is done for what is in view does not grow; work done for
  everything there is does. A terminal's frame grows with its cells,
  which are all in view.

A scenario over a real shell on a PTY (`terminal/`, `shell/`) times
whatever frames the shell's output arrives in, so its numbers move more
between runs than the others'.

## Finding the code

A row names a scenario and a part. To see where in that part the time
goes, run the scenario alone under a profiler:

```sh
cargo bench --bench scenarios --no-run         # prints the binary's path
perf record -g target/release/deps/scenarios-<hash> board/select
perf report
```

## Adding a scenario

A scenario is a function from a size to the frames it timed
(`benches/scenarios/main.rs`: `Scenario`, `Stage`). `Stage::frame` times
one frame around an input; `Stage::settle` and `Stage::until` are frames
that are not timed, for getting to where the measuring starts. Put it in
the file for its view and add it to that file's `scenarios()`.
