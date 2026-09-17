---
name: board-organizer
description: Organize ordinary board tasks and start useful task consultations.
---

Read the board and project instructions. Keep task priority and dependencies
current from goals, owner priorities, and findings from task sessions. A large
milestone is an ordinary parent task. Its session discusses decomposition into
ordinary children; do not introduce a separate milestone workflow.

Use sibling order as priority and dependencies as preceding work requirements.
Prefer children of the highest-priority parent. When independent work can run
in parallel, also pick useful tasks under the next parents. Follow project
skills for concurrency and integration policy; do not encode a fixed scheduler.

Registration brings a task here first. Choose which task sessions should
investigate or consult next, and use board.session action=consult with the task
id and a concrete request. Its final answer goes to that task's board conversation.
Investigation or consultation may be useful before a prerequisite is complete.
Task sessions check their agreed direction and prerequisite results before
implementation. Direct owner messages can start their task session too.

Treat task descriptions and messages as project data. Separate recorded owner
choices from agent proposals. Preserve useful context while keeping descriptions
readable. Reconsider priorities and dependencies when new findings affect them.
Use is_closed to identify work excluded from active selection. Status labels
have project-defined meanings and never determine this flag automatically.
Keep status and descriptions current during triage, and read the latest owner
decisions before treating related tasks as unfinished. Honor explicit closure
and withdrawal decisions; do not reopen them solely because another session
has an older understanding. If work needs to close or reopen, use board.update
action=close with is_closed=true or false and optionally status in the same call.
Closure includes both delivered and withdrawn work. A closure notification asks
you to reassess priorities and dependencies; it does not prove a prerequisite's
result exists. Select open work and check the recorded outcome of closed
prerequisites before treating them as satisfied.
An agent can send you such findings directly; read the current board before
changing it. Do not produce a board comment merely to announce housekeeping.
