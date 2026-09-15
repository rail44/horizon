---
name: board-task
description: Consult on and implement one ordinary board task in the same session.
---

Read the assigned task, its parent, prerequisites, consultation, and project
instructions. If the project supplies a board-integration skill, read it for
the project's integration policy. Discuss purpose, user-visible behavior, and technical approach at
a depth the owner can judge. Resolve code details yourself and keep the working
investigation in session history. Match the conversation's language. State
uncertainty and verification limits; never invent owner agreement.

For a large task, propose concrete child tasks and record useful decomposition
with board.update. Send findings that affect priorities or dependencies to the
board organizer using board.session action=send. The organizer handles board-wide
ordering. Both directly registered ideas and conversation-created tasks are valid.

When the direction is sufficiently clear and prerequisite results permit work,
continue implementation in this same session. Choose an explicit starting commit
containing the needed prerequisite results. Request board.session action=implement
with that base. Wait for the environment change result before editing, then use
the new working root. Keep goal, completion conditions, agreed design, and reasons
in the task body. Keep detailed implementation work in session history.

If implementation requires changing agreed behavior, scope, or technical direction,
pause the affected work and explain the failed premise, impact, options, and your
recommendation to the owner. Handle ordinary code judgments without unnecessary
consultation. A dependency notification is a reason to reread the current task and
all prerequisites, not proof that implementation can proceed.

Request a separate reviewer with board.session action=review, supplying the task,
recorded base commit, target tip, and actual checks/results. A review can cover
multiple commits. Handle code findings and corrections between agents, requesting
review again at the updated tip when needed. Explain the result and remaining
human decisions in language the owner can assess.

Use the project's skill or explicit instructions for integration destination
and conditions. Read board-integration if the project provides it. Some projects
integrate reviewed work directly; others require a PR or human approval. Missing
integration policy calls for consultation. Record task completion separately
with board.update only when its actual completion conditions are satisfied and
its results are available as required by the project. Ending a turn is not task
completion. Intermediate state labels follow project policy.

The triggering input specifies where your final answer goes. Address that
recipient. The harness forwards the final answer; working commentary stays in
session history. Use board.comment or board.session action=send only for an
explicit additional message during work. Passive notifications do not request
an automatic reply. Do not duplicate your final answer with a board.comment call.
