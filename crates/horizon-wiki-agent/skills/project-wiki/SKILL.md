---
name: project-wiki
description: Refresh the project wiki -- the human-facing pages that describe Horizon's current state, one per concept plus an overview, kept outside the repository. Use when the owner asks to update, regenerate or extend the wiki, or to write the page for a concept ("update the wiki", "refresh the wiki pages", "write the wiki page for X"); not as a background activity.
---

# Refreshing the project wiki

The wiki is a view of what the project **is now**, written for the owner:
an overview plus one page per concept. Its design, page format and the
reasons behind them are `docs/project-wiki-design.md`; read it first. This
procedure does not depend on which agent runs it: it names commands and
files, not any agent's tools.

## Where things are

The store is `$XDG_DATA_HOME/horizon/wiki/<sanitized-root>/` (default
`~/.local/share/horizon/wiki/...`), where `<sanitized-root>` is the main
repository's toplevel with every `/` replaced by `-` -- the knowledge
layer's naming. In it:

- `index.md` -- the overview.
- `<concept-id>.md` -- one page per concept.
- `concepts.md` -- the concept file: which namespaces each concept covers
  (`## id: title`, then one namespace per line). It is what persists; do not
  replace it with a fresh clustering.

- `threads/` and `snapshots/` -- the owner's comments on pages and the page
  copies they were made on. Never edit them; they are written by the wiki
  view and the thread sessions.

Each page's frontmatter carries an `id` (`urn:uuid:...`) that comments point
at: keep it when rewriting a page, and never copy it into another page.

Nothing in the store is ever placed in the repository. A Horizon agent writes
pages with the `wiki_write` (whole page) and `wiki_edit` (exact-string
replacements) tools, which keep the `id` and need no approval, and reads them
with `wiki_read` and `wiki_list`. Any other agent writes the files directly;
the store is outside any workspace, so under containment that needs an
approval -- ask for it rather than writing pages into the repository.

## 1. Read against the owner's local main

Pages describe local `main`, never a worker branch (a branch can be commits
behind). Take a detached checkout of it and work there:

```sh
git worktree add --detach <tmpdir>/wiki-main main
git -C <tmpdir>/wiki-main rev-parse --short HEAD   # the pages' code_at
```

Remove it with `git worktree remove <tmpdir>/wiki-main` when done.

## 2. The mechanical inputs

Build the tools once per machine (`duckdb-ext/README.md`): `duckdb-ext/build.sh`
for `code-db`, `uv run --locked --script scripts/refactor-audit/run.py setup`
for the jscpd binary `code-db index` requires, and `cargo build -p horizon-wiki`
for `horizon-wiki` (in `target/debug/`). Then, on the main checkout:

```sh
horizon-wiki store-dir <main repository toplevel>     # the <store> below
code-db index <tmpdir>/wiki-main
code-db query <tmpdir>/wiki-main namespace_graph FORMAT=csv > <tmpdir>/edges.csv
horizon-wiki place <tmpdir>/edges.csv <store>/concepts.md
horizon-wiki concept-graph <tmpdir>/edges.csv <store>/concepts.md > <tmpdir>/concept-graph.csv
```

- `place` lists namespaces the concept file does not name, each with the
  concept it is most strongly tied to, and names the file has that the code
  no longer has. Read each new namespace before accepting the proposal: the
  graph sees references only, so a namespace can sit by reference next to a
  concept it does not belong to. A new area tied to nothing is a new concept;
  propose it to the owner rather than inventing one silently. Update
  `concepts.md` before writing pages.
- `concept-graph` gives the directed weights between concepts that pages and
  the overview cite (non-test unit edges, `src,dst,weight`).

## 3. Decide which pages to write

- A page whose namespaces changed since its `code_at`:
  `git -C <tmpdir>/wiki-main diff --stat <code_at> HEAD -- <the page's namespaces>`.
- A page whose concept gained or lost namespaces in step 2.
- A concept with no page yet.

Everything else stays as it is. When the owner asked for one concept, do
only that one (and the overview line for it).

## 4. Write a page

Read the code. Module comments are the place to start, not proof: the first
full pass found several that described code that had since moved. State
nothing about behavior you have not seen in the code at `code_at`.

The format (`docs/project-wiki-design.md`, "The page pilot"):

- Frontmatter: `concept`, `title`, `code_at` (with "(local main)"), `written`.
- What it is: a few lines.
- Entry points: where users and other concepts touch it.
- The flow inside: the main types and functions, each with its file.
- Links to other concepts, with the weights from `concept-graph.csv`.
- Related records, links only, never restated:
  - docs: candidates are the `docs/*.md` that mention the concept's paths
    (`grep -l` over `docs/` for its directories); keep the ones that are
    about it after reading their titles and the matching lines.
  - board: fold the board's log and pick items by reading titles; list open
    items apart from past ones.

    ```sh
    jq -s -r '[.[] | select(.type=="task-stored" or .type=="task-imported") | .item]
      | group_by(.id) | map(last) | .[] | "\(.id)\t\(.status)\t\(.title)"' \
      "${XDG_DATA_HOME:-$HOME/.local/share}/horizon/board/<sanitized-root>/events.jsonl"
    ```
  - knowledge-layer entries whose anchors fall in the concept's namespaces.
- Stale text noticed while reading (a comment or doc that no longer matches
  the code), under its own heading.
- The page's namespaces, as its anchors.

Pages are written in Japanese: the reader is the owner.

## 5. Update the overview

`index.md` keeps the process split, every concept grouped by where it runs
with one line each, the strongest edges from `concept-graph.csv`, and the
entry records (`AGENTS.md`, `docs/roadmap.md`, `docs/ux-principles.md`).
Every concept line links to its page.

## 6. Report

In the conversation, not in the pages: which pages were written or left,
what changed in `concepts.md` and why, and the stale text found -- offered as
fixes for the owner to decide on, since fixing it is repository work.
