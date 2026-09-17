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

Keep the task's status and description current at meaningful changes of stage.
Use the project's vocabulary; preserve decision history in the conversation,
while the description explains the current agreement and remaining work.
The status text is independent of is_closed: writing "done", "archived", or
any other label does not close a task. Use board.update action=close with
is_closed=true when the task is finished or explicitly withdrawn; include status
in that call to update the label atomically. Use is_closed=false to reopen work
when its direction calls for it, optionally setting its new status in the same
call. Neither a new comment nor an ended turn automatically reopens or closes it.
Respect an owner's decision to close or withdraw work. Read the latest task and
owner decisions before describing related work as outstanding; new findings do
not silently invalidate an earlier closure decision.

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
all prerequisites, not proof that implementation can proceed. A closed
prerequisite may have been withdrawn rather than delivered. Check its recorded
outcome and reassess the dependency before relying on any result.

Request a separate reviewer with board.session action=review, supplying the task,
recorded base commit, target tip, and actual checks/results. A review can cover
multiple commits. Handle code findings and corrections between agents, requesting
review again at the updated tip when needed. Explain the result and remaining
human decisions in language the owner can assess.

Use the project's skill or explicit instructions for integration destination
and conditions. Read board-integration if the project provides it. Some projects
integrate reviewed work directly; others require a PR or human approval. Missing
integration policy calls for consultation. When closing finished work, check its
actual completion conditions and that its results are available as required by
the project. When closing withdrawn work, retain the reason and any conditions
for revisiting it. is_closed records removal from active work, not proof of a
successful result. Status labels and the conditions for closing follow project
policy; no status registry or fixed state sequence is required.

The triggering input specifies where your final answer goes. Address that
recipient. The harness forwards the final answer; working commentary stays in
session history. Use board.comment or board.session action=send only for an
explicit additional message during work. Passive notifications do not request
an automatic reply. Do not duplicate your final answer with a board.comment call.
