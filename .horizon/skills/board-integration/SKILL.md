---
name: board-integration
description: Integration policy for work performed through Horizon's own task board.
---

For Horizon board tasks, integrate the task's reviewed changes into main once
its completion conditions, independent agent review, and the repository's
required quality gate are satisfied. A task may contain multiple commits.
For finished implementation work, close the task after the required integration
using board.update action=close, is_closed=true, and an appropriate status such
as "done". For withdrawn work, use the same operation with an appropriate status
such as "archived", retaining the owner's reason. These labels are conventions,
not code-defined transitions. Open work keeps is_closed=false, including work
waiting for review or integration. Reopening is an explicit decision and uses
is_closed=false with the current status. A closed prerequisite must still be
read before deciding whether its result is available or its dependency needs
revision.
Coordinate integration against the current main and rerun checks when the
combined changes require it. Keep detailed review and corrections between
agents; consult the owner if agreed behavior or technical direction must change.

This is the owner's integration policy for Horizon's board-driven task sessions.
Other projects supply their own policy, which may require a PR or human approval.
It does not authorize an unrelated coding session to merge merely because this
skill exists in the repository. Follow explicit instructions for the current work.
