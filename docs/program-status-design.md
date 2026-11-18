# Program Status Design

Scope: what a program running in a terminal session says of its own state
over OSC 7501, the Program Status Protocol
(<https://www.superlogical.com/rex/docs/build/program-status>, revision
0.4), from the PTY to the tab strip, the session manager and
`horizon sessions`.

A coding agent that runs as a TUI in a terminal pane (Claude Code, Codex)
reports this way whether it is working, waiting on its user, or done. The
session stays a terminal session: nothing here sends the program input,
answers its prompts, learns its own session id, or resumes it.

## 1. Pipeline

1. **Extraction** — `crates/horizon-terminal-core/src/core/osc_notify.rs`
   takes OSC 7501 off the PTY stream ahead of the VT parser, which does not
   know the sequence, as it takes the notification sequences.
   `core/program_status.rs` reads the body.
2. **The answer** — a program asks `ESC ] 7501 ; ? ST` whether its reports
   are read. `TerminalCore::write_vt` answers on the PTY with the states
   and kinds it reads (`program_status::QUERY_REPLY`). Claude Code reports
   only to a terminal that answered.
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
6. **Model** — `Workspace::set_session_program_status` records it on the
   session (`WorkspaceSession::program`,
   `crates/horizon-workspace/src/types/program.rs`), in the protocol's own
   words. Not persisted.
7. **Seen** — `ProgramStatus::seen` is set by
   `Workspace::mark_session_program_seen` when the session is the active
   pane's in a window that has the focus: the condition under which a
   notification from that session is not posted
   (`Shell::take_in_sessions`, `src/workspace/mod.rs`). Claude Code keeps
   reporting `done` until its next prompt, typed at or not, so finished
   work that has not been looked at is told apart here and not by the
   program.

## 2. Where it shows

- **Tab strip** — a mark before the title of a tab whose titled session
  has a status (`src/workspace/program.rs`, `mark`): `◐` working, `?`
  blocked, `●` done and not seen, `!` error, `✳` idle or done and seen.
  A program that is answered leaves out the mark it puts in its own title
  otherwise (Claude Code's `✳` and `◐`), which is why the tab draws one.
  The mark is text in the tab's label, not a designed indicator.
- **Session manager** — the session's line carries
  `<app>: <state> (<waiting for>), unseen — <message>` as far as each part
  is there (`program::line`).
- **`horizon sessions`** — `SessionEntry::program`
  (`crates/horizon-control/src/contract.rs`): `state`, `blocked_on`, `app`,
  `message`, `seen`. Absent for a session whose program does not report.
  The text form adds a column, `<app>:<state>(<waiting for>)`, with
  `,unseen` on finished work nobody has looked at.

## 3. What a status does not say

- Which of the program's own sessions it is: the protocol carries no
  session id. `/clear`, a resumed conversation and a restarted program in
  the same pane are not told apart.
- Whether a model request or a tool is running: both are `working`.
- Whether a finished turn ends in a question: both are `done`.
- That the program has gone, when it went without `state=clear`: the last
  status stands until the terminal session ends. Horizon reads no OSC 133
  prompt marks, which is the protocol's other occasion to drop a record.
- Anything, for a program that does not report: an agent that does not
  implement the protocol, Claude Code inside tmux or as a background
  session (`claude --bg`, also while attached), or Codex before 0.163.0.

## 4. Measured

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

Codex was not measured. Its source (HEAD of 2026-11-17) reports without
asking, returns from `done` to `idle` on the next key, paste or click, and
reports only a root record.
