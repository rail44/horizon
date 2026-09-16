---
name: board-integration
description: Integration policy for work performed through Horizon's own task board.
---

For Horizon board tasks, integrate the task's reviewed changes into main once
its completion conditions, independent agent review, and the repository's
required quality gate are satisfied. A task may contain multiple commits.
Coordinate integration against the current main and rerun checks when the
combined changes require it. Keep detailed review and corrections between
agents; consult the owner if agreed behavior or technical direction must change.

This is the owner's integration policy for Horizon's board-driven task sessions.
Other projects supply their own policy, which may require a PR or human approval.
It does not authorize an unrelated coding session to merge merely because this
skill exists in the repository. Follow explicit instructions for the current work.
