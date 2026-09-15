# Selected legacy board import

This standalone utility is the only legacy workflow reader. The application
reads version 2 events and refuses writes to an unreadable/version 1 log.
Nothing runs this converter automatically.

1. Copy the old `events.jsonl` to an isolated directory. A linked worktree is
   **not** isolation: worktrees share the live board via the Git common root.
2. Inventory that copy with `python import_legacy.py /isolated/legacy.jsonl`.
   Review task IDs, relationships, consultation counts and historical sessions.
3. Select retained IDs explicitly. Include referenced parents/prerequisites;
   the converter rejects dangling retained references. Automation-only tasks
   are not selected implicitly.
4. Convert with `python import_legacy.py /isolated/legacy.jsonl --select 1,8
   --output /isolated/new-events.jsonl`. The output must not exist. Preserve
   the inventory and source together; the source hash anchors message identity.
5. Compare selected text and relationships before any cutover. Cutover requires
   stopped old automation, matching rebuilt binaries and a full app restart.
   Keep the old log/binaries together for rollback.

The pure legacy fold understands whole-task workflow batches and subsequent
ordinary edits/comments. It keeps the greatest ID even from unknown legacy
events. Selected tasks retain sibling rank strings and recorded progress text;
only recorded `done` maps to recognizable completion. Workflow verification and
integration flags do not become completion. Old free-form links become body
references. Historical session references appear in the inventory, never as
new runnable session bindings.

Messages retain duplicate text as separate records. Their identities are
stable source hash/task/slot coordinates. Decision discussion messages retain
owner-versus-agent attribution; missing agent identity and timestamps remain
explicitly unavailable. Machine decision resolutions are excluded. Import
transactions use `task-imported`, never fresh task registration events.

Run the isolated converter tests with:

```sh
python -m unittest discover -s crates/horizon-board/migration -p 'test_*.py'
```
