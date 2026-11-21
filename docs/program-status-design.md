# Program Status Design

Scope: what a session is doing, as the tab strip, the session manager and
`horizon sessions` show it. Foremost that is what a program running in a
terminal session says of its own state over OSC 7501, the Program Status
Protocol (<https://www.superlogical.com/rex/docs/build/program-status>,
revision 0.4), followed here from the PTY on.

A coding agent that runs as a TUI in a terminal pane (Claude Code, Codex)
reports this way whether it is working, waiting on its user, or done. The
session stays a terminal session: nothing here sends the program input,
answers its prompts, learns its own session id, or resumes it.

An agent session of `horizon-agentd` shows in the same places, from what
its runtime says of it (§3).

## 1. Pipeline

1. **Extraction** — `crates/horizon-terminal-core/src/core/osc_notify.rs`
   takes OSC 7501 off the PTY stream ahead of the VT parser, which does not
   know the sequence, as it takes the notification sequences.
   `core/program_status.rs` reads the body.
2. **The answer** — a program asks `ESC ] 7501 ; ? ST` whether its reports
   are read. `TerminalCore::write_vt` answers on the PTY with the states
   and kinds it reads (`program_status::QUERY_REPLY`). Claude Code reports
   only to a terminal that answered, and only where the answer comes before
   the one to DA1, which it asks right after: the engine takes in the bytes
   up to each question before it is answered, so the answers keep the
   order of the questions when both are read at once.
3. **The root record only** — a report is
   `ESC ] 7501 ; key=value(:key=value)* ST`. Read from it: `state`, `kind`
   (with `blocked`), `app`, `msg` (base64). A record with an `id` is one of
   the program's own pieces of work (Claude Code sends one per background
   command or subagent, and `state=clear:id=…` when it ends); both are
   dropped, so the end of one is not taken for the end of the program's
   own state. `state=clear` without an `id` takes the report back.
4. **Wire** — `TerminalUpdate::ProgramStatus(Option<TerminalProgramStatus>)`
   (terminal wire v21), `None` for a report taken back.
5. **Daemon** — `horizon-terminald` keeps the last status of each session
   (`HostedTerminal::program_status`), sends a change once, and sends what
   stands to every newly installed subscriber, as it does the working
   directory. A program that waits on its user reports once and then says
   nothing, so a shell that attaches later is told.
6. **Model** — `Workspace::set_session_activity` records it on the
   session (`WorkspaceSession::activity`, a `Doing::Program` in
   `crates/horizon-workspace/src/types/activity.rs`), in the protocol's
   own words. Not persisted.
7. **Seen** — `SessionActivity::seen` is set by
   `Workspace::mark_session_seen` when the session is the active pane's in
   a window that has the focus: the condition under which a notification
   from that session is not posted (`Shell::take_in_sessions`,
   `src/workspace/mod.rs`). What the session comes to be doing next is
   not seen, unless it is what was last looked at with no work since
   (§3). Claude Code keeps reporting `done` until its next
   prompt, typed at or not, so finished work that has not been looked at
   is told apart here and not by the program.

## 2. Where it shows

What a session is doing is kept in its own kind's words, and read one way
for every kind: `SessionActivity::attention`, what the session waits on
its owner for.

| Attention | A terminal's program | An agent session |
| --- | --- | --- |
| `approval` | `blocked`, `kind=permission` | `waiting-for-approval` |
| `input` | `blocked`, `kind=question` | `paused` (stopped by a guard, to be told to go on) |
| `login` | `blocked`, `kind=auth` | — |
| `blocked` | `blocked` with no kind | — |
| `failed` | `error` | `failed` |
| `unseen` | `done`, not seen | `completed`, not seen |

- **Tab strip** — a mark before the title of a tab whose titled session
  has an activity (`src/workspace/activity.rs`, `mark`): `?` for
  `approval`, `input`, `login` and `blocked`, `!` for `failed`, `●` for
  `unseen`; otherwise `◐` at work and `✳` at rest. A program that is
  answered leaves out the mark it puts in its own title otherwise (Claude
  Code's `✳` and `◐`), which is why the tab draws one. The mark is text in
  the tab's label, not a designed indicator.
- **The list of sessions** (`plugins/sessions`, in a pane or as the
  list over the workspace) — a row is two lines, as Zed's list of threads
  draws one (board #95): an icon for the attention or `working`, which
  turns, a turn a second (one for the kind -- a terminal's or an
  agent's -- where there is neither), the
  title, muted where no pane shows the session, and how long it has been
  in its state; under it the program's name or the session's kind, the
  attention's or `working`'s word, and its words. The extension reads these from the host's
  `sessions` interface (`src/workspace/session_feed.rs`).
- **`horizon sessions`** — `SessionEntry::activity`
  (`crates/horizon-control/src/contract.rs`): `state` (the kind's own
  word), `attention`, `seen`, `since_ms`, a program's `app`, and `detail`. Absent
  for a session nothing is known of. The text form adds a column,
  `<app>:<state> [<attention>]`, and then the detail.

## 3. Agent sessions

The shell is attached to every agent session of the workspace, shown or
detached (`src/restore.rs` attaches the ones that come back), and each
says what it is doing when asked (`AgentSession::activity`, the status the
board's rows show). `Shell::take_in_sessions` asks at every layout and
tells the model where the answer is not what it has (`Doing::Agent`,
`AgentStatus`); an agent session wakes the page when it changes.

`loading`, `unavailable` and `terminated` have no mark on a tab: there is
no session at the other end to be at rest.

What a session was last seen doing is kept in `workspace.json`
(`WorkspaceSession::looked_at`, `SessionState::looked_at`), for either
kind, and dropped when the session is next at work. A session that says
after the shell is restarted what it was last seen doing is seen; one that
says something else is not. A session that worked and came back to the
same words while the shell was away is taken as seen: nothing a session
says tells one piece of finished work from the next. A program's message
is part of what is kept.

## 4. What a program's status does not say

- Which of the program's own sessions it is: the protocol carries no
  session id. `/clear`, a resumed conversation and a restarted program in
  the same pane are not told apart.
- Whether a model request or a tool is running: both are `working`.
- Whether a finished turn ends in a question: both are `done`.
- That the program has gone, when it went without `state=clear`: the last
  status stands until the terminal session ends. Horizon reads no OSC 133
  prompt marks, which is the protocol's other occasion to drop a record.
- Anything, for a program that does not report: an agent that does not
  implement the protocol, Claude Code inside tmux, or Codex before
  0.163.0. A background session of Claude Code (`claude --bg`) reports
  through `claude agents` while that is attached to it, in the terminal
  `claude agents` runs in (§5).

## 5. Measured

Claude Code 2.1.327, 2026-11-18, in a PTY read by the terminal core
(`crates/horizon-terminal-core/src/program_status_probe.rs`, an ignored
test that runs a real program and logs what it reports and when):

| Occasion | Report |
| --- | --- |
| Start | `?` 0.3–0.6 s in; the first status right after the answer |
| Folder trust prompt | `blocked`, `kind=permission`, "trust this folder" |
| Trust accepted | `clear`, then `idle` |
| Prompt sent | `working` within 0.1 s; a running tool adds a message |
| Tool approval (permission mode `default`) | `blocked`, `kind=permission`, "approve Bash: <command>" |
| AskUserQuestion | `blocked`, `kind=question`, the question and its options |
| Turn ended | `done`, unchanged by keys typed after it |
| Esc during a turn | `idle` |
| Background command still running | root stays `working` ("1 shell still running") past the turn's answer; the command is `id=<task>` with a title, cleared by id when it ends |
| `/exit`, or the process killed | `clear` |

Without the answer the same session reported nothing over OSC 7501 and put
its mark in the title (`✳ Claude Code`, `◐ Claude Code`); with it, the
title came without the mark.

`claude agents`, Claude Code 2.1.330, 2026-11-21, in a PTY that answered
as the terminal core does (a script; nothing typed into a session):

| Occasion | Report |
| --- | --- |
| Start | `ESC [ ? u` and `ESC ] 7501 ; ? BEL` in one write, DA1 in the next |
| The list | none; the title is the list's (`1 awaiting input · claude agents`) |
| A background session attached | `clear`, then the session's own status (`done`, `idle`) |
| The window resized while attached | the same status again |
| Focus in or out while attached | none |
| Two attached to one session | each its status |
| The DA1 answer before the OSC 7501 one | nothing over OSC 7501 for as long as it runs |

Codex was not measured. Its source (HEAD of 2026-11-17) reports without
asking, returns from `done` to `idle` on the next key, paste or click, and
reports only a root record.
