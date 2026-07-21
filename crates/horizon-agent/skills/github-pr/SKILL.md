---
name: github-pr
description: Publish work to a GitHub pull request with the authenticated gh CLI. Use when asked to commit, push, open, inspect, or update a PR for a repository whose remote is GitHub.
---

# Publishing a GitHub pull request

Use Git for local history and the authenticated `gh` CLI for GitHub state.
Do not use `web_fetch` on a browser-oriented `/pull/new/...` URL: it cannot
carry the user's CLI authentication and normally returns a login page.

## Workflow

1. Inspect `git status --short`, the current branch, and `git remote -v`.
2. Review the intended diff and keep unrelated user changes out of the commit.
3. Commit with a message that describes the outcome. Omit `timeout_secs`
   normally; if hooks run builds or tests, allow at least 600 seconds.
4. Push the exact current branch to its GitHub remote.
5. Create the PR with `gh pr create`, supplying explicit base, head, title,
   and body. The body should summarize behavior and verification actually run.
6. Verify the result with `gh pr view --json number,url,state,headRefName,baseRefName`.

Try `gh` directly before claiming PR creation is unavailable. If it is missing
or unauthenticated, confirm with `command -v gh` or `gh auth status`, then report
the pushed branch and the exact remaining authentication problem. Do not treat
an unauthenticated HTML login page as evidence that the CLI cannot create the
PR.
