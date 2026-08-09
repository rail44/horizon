# Issues — owner-filed dogfooding findings

Problems the owner hits while actually using Horizon live here, one file
per finding: repro and observed-vs-expected, no fix, no triage. Filing an
issue is not a request to fix it now — when and how a finding gets worked
is decided at the time, outside this directory.

## File format

One issue per file, `NNN-short-slug.md` (zero-padded sequential id).
Frontmatter plus a short body:

```markdown
---
id: 007
title: <one line>
status: open        # open | triaged | in-progress | resolved | wont-fix | duplicate
severity: <blocker | high | medium | low>
area: <affected surface, e.g. agent, terminal, session-manager, workspace>
---

## Repro
1. ...

## Observed
What happened (the bug).

## Expected
What should have happened.

## Notes
Anything else — frequency, environment, guesses. Optional.
```

Keep the body to what only the owner can supply (repro, observed,
expected). Root-cause analysis and design are added later, by whoever
picks the issue up.
