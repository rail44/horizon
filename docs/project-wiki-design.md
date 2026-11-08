# Project wiki — design

Status: shape decided with the owner 2026-11-07/08; concept grouping
spiked and accepted 2026-11-08 ("悪くないと思うのでこれで進めてみましょう");
a pilot of the overview and three concept pages accepted the same day
("よい感じだと思います"); all 21 concept pages written by the session
agent the same day, read against local main a3521c17. Background reading: Anthropic's "The
components of agent memory" (2026-11-06) on wikis built by dreaming.

## Problem

The owner, 2026-11-07:

> docsはそもそも人間が読みやすい形式や粒度ではない
> 現状のプロジェクトの状態を眺め、次に手を入れる箇所の探索や知識を得るため

`docs/` records decisions; it is not a way to look over what the project
is now, find where to work next, or learn an area. The wiki is that view,
written for a person. It is separate from the knowledge layer
(`agent-knowledge-design.md`), which holds lessons for agents.

## Decisions

1. **Shape: an overview page plus one page per concept, linked to each
   other** (owner: "D"). A concept is a unit a person thinks in — sandbox,
   approval, the terminal — which usually spans several crates.
2. **Pages describe current behavior only** (owner: "a"). Design decisions
   and in-flight work are not restated: a page links to the `docs/` record
   or board item instead ("bやcはそれこそリンクでよい").
3. **The store is user-side Markdown** (owner: "iii"), like the knowledge
   layer: under `$XDG_DATA_HOME/horizon/`, outside the repository, so
   regenerating pages makes no commits.
4. **Analysis tooling is built where it is missing.** The owner wants the
   namespaces and the dependencies that should become links analysed by a
   mechanism, not guessed. The code database (`code-database-design.md`)
   already provides namespaces and the weighted namespace graph from SCIP;
   the missing part was grouping namespaces into concepts.
5. **Concepts come from clustering plus an agent pass** (owner: combine
   "b" and "c"): community detection on the namespace graph proposes
   groups; an agent names, splits, merges and moves them and writes the
   result down as data.

## The concept grouping spike (2026-11-08)

`duckdb-ext/cluster-spike/` (README, `results/findings.md`,
`results/concepts.md`). Input: 443 namespaces and 2068 undirected edges
weighted by non-test unit edges.

- leiden-rs (Leiden, modularity, resolution 2) proposed 18 groups, most of
  which read as one concept; the agent pass turned them into 21 concepts,
  moving 56 namespaces. Modularity fell 0.65 → 0.60: reference cohesion
  traded for meaning. The lowest-cohesion concepts are the cross-cutting
  ones the graph cannot see as a unit (shared UI, approval, prompt context,
  logd, preview).
- Every implementation tried is reproducible for a fixed seed, but
  leiden-rs's partitions move with the seed: against other seeds, 12–36
  more namespaces fall outside their concept's source group.
- **Consequence: the concept file is what persists, not the clustering.**
  Re-clustering and redoing the agent pass would re-argue those namespaces
  on seed noise. Clustering is kept for placing namespaces that are new
  since the file was written and for flagging concepts whose cohesion drops.

The owner accepted the 21 concepts as the starting page set.

## The page pilot (2026-11-08)

Written by the session agent into
`$XDG_DATA_HOME/horizon/wiki/<sanitized-root>/` (the knowledge layer's
root naming): `index.md`, `sandbox.md`, `approval.md`, `terminal.md`. The
three concepts were picked for differing shapes: a cohesive one, a
cross-cutting low-cohesion one, and one split across a daemon, a library
and a pane.

Page format, accepted with the pilot:

- Frontmatter: `concept`, `title`, `code_at` (the commit the page was read
  against), `written`.
- A concept page: what it is (a few lines); entry points (where users and
  other concepts touch it); the flow inside, naming the main types and
  functions with their files; links to other concepts with the
  concept-graph weight; related records (docs, board items — open ones
  apart from past ones — and knowledge-layer entries, links only); stale
  text noticed while reading; and the page's namespaces as its anchors.
- The overview: the process split, the concepts grouped by where they run
  with one line each (unwritten pages marked), the strongest concept-graph
  edges, and the entry records (`AGENTS.md`, the roadmap, the UX
  principles). It says the graph shows references only, not messages
  between processes.
- Japanese, since the reader is the owner.
- Pages are read against the owner's local `main`, not a worker branch:
  the pilot's branch was seven commits behind, two of them in approval.

What the pilot showed:

- Concept-graph weights and docs/board candidates were mechanical (a join
  of the concept file with `namespace_graph`, a grep of `docs/` for the
  concept's paths, the board's folded items); the body still needed the
  code read, and three statements had to be corrected against it before
  they were right.
- Reading for a page surfaces stale text, which serves the "find where to
  work next" purpose directly: in the pilot, a module comment claiming the
  PTY threads live in the shell, the roadmap still calling logd "not
  started", and a knowledge-layer entry citing a function main removed.

## Next directions (owner, 2026-11-08)

The owner named three: updating the wiki as main changes, asking an agent
about any spot in the wiki by comment, and embedding diagrams such as
mermaid. No order among them ("特にどれから、というのはないです").
Updating stays manual for now, since automatic and manual updating share
almost all their parts ("まずは手動運用でシンプルな形でいいかなと感じています").
Diagrams are deferred; gpui-component (7591a35) has no mermaid rendering.

## Comments and the wiki view (decided 2026-11-08)

- **A view of its own.** A wiki view built from the Markdown view
  (`src/markdown/`), with comments as what makes it specific to reading
  the wiki ("明らかに、コメント機能だと思うんですが").
- **A comment targets a selected text range**, and answers come back as a
  thread. Whoever answers — the thread's session or an ordinary one — can
  edit and create pages ("スレッドとして帰ってくれば十分。ただしそこで
  動作するセッションでも普段のセッションでも、必要に応じてページの修正や
  作成は出来る作りであるべき").
- **Agents are woken the way the board wakes them, but the board is not
  the store** ("boardと同様のエージェント発火機構を用いていいという意味で、
  boardをそのまま活用するという意味ではないです"). Comment threads live in
  a log of their own beside the wiki pages.
- **One session per thread.** A thread's session is closed after it
  answers and resumed when the thread gets a new message. Nothing about
  the session is shown to the reader: the thread itself shows whether a
  question is still unanswered ("利用者から見える状態はそもそも必要ない
  んじゃないですかね"). Closing a thread's session is internal and must not
  reuse `Terminated`, which is the owner's explicit, destructive end.
  `resume_session` today re-reads the whole event log (~640 MB) per call;
  resuming per follow-up needs a projection-based path like startup
  resume's.
- **Thread sessions do not change the repository**, for now; usage may
  change that ("一旦、リポジトリを変えることはない前提でよいですが、
  使用感次第で変わりウルトは思います").
- **Pages are written through dedicated tools**, both a partial edit
  (exact-string replacement, as `fs.edit`) and a whole-page write for new
  or regenerated pages. The store sits outside every workspace, so
  without them every write needs an approval.
- **Anchors borrow the W3C Web Annotation Data Model** (Recommendation,
  2017-02-23), a subset of it, kept conformant to its MUSTs:
  - Annotation core (`@context` `http://www.w3.org/ns/anno.jsonld`, `id`,
    `type`, `motivation`, `body`, `target`, `creator`, `created`,
    `modified`); `TextualBody` (`text/markdown`, `ja`); `SpecificResource`
    with `TextQuoteSelector` (primary) and `TextPositionSelector`
    (disambiguation) as alternatives; `TimeState` with `sourceDate` and
    `cached` pointing at a snapshot of the page when the comment was made;
    agents as `Person` (owner) and `Software` (a session).
  - A reply is an Annotation with motivation `replying` whose target is
    the parent Annotation. The spec has no reply example; a target may be
    any resource.
  - Quotes are taken from the rendered text (markup removed, as the spec
    asks), positions counted in Unicode code points.
  - Every identifier is an IRI (the Annotation `id` MUST be one;
    relative references such as `sandbox.md` are not).
  - Not used: the HTTP protocol, collections and paging, JSON-LD
    processing. The data is read and written as plain JSON carrying the
    `@context` line.
  - Why this subset: offsets alone break on any edit; edit-tracked
    positions (Yjs-style) need every write to pass one model and lose a
    comment when its own range is replaced, which agent edits do often;
    quote plus context with fuzzy re-anchoring is what annotation systems
    over documents they do not own use (Hypothesis), and the comment-time
    snapshot keeps an "outdated" display possible (GitHub review style).
    Web Annotation's broad web-wide vision did not take hold (no browser
    shipped it, Apache Annotator retired from the Incubator 2025-08-11),
    but its selector model is in use where one party holds documents and
    annotations (IIIF Presentation 3.0, EPUB Annotations drafts).
  - Conformance can be checked with the WPT `annotation-model/` JSON
    Schemas (draft-04, BSD-3) under a JSON Schema validator in tests.
    Re-anchoring can port Hypothesis's `match-quote.ts` (BSD-2) and the
    approximate search it uses (`approx-string-match`, MIT); no Rust crate
    implements either.
- **The wiki is an extension package**: its store, annotation model,
  anchoring, tools, skill and the shell's wiki view are kept apart from
  Horizon's core, positioned as an extension ("horizonの基本機能という
  より拡張という立ち位置を明確にしたい"). Roles and skills already have
  external registration (`roles::register_external`,
  `skills::register_external_skill_sources`); tools do not, so this work
  includes a seam for registering tools from outside `horizon-agent`.
  Moving the board's and knowledge layer's tools onto that seam is handed
  to a separate session directly, not through the board.

### Still open before planning

- Generalize `board_flow` for wiki threads, or add a parallel flow of the
  same shape.
- Who writes the comment log: through `horizon-logd` as board writes do
  (the view and agents write concurrently), or otherwise.
- Package layout: crates, how the shell's wiki view is registered and
  opened, where the skill and `cluster-spike` move.
- Whether the text a view selection yields (gpui-component `TextView`'s
  `selected_text`) can be reproduced from the Markdown source on the
  agent side; the anchor design rests on it.
- The re-anchoring implementation (a port, or existing crates).
- The IRI forms for pages, snapshots, comments and sessions.
- Later: how orphaned and outdated comments look; what else the view
  shows; the slicing of the work.

## Open points

- **Regenerating pages: settled 2026-11-08.** The procedure is the
  repository skill `.horizon/skills/project-wiki/SKILL.md`, written so it
  does not depend on which agent runs it (owner: "スキルとしてはエージェント
  実装によらない形でいいんじゃないですか？"); `.claude/skills/project-wiki`
  links to the same file. Its mechanical steps are `cluster-spike place`
  (namespaces the concept file lacks, with the concept each is most tied
  to, and names the code no longer has) and `cluster-spike concept-graph`
  (directed weights between concepts). The concept file moved to the store
  as `concepts.md` beside the pages (owner's choice), so open point "Where
  the concept file lives" is closed. Still open: a Horizon agent needs an
  approval to write to the store, which sits outside every workspace; a
  dedicated write path like `knowledge.write` would remove that, and
  whether one is wanted is undecided. The full pass found four more stale
  module comments beyond the pilot's, fixed on the same branch.
- **Choosing links to docs and the board.** The pilot used path mentions
  and the agent's judgment together. 50 of 72 docs mention repository
  paths; 53 of their 153 distinct backticked paths no longer exist.
- **Keeping pages current.** Anchoring pages to their concept's namespaces
  makes "did the code under this page change" a git question, as in the
  knowledge layer's verification pass.
- **The production clustering.** A crate inside code-db (leiden-rs or
  graphrs's Louvain; graphrs gives the same partition for every seed,
  which suits a candidate generator) versus keeping it a separate tool.
  Two DuckDB community extensions offer Louvain (petgraph_ext, onager);
  duckpgq offers none.
- **code-db friction met on the way.** It is not built by default, and
  `code-db index` requires jscpd even when only references are wanted.
