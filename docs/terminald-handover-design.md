# terminald handover — a terminal daemon that gives its sessions to its next build

Status: designed 2026-11-21 (board #99). Step 1, the daemon's own PTY
(`crates/horizon-terminald/src/pty.rs`), is implemented; the handover
itself is not.

## Problem

`Reload Terminal Runtime` is how a rebuilt `horizon-terminald` comes into
use, and it ends every terminal session and what runs in it: the shell
terminates the sessions in the workspace, the daemon is drained, and a new
one starts empty (`src/workspace/reload.rs`, `src/terminald.rs`).
`docs/terminald-split-design.md` made that daemon rarely restarted, and
recorded a server hot-swap as work no terminal multiplexer had done, to be
avoided; its backstop is the workspace snapshot restore.

## What changed since

A session's state is two things: the PTY with the program in it, and the
terminal emulator's state.

- **The emulator's state crosses already.** libghostty-vt writes a
  terminal's whole state out (`Terminal::encode_snapshot`) and reads it
  back (`snapshot::Decoder`): screen, history, modes, kitty keyboard
  flags, title, and a parser that stopped in the middle of a sequence.
  `TerminalCore::state` / `TerminalCore::from_state` use it for every view
  that attaches, and
  `a_state_taken_between_any_two_bytes_goes_on_to_the_same_screen` (in
  the gate) checks that a state taken between any two bytes of a
  program's output goes on to the same screen.
- **The PTY is held by a descriptor and a process id.** Since step 1 the
  daemon keeps nothing else of either, so both can be given to another
  executable.

What is left is keeping the PTYs and the programs across the replacement
of the daemon's executable, which is the operating system's part.

## Mechanism

The daemon replaces its own executable with `execve`. Its process id stays
the same, so the programs stay its children; descriptors without
close-on-exec stay open, so the PTYs' master sides and the listening
socket stay open.

1. **Asked.** The shell calls a new `TerminalHub` method, `hand_over`.
2. **Reading stops.** Each session's reader stops before its next read
   (the reader waits on the PTY and a wake descriptor with `poll` rather
   than in `read`). What a program writes after that waits in the
   kernel's PTY buffer for the next daemon.
3. **Each core finishes and writes out.** The core loop takes in
   everything the reader sent, then writes out `TerminalCore::state()`
   and what the core keeps beside the engine: the bytes the OSC scanner
   holds back (handed back as bytes not yet read), the color scheme, the
   scrollback limit. The writer writes what it still has for the PTY.
4. **The handover is written** to an anonymous file (a `memfd` on Linux,
   an unlinked temporary file elsewhere): its format version, and per
   session the session id, the master descriptor's number, the process
   id, the size, the last cwd and program status the daemon published,
   and the core's state.
5. **The daemon executes its executable again**: the path it was started
   from, recorded at start (`/proc/self/exe` names the replaced file once
   a build overwrites it), with `--adopt <descriptor>`. The masters, the
   listener and the handover are cleared of close-on-exec first.
6. **The new daemon adopts**: reads the handover, builds each session's
   threads around its descriptor and `TerminalCore::from_state`, and
   accepts on the listener it was given. Connections made while it
   started wait in the listener's backlog, so a shell never finds the
   socket unanswered and never starts a second daemon.

If `execve` fails (the file is gone, not executable), the daemon executes
`/proc/self/exe` (its own replaced file, still readable through that
link) with the same handover, so the sessions come back on the old
build. Where neither can run, the daemon logs and exits as a drain does.

## When the state cannot be read

The emulator state's format is "versioned separately from the library and
isn't covered by the ABI promise" (libghostty-vt `snapshot.rs`), and the
handover's own format is the daemon's. They are read separately:

- The per-session record (descriptor, process id, session id, size) is the
  part a new build must always read; it changes only with a version of
  its own.
- Where the emulator state cannot be decoded (a libghostty-vt bump changed
  the format), the program and its PTY stay and the screen starts empty
  at the session's size (board #99). The new daemon then sends `SIGWINCH`
  to the PTY's foreground process group (`tcgetpgrp` on the master), so a
  full-screen program draws itself again; setting the same size again
  would not, since the kernel signals only a change. A shell's history is
  lost.

## The commands

- `Reload Terminal Runtime` hands over (board #99): the terminal sessions
  stay, with their screens.
- The destructive form stays under another name (board #99), for a
  handover that does not work and for ending every terminal on purpose.
  Proposed: `Restart Terminal Runtime, Ending Terminals`
  (`restart-terminal-runtime`), the present behavior unchanged.

## The shell's side

`Reload Agent Runtime` already keeps its sessions across a new daemon
(`reload_agent_runtime` in `src/workspace/reload.rs`): it lets go of its
attachments without a word to the sessions, has the panes made again,
and once the runtime is back takes the runtime's list in (`take_in`) and
attaches to each. The handover reload is the same with terminals:
`Terminald::hand_over` in place of `Terminald::restart` (no drain, no
`terminate_session`), then the new connection's `list_terminals` taken in
and each pane made again, which attaches and asks for the state.

A handover to a build whose terminal wire version differs from the
shell's is refused at the shell's next `hello`, as any skew is
(`MIN_SUPPORTED == current`); Horizon is then restarted, and the sessions
are still there for it.

## Wire

`hand_over` is appended to `TerminalHub` (append-only, decision 5 of
`docs/terminald-split-design.md`); `check-wire-schema.sh` decides whether
the terminal wire version moves with it.

## Checks

As `tests/terminald.rs` runs the shell over a real daemon: a session typed
into, the runtime handed over, then the same process still answering
input and the screen the same as before (its text and its history). A
second check hands over with an emulator state that does not decode and
expects the process alive with an empty screen. A third makes `execve` of
the recorded path fail and expects the sessions back on the old build.

## Not covered

- A daemon that crashes or is killed loses its sessions as before; the
  workspace snapshot restore stays the backstop.
- The macOS branches (the temporary file, `fcntl` instead of
  `close_range`) do not run on this machine.
- A program that writes more than the kernel's PTY buffer holds while the
  daemon restarts waits on the write until the new daemon reads; nothing
  it wrote is lost.
