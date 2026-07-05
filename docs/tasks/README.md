# Task Handoff Convention

Specs in this directory are self-contained missions, written so an agent
with no prior context (a Horizon agent session, or any other worker) can
execute them. The convention:

- One file per mission: `NNN-slug.md`, with a `Status:` line
  (`todo | in-progress | done`).
- The spec must carry everything needed: problem, constraints, files
  involved, acceptance criteria. Assume the reader starts from zero and
  will read AGENTS.md first.
- **Definition of done, always**: the full quality gate passes
  (`cargo fmt` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace`), and the agent appends a `## Result` section
  to this same file (what changed, test names, gate line, surprises)
  and flips `Status:` to `done`. The Result section is the report — the
  session that filed the mission watches for it.
- `backlog.md` holds known small issues that are not yet missions.
